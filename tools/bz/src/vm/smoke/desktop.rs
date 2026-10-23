//! The desktop (ADR 0027 §6): the owner signs in on tty1 at the keyboard
//! and types `beize-session`; the compositor runs as the owner in its
//! sandbox, logind knows the session as a Wayland one. The shell (ADR 0028)
//! runs in its own sandbox, the compositor knows it, and its menu bar lies
//! along the top with the time. A terminal opens with the desktop and
//! another with Ctrl+Alt+T. A command typed into it shows the terminal's
//! sample as the reference in the repository has it. A client's window
//! fills the screen below the menu bar, shows exactly its pattern and is
//! offered no panels. Beize UI's test program shows its sample – text,
//! colours, blending – exactly as it draws it offscreen, the reference in
//! the repository. Killed, the shell comes back and the window stays.
//! Signing out stops the desktop with its shell and terminals and takes
//! its environment back.
//!
//! The owner's shell on the serial console looks on: the same user, so the
//! same service manager as the login on tty1.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::Installed;
use crate::paths::Paths;
use crate::say;
use crate::vm::console::{
    Answers, OWNER_CONSOLE_CREDENTIALS, SMOKE_PASSWORD, SMOKE_USER, german_presses, keys,
    screendump,
};
use crate::vm::debug::Shell;
use crate::vm::machine::{Machine, test_medium};

/// The test client's quadrants as the screen shows them (RGB), from
/// os/beize-testclient: red, green above; blue, white below.
const QUADRANTS: [[u8; 3]; 4] = [[208, 48, 48], [48, 176, 64], [48, 80, 208], [255, 255, 255]];

/// How far a channel may be off: the pattern goes through unchanged
/// (XRGB8888 to the screen), so only rounding.
const CHANNEL_TOLERANCE: u8 = 2;

/// The pointer: drawn into the image in the VM (ADR 0027 §8), 24×24 at
/// most, in the screen's top left corner – the compositor starts it at
/// (0, 0) and the test never moves it. That is on the menu bar: windows
/// below it are compared without exception, down to one changed character.
const POINTER_SIZE: usize = 24;

/// The menu bar along the top of the screen (os/beize-shell, `HEIGHT` and
/// `BAR`); windows fill the screen below it.
const MENU_BAR: usize = 24;
const MENU_BAR_COLOR: [u8; 3] = [0xf5, 0xf5, 0xf5];
/// Where the time is: the right end of the bar, this wide. "00:00" in
/// Inter at 13 px is some 35 px; the rest of the bar is its colour alone.
const CLOCK_WIDTH: usize = 80;
/// The time's least ink, in dark pixels: well below the narrowest time,
/// "11:11", and well above none.
const CLOCK_INK: usize = 20;

/// The compositor's warning that Smithay could not become DRM master
/// itself: it already is, through the device logind handed over (reported
/// to Smithay, ~/upstream/smithay/ISSUES.md). Every other warning fails.
const KNOWN_WARNING: &str = "Unable to become drm master, assuming unprivileged mode";

/// Beize UI's sample as its test program draws it offscreen at the size of
/// a window in the VM (`beize-ui-test --offscreen FILE 1280x776`: the screen
/// below the menu bar), relative to the repository. Replaced only by hand,
/// after looking at the screen the test leaves in the logs when they
/// differ (ADR 0027 §9).
const UI_SAMPLE: &str = "os/beize-ui/tests/sample.png";

/// The terminal's window once it has run its sample
/// (`os/beize-terminal/tests/sample.sh`), as a run of this test showed it
/// (openQA's needles): replaced only by hand, after looking at it.
const TERMINAL_SAMPLE: &str = "os/beize-terminal/tests/sample.png";

/// What the sample is called on the test medium, typed into the terminal.
const TERMINAL_SAMPLE_COMMAND: &str = "/run/t/beize-terminal-sample";

/// How often the compositor has told the shell's connection apart
/// (os/beize-compositor, `insert_client`), in this boot.
const SHELL_CONNECTED: &str = "journalctl --user -b -u beize-compositor.service -o cat \
                               | grep -c '^the shell connected$'";

