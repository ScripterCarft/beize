//! Headless boots: watching the serial console and answering its questions
//! as the owner would.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

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
const SMOKE_CREDENTIALS: [&str; 2] = [
    "type=11,value=io.systemd.credential:beize.smoke=1",
    "type=11,value=io.systemd.credential:getty.auto=no",
];

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
    /// Every console line, without terminal control sequences.
    pub(super) console: Vec<String>,
}

/// systemd's prompt for a disk passphrase or recovery key ("Please enter
/// recovery key for disk root: ..."). It does not end with a newline, so it
/// is looked for in incomplete lines, too.
fn is_passphrase_prompt(text: &str) -> bool {
    text.contains("Please enter") && text.contains(" for disk ")
}

/// beize-tpm's question whether to adopt a different TPM (beize-tpm/src/main.rs).
const ADOPT_TPM_PROMPT: &str = "Type yes to bind the data to it";

/// The first boot waits for the owner to confirm the recovery key is saved
/// (system/initrd/usr/lib/beize/recovery-key); the tests answer "saved".
pub(super) const RECOVERY_KEY_SAVED_PROMPT: &str =
    "Type saved once the recovery key is written down";

/// The owner's account the tests create on first boot (beize-setup).
pub(super) const SMOKE_USER: &str = "smoke";

/// Typed as an answer: Ctrl+C alone, without Enter.
pub(super) const CTRL_C: &str = "\u{3}";

/// beize-setup's questions and the tests' answers, in the order typed ("Password:"
/// is not part of "Password again:", each matches only its own question). The
/// first password question gets Ctrl+C: the setup must start over instead of
/// ending, so the owner's name is asked twice. The time zone: a country with
/// several zones, so the setup asks back.
pub(super) const SETUP_ANSWERS: [(&str, &[&str]); 6] = [
    ("Your login name", &[SMOKE_USER, SMOKE_USER]),
    ("Your full name", &["Smoke Test", "Smoke Test"]),
    ("Password again:", &["smoke-test-password"]),
    ("Password:", &[CTRL_C, "smoke-test-password"]),
    ("Your time zone", &["Brazil"]),
    ("Which one?", &["Sao Paulo"]),
];

/// What beize-setup says after Ctrl+C.
pub(super) const SETUP_STARTS_OVER: &str = "Starting over.";

/// The zone the answers above must lead to.
pub(super) const SMOKE_TIME_ZONE: &str = "America/Sao_Paulo";

/// What to type at the console during a boot, as the owner would.
#[derive(Clone, Default)]
pub(super) struct Answers {
    /// Typed when the boot asks for the recovery key; without one, that
    /// question ends the boot.
    recovery_key: Option<String>,
    /// Typed when beize-tpm asks whether to adopt a new TPM ("no" if unset).
    adopt_tpm: Option<&'static str>,
}

impl Answers {
    pub(super) fn none() -> Self {
        Self::default()
    }

    pub(super) fn recovery_key(key: &str) -> Self {
        Self {
            recovery_key: Some(key.to_owned()),
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
    fn replies(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        let mut replies = vec![
            (ADOPT_TPM_PROMPT, vec![self.adopt_tpm.unwrap_or("no")]),
            (RECOVERY_KEY_SAVED_PROMPT, vec!["saved"]),
        ];
        replies.extend(
            SETUP_ANSWERS
                .iter()
                .map(|(prompt, typed)| (*prompt, typed.to_vec())),
        );
        replies
    }
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
        LAST_CONSOLE.set(Some(log_path));
        let started = Instant::now();
        let mut qemu = qemu(machine, &tpm)?;
        for credential in credentials {
            qemu.args(["-smbios", credential]);
        }
        let mut child = qemu
            .args(["-display", "none", "-serial", "stdio", "-monitor", "none"])
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
            thread::spawn(move || read_console(stdout, log, &stdin, &answers, &tx))
        };
        Ok(Self {
            child,
            stdin,
            events,
            reader: Some(reader),
            started,
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
        let seen = self
            .reader
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
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
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n @ 1..) = stdout.read(&mut chunk) {
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            line_answered = false;
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
        if !seen.prompted && is_passphrase_prompt(&shown) {
            seen.prompted = true;
            if let Some(key) = &answers.recovery_key {
                echo(&mut log, "(typing the recovery key)");
                answer(key);
            }
            let _ = events.send(ConsoleEvent::Prompted);
        }
        for ((prompt, typed), replied) in replies.iter().zip(&mut replied) {
            if line_answered || !shown.contains(prompt) {
                continue;
            }
            let Some(&reply) = typed.get(*replied) else {
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
        console: seen.console,
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

pub(super) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
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
        assert!(is_passphrase_prompt(
            "Please enter recovery key for disk root: (press TAB for no echo) "
        ));
        assert!(is_passphrase_prompt(
            "Please enter passphrase for disk root (root): "
        ));
        assert!(!is_passphrase_prompt(
            "Starting Cryptography Setup for root..."
        ));
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
