//! Headless boots: watching the serial console and answering its questions
//! as the owner would.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::machine::{Machine, Swtpm, qemu};
use crate::say;
use crate::util;

pub(super) const REPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// The firmware refuses a bad image within seconds; this only bounds a hang.
pub(super) const TAMPER_TIMEOUT: Duration = Duration::from_secs(45);

/// systemd credentials passed via SMBIOS (honoured in VMs, also under Secure
/// Boot): boot-report powers off when beize.smoke is present; getty.auto=no
/// keeps systemd from starting a getty on the serial console, which belongs
/// to the test – a getty resets the console when it starts and discards what
/// the report wrote.
pub(super) const SMOKE_CREDENTIALS: [&str; 2] = [
    "type=11,value=io.systemd.credential:beize.smoke=1",
    "type=11,value=io.systemd.credential:getty.auto=no",
];

/// The disk the boot report writes the journal of the boot to: empty (sparse)
/// at power-on, the guest finds it by its serial number
/// (/dev/disk/by-id/virtio-beize-journal). A disk, not a serial port: a
/// serial port costs a VM exit per byte, on the nested CI runner more than
/// the power-off allows for half a megabyte.
const JOURNAL_DISK_SIZE: u64 = 16 << 20;

/// What the guest wrote to the journal disk: everything before the first
/// zero byte, as the disk started out zeroed.
fn written(disk: &[u8]) -> &[u8] {
    let end = disk.iter().position(|&b| b == 0).unwrap_or(disk.len());
    &disk[..end]
}

/// Numbers the boot logs in the order of the boots.
static BOOTS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The console log of this thread's latest boot: shown when a check fails.
    pub(super) static LAST_CONSOLE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Outcome of a headless boot.
pub(super) struct BootRun<T> {
    /// First answer of the verdict function and the time it took to get there.
    pub(super) answer: Option<(T, Duration)>,
    /// Whether the guest shut down by itself (instead of being stopped).
    pub(super) powered_off: bool,
    /// Console lines reporting failed units, from the initrd and the system.
    pub(super) failures: Vec<String>,
    /// Whether the boot asked for a passphrase (the TPM did not unlock).
    pub(super) prompted: bool,
    /// How often it asked.
    pub(super) prompts: u32,
    /// Every console line, without terminal control sequences.
    pub(super) console: Vec<String>,
    /// The journal of the boot as `journalctl --output=json`, if it wrote one.
    pub(super) journal: Option<PathBuf>,
}

/// How many of systemd's prompts for a disk passphrase or recovery key
/// ("Please enter recovery key for disk root: ...") `text` holds. They do not
/// end with a newline, so they are looked for in incomplete lines, too.
fn passphrase_prompts(text: &str) -> usize {
    text.split("Please enter")
        .skip(1)
        .filter(|rest| rest.contains(" for disk "))
        .count()
}

/// beize-tpm's question whether to adopt a different TPM (beize-tpm/src/main.rs).
const ADOPT_TPM_PROMPT: &str = "Type yes to bind the data to it";

/// The first boot waits for the owner to confirm the recovery key is saved
/// (system/initrd/usr/lib/beize/recovery-key); the tests answer "stored".
pub(super) const RECOVERY_KEY_SAVED_PROMPT: &str =
    "Type stored once the recovery key is written down";

/// The owner's account the tests create on first boot (beize-setup).
pub(super) const SMOKE_USER: &str = "smoke";
pub(super) const SMOKE_PASSWORD: &str = "smoke-test-password";

/// Credentials that log the owner in on the serial console without a
/// password (as [`super::run::DEBUG_CONSOLE_CREDENTIALS`] do for root):
/// the tests act as the owner would at a terminal.
pub(super) const OWNER_CONSOLE_CREDENTIALS: [&str; 2] = [
    "type=11,value=io.systemd.credential:tty.serial.ttyS0.agetty.autologin=smoke",
    "type=11,value=io.systemd.credential:tty.serial.ttyS0.login.noauth=yes",
];

/// The password question of PAM, here asked by polkit's agent (run0).
const PASSWORD_PROMPT: &str = "Password:";
/// How long the owner takes to start typing a password.
pub(super) const PASSWORD_PAUSE: Duration = Duration::from_millis(500);

/// Typed as an answer: Ctrl+C alone, without Enter.
pub(super) const CTRL_C: &str = "\u{3}";