/// How long a screen may take to become what its reference shows.
const SCREEN_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn desktop(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-desktop")?;
    machine.test_medium = test_medium(paths)?;
    ensure!(
        machine.test_medium.is_some(),
        "no test medium: the test client is missing"
    );
    // run0 asks for the owner's password once, to mount the test medium.
    let shell = Shell::boot(
        &machine,
        &Answers::passwords(&[SMOKE_PASSWORD]),
        &OWNER_CONSOLE_CREDENTIALS,
        false,
    )?;
    let qmp = machine.qmp();

    // At the keyboard, as the owner would: name, password, beize-session,
    // each when the console has had time to ask.
    for (typed, pause) in [(SMOKE_USER, 1), (SMOKE_PASSWORD, 2), ("beize-session", 0)] {
        keys(&qmp, &german_presses(typed)?)?;
        thread::sleep(Duration::from_secs(pause));
    }
    let (_, lines) = shell.capture(
        "for i in $(seq 100); do systemctl --user -q is-active beize-session.target && break; \
         sleep 0.2; done; systemctl --user is-active beize-session.target",
    )?;
    ensure!(
        lines.iter().any(|l| l == "active"),
        "the desktop did not start after the login on tty1: {lines:?}"
    );

    // The session: the login on tty1, active, of type wayland (libseat
    // passed beize-session's XDG_SESSION_TYPE on to logind).
    let (_, lines) = shell.capture(
        "s=$(systemctl --user show-environment | sed -n 's/^XDG_SESSION_ID=//p'); \
         loginctl show-session \"$s\" -p TTY -p Active -p Type -p Name",
    )?;
    for wanted in ["TTY=tty1", "Active=yes", "Type=wayland", "Name=smoke"] {
        ensure!(
            lines.iter().any(|l| l == wanted),
            "the desktop's session is not {wanted}: {lines:?}"
        );
    }
    // The compositor: the owner's process, in its system call filter.
    let (_, lines) = shell.capture(
        "p=$(systemctl --user show -P MainPID beize-compositor.service); \
         echo BEIZE-OWNER-$(stat -c %U /proc/$p); \
         while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
         done < /proc/$p/status",
    )?;
    ensure!(
        lines
            .iter()
            .any(|l| l == &format!("BEIZE-OWNER-{SMOKE_USER}"))
            && lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
        "the compositor does not run as the owner in its system call filter: {lines:?}"
    );
    say!(
        "  ok: the login on tty1 started the desktop: a Wayland session, the compositor as the owner, sandboxed"
    );
    let (status, lines) = shell.capture(
        "run0 sh -c 'mkdir -p /run/t && mount -o ro /dev/disk/by-id/virtio-beize-test /run/t'",
    )?;
    ensure!(
        status == 0,
        "could not mount the test medium (exit status {status}): {lines:?}"
    );

    // The shell: its own process in its system call filter, which the
    // compositor knows as the shell by its unit; its menu bar on the screen.
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 50); do [ \"$({SHELL_CONNECTED})\" -ge 1 ] && break; sleep 0.2; done; \
         echo BEIZE-SHELL-$({SHELL_CONNECTED}); \
         p=$(systemctl --user show -P MainPID beize-shell.service); \
         while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
         done < /proc/$p/status"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-SHELL-1") && lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
        "the shell did not connect once as the shell, in its system call filter: {lines:?}"
    );
    wait_for_menu_bar(&machine)?;
    say!("  ok: the shell runs sandboxed, the compositor knows it, its menu bar shows the time");

    // The desktop starts with a terminal, Ctrl+Alt+T opens another: each in
    // a unit of its own, with the owner's shell in it.
    let first = wait_for_terminals(&shell, 1)?;
    keys(&qmp, &["ctrl+alt+t"])?;
    let both = wait_for_terminals(&shell, 2)?;
    ensure!(
        both.contains(&first[0]),
        "the first terminal ended when the second opened: {both:?}"
    );
    say!(
        "  ok: the desktop started with a terminal, Ctrl+Alt+T opened another, each with its shell"
    );

    // The new terminal has the keyboard: typed into once its shell reads,
    // the sample's command runs and the screen shows what it printed.
    let second = both
        .iter()
        .find(|u| **u != first[0])
        .context("no second terminal")?;
    wait_for_prompt(&shell, second)?;
    keys(&qmp, &german_presses(TERMINAL_SAMPLE_COMMAND)?)?;
    wait_for_screen(paths, &machine, TERMINAL_SAMPLE, "terminal")?;
    say!(
        "  ok: the typed command ran in the terminal, its window shows the sample as the \
         reference has it"
    );

    // A client started by the service manager, which knows the socket only
    // from beize-session.
    let (status, lines) = shell.capture(
        "systemd-run --user --unit=beize-testclient /run/t/beize-testclient --seconds 60",
    )?;
    ensure!(
        status == 0,
        "could not start the test client (exit status {status}): {lines:?}"
    );
    let (_, lines) = shell.capture(
        "for i in $(seq 50); do journalctl --user -u beize-testclient -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; journalctl --user -u beize-testclient -o cat",
    )?;
    let size = lines
        .iter()
        .find_map(|l| l.strip_prefix("configure "))
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?)))
        .with_context(|| format!("the client got no window: {lines:?}"))?;
    // An app is not offered the shell's panels (the list is real: it has
    // xdg-shell).
    let globals = lines
        .iter()
        .find_map(|l| l.strip_prefix("globals "))
        .with_context(|| format!("the client printed no globals: {lines:?}"))?;
    let offered: Vec<&str> = globals.split(' ').collect();
    ensure!(
        offered.contains(&"xdg_wm_base") && !offered.contains(&"zwlr_layer_shell_v1"),
        "an app was offered the shell's panels, or no windows: {globals}"
    );
    // Its first frame reaches the screen with the next refresh.
    thread::sleep(Duration::from_secs(1));
    let file = machine.logs.join("desktop-screen.ppm");
    screendump(&qmp, &file)?;
    let window = Image::read_ppm(&fs::read(&file)?)?.below(MENU_BAR);
    ensure!(
        (window.width, window.height) == size,
        "the window is {}x{}, the screen below the menu bar {}x{}: it does not fill it",
        size.0,
        size.1,
        window.width,
        window.height
    );
    compare(
        &window,
        |x, y| expected(x, y, size.0, size.1),
        "the client's pattern",
        &machine.logs.join("desktop-difference.ppm"),
    )?;
    say!(
        "  ok: the client's window fills the screen below the menu bar, {}x{}, exactly its \
         pattern; it is offered no panels",
        size.0,
        size.1
    );

    // Frames in a row: a second client draws 120, each on "frame done";
    // once they stop, the compositor sums up how they met their vblanks.
    // Its summary is the one after those of earlier frames (the terminal's).
    let (status, lines) = shell.capture(
        "p() { journalctl --user -b -u beize-compositor.service -o cat | grep '^pacing: '; }; \
         n=$(p | wc -l); systemd-run --user --unit=beize-testclient-frames \
         /run/t/beize-testclient --frames 120 --seconds 20 && for i in $(seq 100); do \
         [ \"$(p | wc -l)\" -gt \"$n\" ] && break; sleep 0.2; done; p | tail -n +$((n + 1))",
    )?;
    let pacing = lines
        .iter()
        .find(|l| l.starts_with("pacing: "))
        .with_context(|| format!("no pacing summary (exit status {status}): {lines:?}"))?;
    let frames = pacing
        .strip_prefix("pacing: ")
        .and_then(|s| s.split_once(' '))
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("a pacing summary without frames: {pacing}"))?;
    ensure!(
        frames >= 100,
        "the compositor counted {frames} frames of the client's 120: {pacing}"
    );
    say!("  ok: the compositor measured the client's frames: {pacing}");

    // Beize UI: its window through Vulkan's WSI shows the sample as the
    // reference has it – the same renderer, glyphs and blending offscreen.
    let (_, lines) = shell.capture(
        "systemctl --user stop beize-testclient \
         && systemd-run --user --unit=beize-ui-test /run/t/beize-ui-test --seconds 60 \
         && for i in $(seq 50); do journalctl --user -u beize-ui-test -o cat \
         | grep -q '^configure ' && break; sleep 0.2; done; \
         journalctl --user -u beize-ui-test -o cat",
    )?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure ")),
        "Beize UI's test program got no window: {lines:?}"
    );
    wait_for_screen(paths, &machine, UI_SAMPLE, "beize-ui")?;
    say!("  ok: Beize UI's window shows its sample as the reference has it");

    // Killed, the shell starts again (ADR 0028 §1): the compositor knows
    // it once more, the bar comes back, the desktop and the window stay.
    let (_, lines) = shell.capture(&format!(
        "systemctl --user kill -s KILL beize-shell.service; \
         for i in $(seq 50); do [ \"$({SHELL_CONNECTED})\" -ge 2 ] && break; sleep 0.2; done; \
         echo BEIZE-SHELL-$({SHELL_CONNECTED}); \
         systemctl --user is-active beize-shell.service beize-compositor.service \
         beize-session.target"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-SHELL-2")
            && lines.iter().filter(|l| *l == "active").count() == 3,
        "the shell did not come back after it was killed, or the desktop went: {lines:?}"
    );
    wait_for_menu_bar(&machine)?;
    wait_for_screen(paths, &machine, UI_SAMPLE, "beize-ui-after-shell")?;
    say!("  ok: killed, the shell came back with its menu bar; the desktop and the window stayed");

    let (_, lines) =
        shell.capture("journalctl --user -b -u beize-compositor.service -p warning -o cat")?;
    let unexpected: Vec<&String> = lines
        .iter()
        .filter(|l| !l.is_empty() && l.as_str() != KNOWN_WARNING)
        .collect();
    ensure!(
        unexpected.is_empty(),
        "the compositor warned: {unexpected:?}"
    );
    say!("  ok: no warnings from the compositor (but Smithay's known one)");

    // Signing out: logind ends the session, beize-session stops the desktop
    // and, last of all, takes the environment back: waited for that, the
    // rest is done too.
    let (_, lines) = shell.capture(
        "s=$(systemctl --user show-environment | sed -n 's/^XDG_SESSION_ID=//p'); \
         loginctl terminate-session \"$s\"; \
         for i in $(seq 50); do systemctl --user show-environment | grep -q '^XDG_SESSION_ID=' \
         || break; sleep 0.2; done; \
         systemctl --user is-active beize-session.target beize-compositor.service \
         beize-shell.service graphical-session.target; systemctl --user show-environment",
    )?;
    ensure!(
        lines.iter().filter(|l| *l == "inactive").count() == 4,
        "signing out did not stop the desktop: {lines:?}"
    );
    ensure!(
        !lines
            .iter()
            .any(|l| l.starts_with("WAYLAND_DISPLAY=") || l.starts_with("XDG_SESSION_ID=")),
        "the session's environment stayed with the service manager: {lines:?}"
    );
    let left = terminals(&shell)?;
    ensure!(
        left.is_empty(),
        "terminals still run after signing out: {left:?}"
    );
    say!(
        "  ok: signing out stopped the desktop, its shell and terminals and took its environment \
         back"
    );
    shell.stop()?;
    machine.remove()
}

