//! The desktop (ADR 0027 §6): the owner signs in on tty1 at the keyboard
//! and types `beize-session`; the compositor runs as the owner in its
//! sandbox, logind knows the session as a Wayland one, a terminal opens
//! with the desktop and another with Ctrl+Alt+T. A command typed into it
//! shows the terminal's sample as the reference in the repository has it.
//! A client's window fills the screen and the screen shows exactly its
//! pattern. Beize UI's
//! test program shows its sample – text, colours, blending – exactly as it
//! draws it offscreen, the reference in the repository. Signing out stops
//! the desktop with its terminals and takes its environment back.
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

/// Where pixels may differ: in the square this wide at the screen's top
/// left, the pointer, drawn into the image in the VM (ADR 0027 §8), 24×24
/// at most. The compositor starts it at (0, 0) and the test never moves it,
/// so any pixel elsewhere is a fault, down to one changed character.
const POINTER_SIZE: usize = 24;

/// The compositor's warning that Smithay could not become DRM master
/// itself: it already is, through the device logind handed over (reported
/// to Smithay, ~/upstream/smithay/ISSUES.md). Every other warning fails.
const KNOWN_WARNING: &str = "Unable to become drm master, assuming unprivileged mode";

/// Beize UI's sample as its test program draws it offscreen at the VM's
/// screen size (`beize-ui-test --offscreen FILE 1280x800`), relative to the
/// repository. Replaced only by hand, after looking at the screen the test
/// leaves in the logs when they differ (ADR 0027 §9).
const UI_SAMPLE: &str = "os/beize-ui/tests/sample.png";

/// The screen once the terminal has run its sample
/// (`os/beize-terminal/tests/sample.sh`), as a run of this test showed it
/// (openQA's needles): replaced only by hand, after looking at it.
const TERMINAL_SAMPLE: &str = "os/beize-terminal/tests/sample.png";

/// What the sample is called on the test medium, typed into the terminal.
const TERMINAL_SAMPLE_COMMAND: &str = "/run/t/beize-terminal-sample";

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
    let differing = wait_for_screen(paths, &machine, TERMINAL_SAMPLE, "terminal")?;
    say!(
        "  ok: the typed command ran in the terminal, the screen shows its sample as the reference \
         has it ({differing} pixels differ: the pointer)"
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
    // Its first frame reaches the screen with the next refresh.
    thread::sleep(Duration::from_secs(1));
    let file = machine.logs.join("desktop-screen.ppm");
    screendump(&qmp, &file)?;
    let screen = Image::read_ppm(&fs::read(&file)?)?;
    ensure!(
        (screen.width, screen.height) == size,
        "the window is {}x{}, the screen {}x{}: it does not fill it",
        size.0,
        size.1,
        screen.width,
        screen.height
    );
    let differing = compare(
        &screen,
        |x, y| expected(x, y, size.0, size.1),
        "the client's pattern",
        &machine.logs.join("desktop-difference.ppm"),
    )?;
    say!(
        "  ok: the screen shows the client's window, {}x{} ({differing} pixels differ: the pointer)",
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
    let differing = wait_for_screen(paths, &machine, UI_SAMPLE, "beize-ui")?;
    say!(
        "  ok: Beize UI's window shows its sample as the reference has it ({differing} pixels \
         differ: the pointer)"
    );

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
         graphical-session.target; systemctl --user show-environment",
    )?;
    ensure!(
        lines.iter().filter(|l| *l == "inactive").count() == 3,
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
    say!("  ok: signing out stopped the desktop and its terminals and took its environment back");
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

/// Waits until the screen shows what `reference` (relative to the
/// repository) has, but the pointer, as openQA waits for a needle; the
/// pixels that differ. If it does not, the last screen is `<name>-screen.png`
/// in the logs, to look at and to take over if the new look is wanted.
fn wait_for_screen(paths: &Paths, machine: &Machine, reference: &str, name: &str) -> Result<usize> {
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
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        let compared = match &wanted {
            None => Err(anyhow!("there is no {reference} yet")),
            Some(r) if (r.width, r.height) != (screen.width, screen.height) => Err(anyhow!(
                "the screen is {}x{}, {reference} {}x{}",
                screen.width,
                screen.height,
                r.width,
                r.height
            )),
            Some(r) => compare(&screen, |x, y| r.pixel(x, y), reference, &difference),
        };
        match compared {
            Ok(differing) => return Ok(differing),
            Err(e) if Instant::now() >= deadline => break (screen, e),
            Err(_) => {}
        }
    };
    let (screen, e) = failed;
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.write_png(&shown)?;
    Err(e.context(format!("the screen is {}", shown.display())))
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
}

/// The client's pattern at (x, y) of a window `width`×`height`, as the
/// client draws it (`x < w / 2` is left).
fn expected(x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
    QUADRANTS[usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)]
}

/// The pixels of `screen` that differ from what is `wanted` (named `what`
/// in errors), all in the pointer's square; any outside it fail, and
/// `difference` then shows them in magenta over a dimmed screen.
fn compare(
    screen: &Image,
    wanted: impl Fn(usize, usize) -> [u8; 3],
    what: &str,
    difference: &Path,
) -> Result<usize> {
    let mut marked = Image {
        width: screen.width,
        height: screen.height,
        pixels: screen.pixels.iter().map(|c| c / 3).collect(),
    };
    let (mut pointer, mut elsewhere) = (0, 0);
    for y in 0..screen.height {
        for x in 0..screen.width {
            let at = (y * screen.width + x) * 3;
            let shown = &screen.pixels[at..at + 3];
            if shown
                .iter()
                .zip(wanted(x, y))
                .any(|(s, w)| s.abs_diff(w) > CHANNEL_TOLERANCE)
            {
                if x < POINTER_SIZE && y < POINTER_SIZE {
                    pointer += 1;
                } else {
                    elsewhere += 1;
                }
                marked.pixels[at..at + 3].copy_from_slice(&[255, 0, 255]);
            }
        }
    }
    if elsewhere > 0 {
        marked.write_ppm(difference)?;
        bail!(
            "{elsewhere} pixels of the screen differ from {what} outside the pointer's \
             {POINTER_SIZE}x{POINTER_SIZE} at the top left; they are magenta in {}",
            difference.display()
        );
    }
    Ok(pointer)
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
    fn the_pattern_passes_with_a_pointer_and_fails_otherwise() {
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
        assert_eq!(compare(&screen, &difference).unwrap(), 0);
        // Rounding, and a pointer.
        screen.pixels[0] += CHANNEL_TOLERANCE;
        for y in 0..POINTER_SIZE {
            for x in 0..POINTER_SIZE {
                let at = (y * 64 + x) * 3;
                screen.pixels[at..at + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        assert_eq!(
            compare(&screen, &difference).unwrap(),
            POINTER_SIZE * POINTER_SIZE
        );
        assert!(!difference.exists());
        // One pixel more, beside the pointer: a character that changed.
        let mut beside = screen.pixels.clone();
        beside[3 * POINTER_SIZE] = 0;
        assert!(
            compare(
                &Image {
                    pixels: beside,
                    ..pattern(64, 40)
                },
                &difference
            )
            .is_err()
        );
        fs::remove_file(&difference).unwrap();
        // And far from it.
        screen.pixels[3 * 64 * 39] = 0;
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