/// beize-setup's questions and the tests' answers, in the order typed ("Password:"
/// is not part of "Password again:", each matches only its own question). The
/// first password question gets Ctrl+C: the setup must start over instead of
/// ending, so the keyboard and the owner's name are asked twice – the
/// keyboard by name first, then by code. The time zone: a country with
/// several zones, so the setup asks back.
pub(super) const SETUP_ANSWERS: [(&str, &[&str]); 7] = [
    ("Your keyboard layout", &["German", "de(nodeadkeys)"]),
    ("Your login name", &[SMOKE_USER, SMOKE_USER]),
    ("Your full name", &["Smoke Test", "Smoke Test"]),
    ("Password again:", &[SMOKE_PASSWORD]),
    (PASSWORD_PROMPT, &[CTRL_C, SMOKE_PASSWORD]),
    ("Your time zone", &["Brazil"]),
    ("Which one?", &["Sao Paulo"]),
];

/// What beize-setup says after Ctrl+C.
pub(super) const SETUP_STARTS_OVER: &str = "Starting over.";

/// The zone the answers above must lead to.
pub(super) const SMOKE_TIME_ZONE: &str = "America/Sao_Paulo";
/// The console keymap the keyboard answers must lead to (systemd's
/// kbd-model-map: de(nodeadkeys)).
pub(super) const SMOKE_KEYMAP: &str = "de-latin1-nodeadkeys";

/// The emergency gate (os/beize-tpm/src/emergency.rs): its headline, its
/// menu, its question for the recovery key, what it says to a wrong key and
/// before the shell starts.
pub(super) const EMERGENCY_HEADLINE: &str = "Beize could not start.";
/// The same gate's headline in the recovery start (ADR 0018).
pub(super) const RECOVERY_HEADLINE: &str = "Beize recovery.";
const EMERGENCY_CHOICE: &str = "Choose:";
const EMERGENCY_KEY_PROMPT: &str = "Recovery key of this computer:";
pub(super) const EMERGENCY_WRONG_KEY: &str = "That is not the recovery key of this computer.";
pub(super) const EMERGENCY_SHELL_STARTS: &str = "Leaving the shell restarts the computer.";
/// A well-formed recovery key that opens nothing.
const NOT_THE_RECOVERY_KEY: &str =
    "cbdefghi-jklnrtuv-cbdefghi-jklnrtuv-cbdefghi-jklnrtuv-cbdefghi-jklnrtuv";

/// What to type at the console during a boot, as the owner would.
#[derive(Clone, Default)]
pub(super) struct Answers {
    /// Typed when the boot asks for the recovery key; without one, that
    /// question ends the boot.
    recovery_key: Option<String>,
    /// The recovery key typed on the machine's keyboard, as a German keyboard
    /// sends it (see [`Answers::recovery_key_on_german_keyboard`]).
    german_keyboard_key: Option<String>,
    /// Typed when beize-tpm asks whether to adopt a new TPM ("no" if unset).
    adopt_tpm: Option<&'static str>,
    /// The recovery key for the emergency gate, typed after a wrong key and
    /// Ctrl+C were refused; unset: the gate's questions stay unanswered.
    emergency_key: Option<String>,
    /// Typed, one after the other, when polkit asks for a password; set, the
    /// first-boot setup is over and its questions stay unanswered.
    passwords: Vec<&'static str>,
    /// The recovery start: the gate's menu gets "s" once; its own question
    /// for the key stays unanswered (it must take the initrd's).
    recovery: bool,
    /// Further questions and what is typed at them, one after the other
    /// (see [`Answers::answering`]).
    more: Vec<(&'static str, Vec<String>)>,
}

impl Answers {
    /// Also types `typed`, one after the other, where the console asks
    /// `prompt` – a question no other answer matches.
    pub(super) fn answering(mut self, prompt: &'static str, typed: &[&str]) -> Self {
        self.more
            .push((prompt, typed.iter().map(|t| (*t).to_owned()).collect()));
        self
    }

    pub(super) fn none() -> Self {
        Self::default()
    }

    pub(super) fn recovery_key(key: &str) -> Self {
        Self {
            recovery_key: Some(key.to_owned()),
            ..Self::default()
        }
    }