/// The terminals in the owner's service manager, units of the template
/// `app-beize\x2dterminal@.service`, with the number of processes in each
/// (the terminal, and its shell once it has a window).
fn terminals(shell: &Shell) -> Result<Vec<(String, usize)>> {
    let (_, lines) = shell.capture(
        "systemctl --user list-units 'app-beize\\x2dterminal@*' --state=active --plain \
         --no-legend | while read -r u _; do \
         echo \"BEIZE-TERMINAL $u $(grep -c . /sys/fs/cgroup$(systemctl --user show -P \
         ControlGroup \"$u\")/cgroup.procs)\"; done",
    )?;
    Ok(lines
        .iter()
        .filter_map(|l| {
            let (unit, count) = l.strip_prefix("BEIZE-TERMINAL ")?.split_once(' ')?;
            Some((unit.to_owned(), count.parse().ok()?))
        })
        .collect())
}

/// Waits until `count` terminals run, each with its shell; their units.
fn wait_for_terminals(shell: &Shell, count: usize) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for _ in 0..50 {
        found = terminals(shell)?;
        if found.len() == count && found.iter().all(|(_, processes)| *processes >= 2) {
            return Ok(found.into_iter().map(|(unit, _)| unit).collect());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("not {count} terminals with a shell each: {found:?}")
}

/// Waits until the shell in the terminal `unit` reads a line: readline
/// takes the terminal out of canonical mode for that. Keys pressed before
/// it would be echoed by the terminal driver ahead of the prompt.
fn wait_for_prompt(shell: &Shell, unit: &str) -> Result<()> {
    let (_, lines) = shell.capture(&format!(
        "d=/sys/fs/cgroup$(systemctl --user show -P ControlGroup '{unit}'); r=; \
         for i in $(seq 50); do for p in $(cat \"$d/cgroup.procs\"); do \
         [ \"$(cat /proc/$p/comm)\" != beize-terminal ] && stty -F /proc/$p/fd/0 -a 2>/dev/null \
         | grep -qw -- -icanon && r=1; done; [ -n \"$r\" ] && break; sleep 0.2; done; \
         echo BEIZE-PROMPT-${{r:-none}}"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-PROMPT-1"),
        "the shell in {unit} never waited for a line: {lines:?}"
    );
    Ok(())
}

/// Waits until the screen below the menu bar – the window – shows exactly
/// what `reference` (relative to the repository) has, as openQA waits for a
/// needle. If it does not, the last window is `<name>-screen.png` in the
/// logs, to look at and to take over if the new look is wanted.
fn wait_for_screen(paths: &Paths, machine: &Machine, reference: &str, name: &str) -> Result<()> {
    let wanted = match fs::read(paths.root.join(reference)) {
        Ok(data) => Some(Image::read_png(&data).with_context(|| format!("reading {reference}"))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {reference}")),
    };
    let file = machine.logs.join(format!("{name}-screen.ppm"));
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    let failed = loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?.below(MENU_BAR);
        let compared = match &wanted {
            None => Err(anyhow!("there is no {reference} yet")),
            Some(r) if (r.width, r.height) != (screen.width, screen.height) => Err(anyhow!(
                "the window is {}x{}, {reference} {}x{}",
                screen.width,
                screen.height,
                r.width,
                r.height
            )),
            Some(r) => compare(&screen, |x, y| r.pixel(x, y), reference, &difference),
        };
        match compared {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => break (screen, e),
            Err(_) => {}
        }
    };
    let (screen, e) = failed;
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.write_png(&shown)?;
    Err(e.context(format!("the window is {}", shown.display())))
}

/// Waits until the menu bar lies along the top of the screen with the time
/// on its right; if it does not, the last screen is in the logs.
fn wait_for_menu_bar(machine: &Machine) -> Result<()> {
    let file = machine.logs.join("menu-bar-screen.ppm");
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        match menu_bar(&screen) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                let shown = machine.logs.join("menu-bar-screen.png");
                screen.write_png(&shown)?;
                return Err(e.context(format!("the screen is {}", shown.display())));
            }
            Err(_) => {}
        }
    }
}

/// Whether `screen` has the menu bar: its colour across the top but the
/// pointer and the time, and the time's ink at the right end. Which time
/// it shows depends on the minute; Beize UI's text itself is checked
/// exactly with the windows.
fn menu_bar(screen: &Image) -> Result<()> {
    ensure!(
        screen.width > CLOCK_WIDTH + POINTER_SIZE && screen.height > MENU_BAR,
        "a screen of {}x{} has no room for the menu bar",
        screen.width,
        screen.height
    );
    let near = |a: [u8; 3], b: [u8; 3]| {
        a.iter()
            .zip(b)
            .all(|(a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE)
    };
    let clock = screen.width - CLOCK_WIDTH;
    let mut other = 0;
    let mut ink = 0;
    for y in 0..MENU_BAR {
        for x in 0..screen.width {
            let shown = screen.pixel(x, y);
            if x < POINTER_SIZE && y < POINTER_SIZE {
                continue;
            }
            if x >= clock {
                ink += usize::from(shown.iter().all(|&c| c < 0x80));
            } else if !near(shown, MENU_BAR_COLOR) {
                other += 1;
            }
        }
    }
    ensure!(
        other == 0,
        "{other} pixels of the menu bar's strip are not its colour"
    );
    ensure!(
        ink >= CLOCK_INK,
        "the menu bar shows no time: {ink} dark pixels at its right end"
    );
    // Below it, something else: a window, not more bar.
    let below = (POINTER_SIZE..clock).all(|x| near(screen.pixel(x, MENU_BAR), MENU_BAR_COLOR));
    ensure!(!below, "the menu bar is higher than {MENU_BAR} pixels");
    Ok(())
}

/// An RGB image.
struct Image {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Image {
    /// A binary PPM (P6) with 8 bits a channel, as QEMU writes it.
    fn read_ppm(data: &[u8]) -> Result<Self> {
        let mut fields = Vec::new();
        let mut at = 0;
        while fields.len() < 4 {
            while data.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if data.get(at) == Some(&b'#') {
                while data.get(at).is_some_and(|&b| b != b'\n') {
                    at += 1;
                }
                continue;
            }
            let start = at;
            while data.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
                at += 1;
            }
            ensure!(at > start, "a PPM header cut short");
            fields.push(std::str::from_utf8(&data[start..at])?);
        }
        ensure!(fields[0] == "P6", "not a binary PPM: {}", fields[0]);
        ensure!(fields[3] == "255", "not 8 bits a channel: {}", fields[3]);
        let (width, height): (usize, usize) = (fields[1].parse()?, fields[2].parse()?);
        // One whitespace byte ends the header.
        let pixels = data.get(at + 1..).unwrap_or_default().to_vec();
        ensure!(
            pixels.len() == width * height * 3,
            "a {width}x{height} PPM with {} bytes of pixels",
            pixels.len()
        );
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn write_ppm(&self, file: &Path) -> Result<()> {
        let mut data = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        data.extend_from_slice(&self.pixels);
        Ok(fs::write(file, data)?)
    }

    /// A PNG with 8-bit RGB, as the references in the repository are.
    fn read_png(data: &[u8]) -> Result<Self> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info()?;
        let mut pixels = vec![0; reader.output_buffer_size().context("a PNG too large")?];
        let info = reader.next_frame(&mut pixels)?;
        ensure!(
            info.color_type == png::ColorType::Rgb && info.bit_depth == png::BitDepth::Eight,
            "not an 8-bit RGB PNG: {:?}, {:?}",
            info.color_type,
            info.bit_depth
        );
        pixels.truncate(info.buffer_size());
        Ok(Self {
            width: info.width as usize,
            height: info.height as usize,
            pixels,
        })
    }

    fn write_png(&self, file: &Path) -> Result<()> {
        let out = std::io::BufWriter::new(fs::File::create(file)?);
        let mut encoder = png::Encoder::new(out, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&self.pixels)?;
        Ok(())
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * 3;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    /// The image below its first `top` rows (the screen below the menu bar).
    fn below(self, top: usize) -> Self {
        let top = top.min(self.height);
        Self {
            width: self.width,
            height: self.height - top,
            pixels: self.pixels[top * self.width * 3..].to_vec(),
        }
    }
}

/// The client's pattern at (x, y) of a window `width`×`height`, as the
/// client draws it (`x < w / 2` is left).
fn expected(x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
    QUADRANTS[usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)]
}

/// Whether `window` shows exactly what is `wanted` (named `what` in
/// errors), but rounding; if not, `difference` shows the pixels that
/// differ in magenta over a dimmed window.
fn compare(
    window: &Image,
    wanted: impl Fn(usize, usize) -> [u8; 3],
    what: &str,
    difference: &Path,
) -> Result<()> {
    let mut marked = Image {
        width: window.width,
        height: window.height,
        pixels: window.pixels.iter().map(|c| c / 3).collect(),
    };
    let mut differing = 0;
    for y in 0..window.height {
        for x in 0..window.width {
            let at = (y * window.width + x) * 3;
            let shown = &window.pixels[at..at + 3];
            if shown
                .iter()
                .zip(wanted(x, y))
                .any(|(s, w)| s.abs_diff(w) > CHANNEL_TOLERANCE)
            {
                differing += 1;
                marked.pixels[at..at + 3].copy_from_slice(&[255, 0, 255]);
            }
        }
    }
    if differing > 0 {
        marked.write_ppm(difference)?;
        bail!(
            "{differing} pixels of the window differ from {what}; they are magenta in {}",
            difference.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: usize, height: usize) -> Image {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&expected(x, y, width, height));
            }
        }
        Image {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn reads_what_it_writes_and_what_qemu_writes() {
        let file = std::env::temp_dir().join(format!("bz-ppm-{}.ppm", std::process::id()));
        let image = pattern(4, 2);
        image.write_ppm(&file).unwrap();
        let read = Image::read_ppm(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            (read.width, read.height, &read.pixels),
            (4, 2, &image.pixels)
        );
        fs::remove_file(&file).unwrap();
        let mut qemu = b"P6\n# a comment\n2 1 255\n".to_vec();
        qemu.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(Image::read_ppm(&qemu).unwrap().pixels, [1, 2, 3, 4, 5, 6]);
        assert!(Image::read_ppm(b"P6\n2 1 255\n\0\0").is_err());
        assert!(Image::read_ppm(b"P3\n1 1 255\n0 0 0").is_err());
    }

    #[test]
    fn the_pattern_passes_exactly_and_fails_otherwise() {
        let compare = |screen: &Image, difference: &Path| {
            compare(
                screen,
                |x, y| expected(x, y, 64, 40),
                "the pattern",
                difference,
            )
        };
        let difference = std::env::temp_dir().join(format!("bz-diff-{}.ppm", std::process::id()));
        let mut screen = pattern(64, 40);
        compare(&screen, &difference).unwrap();
        // Rounding.
        screen.pixels[0] += CHANNEL_TOLERANCE;
        compare(&screen, &difference).unwrap();
        assert!(!difference.exists());
        // One pixel in the corner: a character that changed.
        screen.pixels[3] = 0;
        assert!(compare(&screen, &difference).is_err());
        assert!(difference.exists());
        fs::remove_file(&difference).unwrap();
        // A black screen, and the quadrants swapped.
        let black = Image {
            pixels: vec![0; 64 * 40 * 3],
            ..pattern(64, 40)
        };
        assert!(compare(&black, &difference).is_err());
        assert!(difference.exists());
        let mut swapped = pattern(64, 40);
        swapped.pixels.reverse();
        assert!(compare(&swapped, &difference).is_err());
        fs::remove_file(&difference).unwrap();
    }

    /// A screen of `width`×40 with the menu bar, ink for a time at its
    /// right end, a dark window below it and a pointer.
    fn desktop(width: usize) -> Image {
        let mut screen = Image {
            width,
            height: 40,
            pixels: [0x1e; 3].repeat(width * 40),
        };
        let mut set = |x: usize, y: usize, c: [u8; 3]| {
            let at = (y * width + x) * 3;
            screen.pixels[at..at + 3].copy_from_slice(&c);
        };
        for y in 0..MENU_BAR {
            for x in 0..width {
                set(x, y, MENU_BAR_COLOR);
            }
        }
        for y in 8..16 {
            for x in width - 40..width - 36 {
                set(x, y, [0x1d; 3]);
            }
        }
        for y in 0..POINTER_SIZE {
            for x in 0..POINTER_SIZE / 2 {
                set(x, y, [0, 0, 0]);
            }
        }
        screen
    }

    #[test]
    fn finds_the_menu_bar_with_its_time_and_nothing_else() {
        let screen = desktop(200);
        menu_bar(&screen).unwrap();
        // The window below it is the rest of the screen.
        let window = desktop(200).below(MENU_BAR);
        assert_eq!((window.width, window.height), (200, 40 - MENU_BAR));
        assert_eq!(window.pixel(0, 0), [0x1e; 3]);
        // A stray pixel on the bar, no time, a bar too high, no bar at all.
        let mut stray = desktop(200);
        stray.pixels[(5 * 200 + 100) * 3] = 0;
        assert!(menu_bar(&stray).is_err());
        let mut no_time = desktop(200);
        for x in 160..200 {
            for y in 0..MENU_BAR {
                let at = (y * 200 + x) * 3;
                no_time.pixels[at..at + 3].copy_from_slice(&MENU_BAR_COLOR);
            }
        }
        assert!(menu_bar(&no_time).is_err());
        let mut high = desktop(200);
        for x in 0..200 {
            let at = (MENU_BAR * 200 + x) * 3;
            high.pixels[at..at + 3].copy_from_slice(&MENU_BAR_COLOR);
        }
        assert!(menu_bar(&high).is_err());
        let none = Image {
            pixels: vec![0x1e; 200 * 40 * 3],
            ..desktop(200)
        };
        assert!(menu_bar(&none).is_err());
    }

    #[test]
    fn a_png_reads_back_as_written() {
        let file = std::env::temp_dir().join(format!("bz-png-{}.png", std::process::id()));
        let image = pattern(5, 3);
        image.write_png(&file).unwrap();
        let read = Image::read_png(&fs::read(&file).unwrap()).unwrap();
        fs::remove_file(&file).unwrap();
        assert_eq!((read.width, read.height), (5, 3));
        assert_eq!(read.pixels, image.pixels);
        assert_eq!(read.pixel(4, 2), expected(4, 2, 5, 3));
    }
}