    /// The recovery key typed on the keyboard, not the serial console – key
    /// by key, as scan codes, so the console keymap decides what arrives
    /// (ADR 0017). The machine's keymap is German: first the key goes in with
    /// the dash where a US keyboard has it, which gives "ß" and must be
    /// refused; then with the dash key of a German keyboard.
    pub(super) fn recovery_key_on_german_keyboard(key: &str) -> Self {
        Self {
            recovery_key: Some(key.to_owned()),
            german_keyboard_key: Some(key.to_owned()),
            ..Self::default()
        }
    }

    /// At the emergency gate: a wrong key, then Ctrl+C at the menu, then
    /// the recovery key.
    pub(super) fn emergency_gate(key: &str) -> Self {
        Self {
            emergency_key: Some(key.to_owned()),
            ..Self::default()
        }
    }

    /// The recovery start (ADR 0018): the recovery key where the initrd asks
    /// for it, then the shell at the gate's menu.
    pub(super) fn recovery(key: &str) -> Self {
        Self {
            recovery_key: Some(key.to_owned()),
            recovery: true,
            ..Self::default()
        }
    }

    /// The passwords for polkit's questions (run0), in order.
    pub(super) fn passwords(passwords: &[&'static str]) -> Self {
        Self {
            passwords: passwords.to_vec(),
            ..Self::default()
        }
    }

    pub(super) fn adopt_tpm(self, answer: &'static str) -> Self {
        Self {
            adopt_tpm: Some(answer),
            ..self
        }
    }

    /// The questions answered besides the recovery key, as (prompt, what is
    /// typed the first time it is asked, the second time, …); asked more
    /// often, it stays unanswered.
    fn replies(&self) -> Vec<(&'static str, Vec<String>)> {
        let owned = |typed: &[&str]| typed.iter().map(|t| (*t).to_owned()).collect();
        let mut replies = vec![
            (ADOPT_TPM_PROMPT, owned(&[self.adopt_tpm.unwrap_or("no")])),
            (RECOVERY_KEY_SAVED_PROMPT, owned(&["stored"])),
        ];
        if self.passwords.is_empty() {
            replies.extend(
                SETUP_ANSWERS
                    .iter()
                    .map(|(prompt, typed)| (*prompt, owned(typed))),
            );
        } else {
            replies.push((PASSWORD_PROMPT, owned(&self.passwords)));
        }
        if let Some(key) = &self.emergency_key {
            replies.push((EMERGENCY_CHOICE, owned(&["s", CTRL_C, "s"])));
            replies.push((
                EMERGENCY_KEY_PROMPT,
                vec![NOT_THE_RECOVERY_KEY.to_owned(), key.clone()],
            ));
        }
        if self.recovery {
            replies.push((EMERGENCY_CHOICE, owned(&["s"])));
        }
        replies.extend(self.more.iter().cloned());
        replies
    }
}

/// Saves what the screen of a running machine shows to `file`: QEMU's
/// `screendump`, the image its display device scans out. PPM for a file
/// ending in `.ppm` (what the smoke test compares), else PNG.
pub(super) fn screendump(socket: &Path, file: &Path) -> Result<()> {
    let filename = serde_json::to_string(&file.display().to_string())?;
    let format = if file.extension().is_some_and(|e| e == "ppm") {
        "ppm"
    } else {
        "png"
    };
    Keyboard::connect(socket)?.execute(&format!(
        r#"{{"execute": "screendump", "arguments": {{"filename": {filename}, "format": "{format}"}}}}"#
    ))
}

/// Presses `keys` on a running machine (QEMU key names, `+` for together).
pub(super) fn keys(socket: &Path, keys: &[&str]) -> Result<()> {
    Keyboard::connect(socket)?.press(keys)
}

/// Moves the machine's mouse by `dx`, `dy` and, if `click`, clicks its left
/// button there.
pub(super) fn mouse(socket: &Path, dx: i32, dy: i32, click: bool) -> Result<()> {
    let mut qmp = Keyboard::connect(socket)?;
    let send = |events: String| {
        format!(r#"{{"execute": "input-send-event", "arguments": {{"events": [{events}]}}}}"#)
    };
    qmp.execute(&send(format!(
        r#"{{"type": "rel", "data": {{"axis": "x", "value": {dx}}}}}, {{"type": "rel", "data": {{"axis": "y", "value": {dy}}}}}"#
    )))?;
    if click {
        for down in [true, false] {
            thread::sleep(Duration::from_millis(40));
            qmp.execute(&send(format!(
                r#"{{"type": "btn", "data": {{"down": {down}, "button": "left"}}}}"#
            )))?;
        }
    }
    Ok(())
}

/// Drags with the left button of the machine's tablet (`Machine::tablet`)
/// from `from` to `to`, pixels of a screen of `screen`, in steps as a hand
/// would; without the button, only moves the pointer there.
pub(super) fn drag(
    socket: &Path,
    screen: (usize, usize),
    from: (i32, i32),
    to: (i32, i32),
    button: bool,
) -> Result<()> {
    const STEPS: i32 = 10;
    let mut tablet = Tablet::connect(socket, screen)?;
    tablet.to(from)?;
    if button {
        thread::sleep(Duration::from_millis(40));
        tablet.button(true)?;
    }
    for step in 1..=STEPS {
        thread::sleep(Duration::from_millis(20));
        let between = |a: i32, b: i32| a + (b - a) * step / STEPS;
        tablet.to((between(from.0, to.0), between(from.1, to.1)))?;
    }
    if button {
        thread::sleep(Duration::from_millis(40));
        tablet.button(false)?;
    }
    Ok(())
}

/// Clicks the tablet's left button `times` at `at`, 60 ms down and up each:
/// two are a double click (400 ms at most from press to press, GNOME's).
pub(super) fn click(
    socket: &Path,
    screen: (usize, usize),
    at: (i32, i32),
    times: u32,
) -> Result<()> {
    let mut tablet = Tablet::connect(socket, screen)?;
    tablet.to(at)?;
    for _ in 0..times {
        thread::sleep(Duration::from_millis(60));
        tablet.button(true)?;
        thread::sleep(Duration::from_millis(60));
        tablet.button(false)?;
    }
    Ok(())
}

/// The machine's tablet, through QEMU's control socket. It reports
/// 0..=0x7fff across the screen, which libinput scales by `screen /
/// 0x8000`: rounded up, a pixel lands in it, at most 1/40 px past its
/// edge (rounded to the nearest, 799 of 800 landed at 798.999 – on the
/// pixel before).
struct Tablet {
    qmp: Keyboard,
    screen: (usize, usize),
}

impl Tablet {
    fn connect(socket: &Path, screen: (usize, usize)) -> Result<Self> {
        Ok(Self {
            qmp: Keyboard::connect(socket)?,
            screen,
        })
    }

    fn send(&mut self, events: &str) -> Result<()> {
        self.qmp.execute(&format!(
            r#"{{"execute": "input-send-event", "arguments": {{"events": [{events}]}}}}"#
        ))
    }

    /// The pointer onto the pixel `(x, y)`.
    fn to(&mut self, (x, y): (i32, i32)) -> Result<()> {
        let scale = |n: i32, size: usize| (f64::from(n) * 32768.0 / size as f64).ceil() as i64;
        let events = format!(
            r#"{{"type": "abs", "data": {{"axis": "x", "value": {}}}}}, {{"type": "abs", "data": {{"axis": "y", "value": {}}}}}"#,
            scale(x, self.screen.0),
            scale(y, self.screen.1)
        );
        self.send(&events)
    }

    fn button(&mut self, down: bool) -> Result<()> {
        self.send(&format!(
            r#"{{"type": "btn", "data": {{"down": {down}, "button": "left"}}}}"#
        ))
    }
}

/// The keyboard (and mouse) of a running machine, through QEMU's control
/// socket (QMP).
struct Keyboard {
    commands: UnixStream,
    replies: BufReader<UnixStream>,
}

impl Keyboard {
    fn connect(socket: &Path) -> Result<Self> {
        let commands = UnixStream::connect(socket)
            .with_context(|| format!("connecting to {}", socket.display()))?;
        commands.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut keyboard = Self {
            replies: BufReader::new(commands.try_clone()?),
            commands,
        };
        keyboard.reply()?; // QEMU's greeting
        keyboard.execute(r#"{"execute": "qmp_capabilities"}"#)?;
        Ok(keyboard)
    }

    /// Presses the keys one after the other, by QEMU key name ("c", "minus");
    /// keys joined by `+` together ("ctrl+alt+f2").
    fn press(&mut self, keys: &[&str]) -> Result<()> {
        for key in keys {
            let together: Vec<String> = key
                .split('+')
                .map(|k| format!(r#"{{"type": "qcode", "data": "{k}"}}"#))
                .collect();
            self.execute(&format!(
                r#"{{"execute": "send-key", "arguments": {{"keys": [{}], "hold-time": 20}}}}"#,
                together.join(", ")
            ))?;
            // Released before the next one is pressed.
            thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    fn execute(&mut self, command: &str) -> Result<()> {
        writeln!(self.commands, "{command}")?;
        loop {
            let reply = self.reply()?;
            if reply.contains(r#""return""#) {
                return Ok(());
            }
            if reply.contains(r#""error""#) {
                bail!("QEMU refused {command}: {}", reply.trim());
            }
            // Anything else is an event; the reply follows.
        }
    }

    fn reply(&mut self) -> Result<String> {
        let mut reply = String::new();
        if self.replies.read_line(&mut reply)? == 0 {
            bail!("QEMU closed its control socket");
        }
        Ok(reply)
    }
}

/// The keys for a recovery key (modhex letters and dashes), then Enter;
/// `dash` is the key that gives "-". The modhex letters are where a US
/// keyboard has them on German and French keyboards, too.
fn recovery_key_presses(key: &str, dash: &'static str) -> Vec<&'static str> {
    key.chars()
        .filter_map(|c| match c {
            '-' => Some(dash),
            'a'..='z' => Some(LETTERS[c as usize - 'a' as usize]),
            _ => None,
        })
        .chain(["ret"])
        .collect()
}

/// QEMU's names of the letter keys, where a US keyboard has them.
const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

/// The keys that type `text` (lowercase letters, "-" and "/"), then Enter,
/// on the German keyboard the smoke test's owner chose (`SETUP_ANSWERS`):
/// Y and Z swapped, the dash on the key a US keyboard has the slash on,
/// the slash on Shift+7.
pub(super) fn german_presses(text: &str) -> Result<Vec<&'static str>> {
    text.chars()
        .map(|c| match c {
            '-' => Ok("slash"),
            '/' => Ok("shift+7"),
            'y' => Ok("z"),
            'z' => Ok("y"),
            'a'..='z' => Ok(LETTERS[c as usize - 'a' as usize]),
            _ => Err(anyhow!("{c:?} has no key here")),
        })
        .chain([Ok("ret")])
        .collect()
}

/// Types a line at the console.
fn type_line(stdin: &mut ChildStdin, text: &str) {
    let _ = write!(stdin, "{text}\r");
    let _ = stdin.flush();
}

/// What the reader thread passes on.
pub(super) enum ConsoleEvent {
    /// A complete console line (with terminal control sequences).
    Line(String),
    /// The boot asked for the recovery key (and got it, if one was given).
    Prompted,
}

/// What a boot's console showed, collected by the reader thread.
#[derive(Default)]
struct Seen {
    failures: Vec<String>,
    console: Vec<String>,
    prompted: bool,
    prompts: u32,
    /// Set by [`Session::finish`].
    journal: Option<PathBuf>,
}

/// A headless boot in progress: QEMU with the serial console on a pipe. A
/// reader thread logs every line, answers the console's questions as the
/// owner would and passes the lines on. Dropping it stops QEMU.
pub(super) struct Session {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    events: mpsc::Receiver<ConsoleEvent>,
    reader: Option<thread::JoinHandle<Seen>>,
    started: Instant,
    /// The journal disk (see [`JOURNAL_DISK_SIZE`]), later the journal.
    journal: PathBuf,
    /// Kept until QEMU is stopped.
    _tpm: Swtpm,
}

impl Session {
    /// Powers the machine on, passing `credential` via SMBIOS.
    pub(super) fn start(
        machine: &Machine,
        answers: &Answers,
        credentials: &[&str],
    ) -> Result<Self> {
        let tpm = Swtpm::start(&machine.tpm)?;
        fs::create_dir_all(&machine.logs)?;
        let log_path = machine.logs.join(format!(
            "{:02}-{}.log",
            BOOTS.fetch_add(1, Ordering::Relaxed) + 1,
            machine.name()
        ));
        let log = File::create(&log_path)?;
        let journal = log_path.with_extension("journal.json");
        File::create(&journal)?.set_len(JOURNAL_DISK_SIZE)?;
        LAST_CONSOLE.set(Some(log_path));
        // Left by a QEMU that was killed; QEMU would refuse to start.
        let _ = fs::remove_file(machine.qmp());
        let started = Instant::now();
        let mut qemu = qemu(machine, &tpm)?;
        for credential in credentials {
            qemu.args(["-smbios", credential]);
        }
        let mut child = qemu
            .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
            // On a slot of its own, like the stick: in the first free one it
            // came before the system's disk, and the firmware tried to boot
            // from it first on every start ("Boot0002 … Not Found").
            .arg("-drive")
            .arg(format!(
                "if=none,id=journal,format=raw,file={}",
                journal.display()
            ))
            .args([
                "-device",
                "virtio-blk-pci,drive=journal,serial=beize-journal,addr=0x13",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("starting qemu")?;
        let stdout = child.stdout.take().context("qemu stdout")?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().context("qemu stdin")?));
        let (tx, events) = mpsc::channel();
        let reader = {
            let stdin = Arc::clone(&stdin);
            let answers = answers.clone();
            let qmp = machine.qmp();
            thread::spawn(move || read_console(stdout, log, &stdin, &answers, &qmp, &tx))
        };
        Ok(Self {
            child,
            stdin,
            events,
            reader: Some(reader),
            started,
            journal,
            _tpm: tpm,
        })
    }

    /// The next console event; `None` once `deadline` passed or QEMU ended.
    pub(super) fn next(&self, deadline: Instant) -> Option<ConsoleEvent> {
        self.events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    }

    /// Types a line at the console.
    pub(super) fn type_line(&self, text: &str) {
        if let Ok(mut stdin) = self.stdin.lock() {
            type_line(&mut stdin, text);
        }
    }

    /// Gives the guest `grace` to power off by itself, then stops QEMU.
    /// Returns whether the guest powered off and what its console showed.
    fn finish(mut self, grace: Duration) -> Result<(bool, Seen)> {
        let deadline = Instant::now() + grace;
        while self.child.try_wait()?.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        let powered_off = self.child.try_wait()?.is_some();
        self.stop();
        let mut seen = self
            .reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        // Only a boot that reported wrote its journal; keep just that.
        let disk = fs::read(&self.journal).unwrap_or_default();
        let json = written(&disk);
        if json.is_empty() {
            let _ = fs::remove_file(&self.journal);
        } else {
            fs::write(&self.journal, json)?;
            seen.journal = Some(self.journal.clone());
        }
        Ok((powered_off, seen))
    }

    /// Gives the guest `grace` to power off, then stops QEMU; returns
    /// whether the guest powered off by itself.
    pub(super) fn power_off_within(self, grace: Duration) -> Result<bool> {
        Ok(self.finish(grace)?.0)
    }

    fn stop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Writes a console line to the boot's log (and, with `--verbose`, here).
fn echo(log: &mut File, line: &str) {
    let _ = writeln!(log, "{line}");
    if util::verbose() {
        say!("  | {line}");
    }
}

/// The reader thread: logs every console line, answers the console's
/// questions and passes the lines on until QEMU closes the console.
fn read_console(
    mut stdout: ChildStdout,
    mut log: File,
    stdin: &Mutex<ChildStdin>,
    answers: &Answers,
    qmp: &Path,
    events: &mpsc::Sender<ConsoleEvent>,
) -> Seen {
    let answer = |text: &str| {
        if let Ok(mut stdin) = stdin.lock() {
            type_line(&mut stdin, text);
        }
    };
    let mut seen = Seen::default();
    let replies = answers.replies();
    // How often each question was answered, and whether the unfinished line
    // (a question waiting for its answer) has been answered already.
    let mut replied = vec![0; replies.len()];
    let mut line_answered = false;
    // Passphrase prompts in the unfinished line: a prompt answered on the
    // keyboard ends without a newline on the serial console.
    let mut prompts_in_line = 0;
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n @ 1..) = stdout.read(&mut chunk) {
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            line_answered = false;
            prompts_in_line = 0;
            let raw: Vec<u8> = pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\r', '\n']);
            echo(&mut log, line);
            if is_unit_failure(line) {
                seen.failures.push(strip_ansi(line));
            }
            seen.console.push(strip_ansi(line));
            let _ = events.send(ConsoleEvent::Line(line.to_owned()));
        }
        let shown = strip_ansi(&String::from_utf8_lossy(&pending));
        let prompts = passphrase_prompts(&shown);
        if prompts > prompts_in_line {
            prompts_in_line = prompts;
            seen.prompts += 1;
            if let Some(key) = &answers.german_keyboard_key {
                let (dash, how) = match seen.prompts {
                    1 => ("minus", "with the dash of a US keyboard"),
                    2 => ("slash", "with the dash key of a German keyboard"),
                    _ => ("", ""),
                };
                if !dash.is_empty() {
                    // The prompt reaches the serial console before the
                    // agent on tty1 reads the keyboard; keys pressed at once
                    // could get lost (2026-10-12: the German dash attempt was
                    // refused once in ten runs). A person reads it first too.
                    thread::sleep(PASSWORD_PAUSE);
                    echo(
                        &mut log,
                        &format!("(typing the recovery key on the keyboard, {how})"),
                    );
                    let typed = Keyboard::connect(qmp)
                        .and_then(|mut keyboard| keyboard.press(&recovery_key_presses(key, dash)));
                    if let Err(error) = typed {
                        echo(
                            &mut log,
                            &format!("(typing on the keyboard failed: {error:#})"),
                        );
                    }
                }
            } else if seen.prompts == 1
                && let Some(key) = &answers.recovery_key
            {
                echo(&mut log, "(typing the recovery key)");
                answer(key);
            }
            if !seen.prompted {
                seen.prompted = true;
                let _ = events.send(ConsoleEvent::Prompted);
            }
        }
        for ((prompt, typed), replied) in replies.iter().zip(&mut replied) {
            if line_answered || !shown.contains(prompt) {
                continue;
            }
            let Some(reply) = typed.get(*replied) else {
                continue;
            };
            *replied += 1;
            line_answered = true;
            if reply == CTRL_C {
                echo(&mut log, &format!("(pressing Ctrl+C at \"{prompt}\")"));
                if let Ok(mut stdin) = stdin.lock() {
                    let _ = stdin.write_all(CTRL_C.as_bytes());
                    let _ = stdin.flush();
                }
            } else if *prompt == PASSWORD_PROMPT && !answers.passwords.is_empty() {
                // pkttyagent prints the question before it switches the
                // terminal to hidden input, which discards what was typed
                // already; a person never types that fast.
                thread::sleep(PASSWORD_PAUSE);
                echo(&mut log, &format!("(typing a password at \"{prompt}\")"));
                answer(reply);
            } else if *prompt == EMERGENCY_KEY_PROMPT {
                echo(&mut log, &format!("(typing a key at \"{prompt}\")"));
                answer(reply);
            } else if answers.more.iter().any(|(more, _)| more == prompt) {
                // Mostly passwords: hidden input begins only once the
                // question is shown, and a person reads it first.
                thread::sleep(PASSWORD_PAUSE);
                echo(&mut log, &format!("(typing at \"{prompt}\")"));
                answer(reply);
            } else {
                echo(&mut log, &format!("(answering \"{prompt}\": {reply})"));
                answer(reply);
            }
        }
    }
    seen
}

/// Boots headless and feeds every console line to `verdict` until it
/// answers or `timeout` passes, typing `answers` at the console's questions.
/// Without a recovery key, being asked for one ends the boot. QEMU is always
/// stopped before returning.
pub(super) fn watch_boot<T>(
    machine: &Machine,
    timeout: Duration,
    answers: &Answers,
    verdict: impl Fn(&str) -> Option<T>,
) -> Result<BootRun<T>> {
    let session = Session::start(machine, answers, &SMOKE_CREDENTIALS)?;
    let deadline = session.started + timeout;
    let mut answer = None;
    while let Some(event) = session.next(deadline) {
        match event {
            ConsoleEvent::Line(line) => {
                if let Some(found) = verdict(&line) {
                    answer = Some((found, session.started.elapsed()));
                    break;
                }
            }
            ConsoleEvent::Prompted if answers.recovery_key.is_none() => break,
            ConsoleEvent::Prompted => {}
        }
    }
    // A healthy guest powers off right after reporting; give it a moment.
    let grace = Duration::from_secs(if answer.is_some() { 10 } else { 0 });
    let (powered_off, seen) = session.finish(grace)?;
    Ok(BootRun {
        answer,
        powered_off,
        failures: seen.failures,
        prompted: seen.prompted,
        prompts: seen.prompts,
        console: seen.console,
        journal: seen.journal,
    })
}

/// A systemd recovery key: 64 characters from the modhex alphabet in eight
/// groups separated by dashes.
pub(super) fn find_recovery_key(console: &[String]) -> Option<String> {
    const MODHEX: &str = "cbdefghijklnrtuv";
    console
        .iter()
        .flat_map(|line| line.split_whitespace())
        .find(|word| {
            let groups: Vec<&str> = word.split('-').collect();
            groups.len() == 8
                && groups
                    .iter()
                    .all(|g| g.len() == 8 && g.chars().all(|c| MODHEX.contains(c)))
        })
        .map(str::to_owned)
}

/// systemd's console status lines for failed units and failed dependencies.
fn is_unit_failure(line: &str) -> bool {
    let line = strip_ansi(line);
    line.contains("[FAILED]") || line.contains("[DEPEND]")
}

/// The line without terminal control sequences: CSI (`ESC [` ... letter) and
/// OSC (`ESC ]` ... `ESC \` or BEL, e.g. run0's window title and context marks).
pub(super) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c == '\u{1b}' && chars.peek() == Some(&']') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_on_a_german_keyboard() {
        assert_eq!(
            german_presses("beize-yz").unwrap(),
            ["b", "e", "i", "y", "e", "slash", "z", "y", "ret"]
        );
        assert_eq!(german_presses("/t").unwrap(), ["shift+7", "t", "ret"]);
        assert!(german_presses("A").is_err());
    }

    #[test]
    fn journal_ends_where_the_empty_disk_begins() {
        assert_eq!(
            written(b"{\"MESSAGE\":\"a\"}\n\0\0\0"),
            b"{\"MESSAGE\":\"a\"}\n"
        );
        // A boot that did not report left the disk empty.
        assert_eq!(written(b"\0\0\0"), b"");
        assert_eq!(written(b""), b"");
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        let line =
            "\u{1b}]3008;end=ab\u{1b}\\BZ-DEBUG-rc0 \u{1b}[1;31mred\u{1b}[0m \u{1b}]2;title\u{7}x";
        assert_eq!(strip_ansi(line), "BZ-DEBUG-rc0 red x");
    }

    #[test]
    fn recognises_failed_units_on_the_console() {
        assert!(is_unit_failure(
            "[0;1;31mFAILED[0m] Failed to listen on Journal Log Access Socket."
                .replace("[0;1;31mFAILED", "[[0;1;31mFAILED")
                .as_str()
        ));
        assert!(is_unit_failure(
            "[DEPEND] Dependency failed for Kernel Core Pattern."
        ));
        assert!(!is_unit_failure("[  OK  ] Reached target Basic System."));
        assert_eq!(strip_ansi("[[0;32m  OK  [0m] x"), "[  OK  ] x");
        let prompt = "Please enter recovery key for disk root: (press TAB for no echo) ";
        assert_eq!(passphrase_prompts(prompt), 1);
        assert_eq!(
            passphrase_prompts("Please enter passphrase for disk root (root): "),
            1
        );
        // A prompt answered on the keyboard ends without a newline here.
        assert_eq!(passphrase_prompts(&format!("{prompt}{prompt}")), 2);
        assert_eq!(
            passphrase_prompts("Starting Cryptography Setup for root..."),
            0
        );
    }

    #[test]
    fn types_a_recovery_key_with_the_given_dash_key() {
        assert_eq!(
            recovery_key_presses("cb-du", "slash"),
            ["c", "b", "slash", "d", "u", "ret"]
        );
    }

    #[test]
    fn finds_the_recovery_key_on_the_console() {
        let key = "fjbdkvhn-nrttdbuj-ecjdhtnc-glklnkjl-crtfgehi-hdgjlbbr-ghlgucnk-lccdfuhb";
        let console = [
            "Recovery key for the data on this computer".to_owned(),
            format!("     {key}"),
        ];
        assert_eq!(find_recovery_key(&console).as_deref(), Some(key));
        // Other dash-separated words are not mistaken for a key.
        let noise = ["uuid 5024fb5b-d220-4ff2-9c32-385e3a1238fe".to_owned()];
        assert_eq!(find_recovery_key(&noise), None);
        assert_eq!(find_recovery_key(&[key.replace('f', "a")]), None);
    }
}
