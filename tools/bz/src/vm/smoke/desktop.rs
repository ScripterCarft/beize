//! The desktop (ADR 0027 §6): the owner signs in on tty1 at the keyboard
//! and types `beize-session`; the compositor runs as the owner in its
//! sandbox, logind knows the session as a Wayland one. The shell (ADR 0028)
//! runs in its own sandbox, the compositor knows it, and its menu bar lies
//! along the top with the time. A terminal opens with the desktop and
//! another with Ctrl+Alt+T. A command typed into it shows the terminal's
//! sample as the reference in the repository has it. Windows float (ADR
//! 0028 §2): a client's window opens in its own size, centred below the
//! menu bar, shows exactly its pattern and is offered no panels; dragged by
//! its top it moves, dragged by its corner it grows, both by exactly the
//! drag, and maximized it fills the screen below the bar and comes back.
//! Beize UI's test program shows its sample – text, colours, blending –
//! below its title bar exactly as it draws it offscreen, the reference in
//! the repository. Killed, the shell comes back and the window stays. The
//! title bar moves the window, maximizes it with a double click and back,
//! minimizes it, and closes it; the dock brings it back. A client that asks the compositor for its frame
//! (xdg-decoration) gets the same title bar, its buttons the same to the
//! pixel, above its pattern, centred with it; the bar moves, maximizes and
//! closes its window as Beize UI's does. Windows have round corners
//! (compared without them), maximized ones do not. The dock (ADR 0028 §4)
//! floats along the bottom with round corners, the pinned terminal first and
//! every running app with it; a click starts the terminal once none runs.
//! The menu bar names the active app; the shell's list of windows is
//! offered to no app.
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
    Answers, OWNER_CONSOLE_CREDENTIALS, SMOKE_PASSWORD, SMOKE_USER, click, drag, german_presses,
    keys, screendump,
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
/// (0, 0), and the test puts it back there after each drag (`park`). That
/// is on the menu bar: windows are compared without exception, down to one
/// changed character.
const POINTER_SIZE: usize = 24;

/// The menu bar along the top of the screen (os/beize-shell, `HEIGHT` and
/// `BAR`); windows lie below it.
const MENU_BAR: usize = 24;
const MENU_BAR_COLOR: [u8; 3] = [0xf5, 0xf5, 0xf5];
/// Where the active app's name is: the left end of the bar, at most this
/// wide (the shell's journal says which name it shows, `SHELL_SAID`).
const APP_NAME_WIDTH: usize = 200;
// The pointer, parked on the bar, lies in that end too.
const _: () = assert!(APP_NAME_WIDTH >= POINTER_SIZE);
/// The dock (os/beize-shell, `dock`): this high, this far above the bottom
/// of the screen, its tiles this wide with this gap and padding; windows
/// stay above it, so the window area ends `DOCK_ZONE` above the bottom.
const DOCK_HEIGHT: usize = 64;
const DOCK_MARGIN: usize = 6;
const DOCK_TILE: usize = 48;
const DOCK_GAP: usize = 8;
const DOCK_PADDING: usize = 8;
const DOCK_ZONE: usize = DOCK_HEIGHT + DOCK_MARGIN;
const DOCK_COLOR: [u8; 3] = [0xf5, 0xf5, 0xf5];
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
/// its window (`beize-ui-test --offscreen FILE 800x600`, Beize UI's
/// default), relative to the repository. Replaced only by hand, after
/// looking at the screen the test leaves in the logs when they differ (ADR
/// 0027 §9).
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

/// What the shell said in this boot (os/beize-shell): `dock: …` for the
/// apps in the dock, `menu bar: <name>` for the active app.
const SHELL_SAID: &str = "journalctl --user -b -u beize-shell.service -o cat";

/// The compositor's line for each window it places (os/beize-compositor,
/// `windows`): `window <app id> <w>x<h> at <x>,<y>`.
const PLACED: &str = "journalctl --user -b -u beize-compositor.service -o cat | grep '^window '";

/// The test client's top rows, which move its window like a title bar
/// (os/beize-testclient, `TITLE`).
const CLIENT_TITLE: usize = 32;
/// How far the test drags the client's window, by its top and then by its
/// bottom right corner: clear of the terminals' windows below it, and of
/// the dock, which lies over windows dragged under it (as macOS' does).
const MOVE_BY: (i32, i32) = (-150, 60);
const RESIZE_BY: (i32, i32) = (60, 40);

/// Beize UI's title bar along the top of its windows (os/beize-ui,
/// `titlebar`), and how far the test drags a window by it.
const TITLE_BAR: usize = 28;
// Its 800x628 window stays clear of the dock (`MOVE_BY`).
const UI_MOVE_BY: (i32, i32) = (-100, 20);
/// The bar's circles at its left end, 20 px apart from 8 px on, close
/// centred 14 px in (os/beize-draw, `titlebar`); compared from the end of
/// the round corner on.
const BUTTONS: (usize, usize) = (CORNER, 68);
const CLOSE_X: usize = 14;
const CIRCLE_STEP: usize = 20;
/// A place on a bar away from its circles.
const BAR_GRIP_X: usize = 120;
/// The windows' round corners (os/beize-compositor, `corners`): the
/// squares they lie in are left out where windows are compared.
const CORNER: usize = 10;
/// How far the test drags the framed client's window by the compositor's
/// bar.
const FRAMED_MOVE_BY: (i32, i32) = (-120, 60);

/// How long a screen may take to become what its reference shows.
const SCREEN_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn desktop(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-desktop")?;
    machine.test_medium = test_medium(paths)?;
    // Drags land on the pixel (`console::drag`).
    machine.tablet = true;
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
    wait_for_panels(&machine)?;
    say!(
        "  ok: the shell runs sandboxed, the compositor knows it, its menu bar shows the time, \
         its dock floats along the bottom with round corners"
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
    wait_for_shell_line(&shell, |l| l == "dock: beize-terminal running")?;
    wait_for_shell_line(&shell, |l| l == "menu bar: Terminal")?;
    say!(
        "  ok: the desktop started with a terminal, Ctrl+Alt+T opened another, each with its \
         shell; the dock shows the terminal running, the menu bar names it"
    );

    // The new terminal has the keyboard: typed into once its shell reads,
    // the sample's command runs and the screen shows what it printed.
    let second = both
        .iter()
        .find(|u| **u != first[0])
        .context("no second terminal")?;
    wait_for_prompt(&shell, second)?;
    keys(&qmp, &german_presses(TERMINAL_SAMPLE_COMMAND)?)?;
    let terminal = placed(&shell, "beize-terminal", 2)?;
    wait_for_screen(paths, &machine, terminal, TERMINAL_SAMPLE, "terminal")?;
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
    let configured = |lines: &[String]| {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix("configure "))
            .filter_map(|s| s.split_once('x'))
            .filter_map(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?)))
            .next_back()
    };
    let size =
        configured(&lines).with_context(|| format!("the client got no window: {lines:?}"))?;
    // An app is not offered the shell's panels (the list is real: it has
    // xdg-shell).
    let globals = lines
        .iter()
        .find_map(|l| l.strip_prefix("globals "))
        .with_context(|| format!("the client printed no globals: {lines:?}"))?;
    let offered: Vec<&str> = globals.split(' ').collect();
    ensure!(
        offered.contains(&"xdg_wm_base")
            && !offered.contains(&"zwlr_layer_shell_v1")
            && !offered.contains(&"beize_shell_v1"),
        "an app was offered the shell's panels or list of windows, or no windows: {globals}"
    );
    // It opens in the size it chose (the compositor left it the choice),
    // centred in what the menu bar leaves of the screen.
    let (at, placed_size) = placed(&shell, "beize-testclient", 1)?;
    ensure!(
        placed_size == size,
        "the client drew {}x{}, the compositor placed {}x{}",
        size.0,
        size.1,
        placed_size.0,
        placed_size.1
    );
    let screen = screen_size(&machine)?;
    let centred = (
        (screen.0 - size.0) / 2,
        MENU_BAR + (screen.1 - MENU_BAR - DOCK_ZONE - size.1) / 2,
    );
    ensure!(
        at == centred,
        "the client's window is at {},{}, not centred below the menu bar at {},{}",
        at.0,
        at.1,
        centred.0,
        centred.1
    );
    wait_for_pattern(&shell, &machine, at, size, "desktop")?;
    // Its corner is round: the corner pixel shows what is below.
    let desktop = Image::read_ppm(&fs::read(machine.logs.join("desktop-screen.ppm"))?)?;
    ensure!(
        desktop.pixel(at.0, at.1) != QUADRANTS[0],
        "the client's window has a square corner"
    );
    say!(
        "  ok: the client's window opened in its own size, {}x{}, centred below the menu bar, \
         exactly its pattern; it is offered no panels",
        size.0,
        size.1
    );

    // Dragged by its top, the window follows the pointer; dragged from just
    // outside its bottom right corner (the compositor's edge), it grows.
    let point = |(x, y): (usize, usize), (dx, dy): (i32, i32)| -> Result<(i32, i32)> {
        Ok((i32::try_from(x)? + dx, i32::try_from(y)? + dy))
    };
    let grip = point(
        at,
        (i32::try_from(size.0 / 2)?, i32::try_from(CLIENT_TITLE / 2)?),
    )?;
    let by = |(x, y): (i32, i32), (dx, dy): (i32, i32)| (x + dx, y + dy);
    drag(&qmp, screen, grip, by(grip, MOVE_BY), true)?;
    park(&qmp, screen)?;
    let moved = point(at, MOVE_BY)?;
    let moved = (usize::try_from(moved.0)?, usize::try_from(moved.1)?);
    wait_for_pattern(&shell, &machine, moved, size, "moved")?;
    let corner = point((moved.0 + size.0, moved.1 + size.1), (2, 2))?;
    drag(&qmp, screen, corner, by(corner, RESIZE_BY), true)?;
    park(&qmp, screen)?;
    let grown = (
        size.0 + usize::try_from(RESIZE_BY.0)?,
        size.1 + usize::try_from(RESIZE_BY.1)?,
    );
    wait_for_pattern(&shell, &machine, moved, grown, "resized")?;
    say!(
        "  ok: dragged by its top the window moved, by its corner it grew to {}x{}, both by \
         exactly the drag",
        grown.0,
        grown.1
    );

    // M maximizes it: it fills the screen below the menu bar; again, and it
    // is back in its place and size.
    keys(&qmp, &["m"])?;
    wait_for_pattern(
        &shell,
        &machine,
        (0, MENU_BAR),
        (screen.0, screen.1 - MENU_BAR - DOCK_ZONE),
        "maximized",
    )?;
    // Maximized, its corners are the screen's: square.
    let maximized = Image::read_ppm(&fs::read(machine.logs.join("maximized-screen.ppm"))?)?;
    ensure!(
        maximized.pixel(0, MENU_BAR) == QUADRANTS[0],
        "the maximized window has a round corner"
    );
    keys(&qmp, &["m"])?;
    wait_for_pattern(&shell, &machine, moved, grown, "restored")?;
    say!(
        "  ok: maximized the window filled the screen between the menu bar and the dock, and \
         came back to its place and size"
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
    let Some(pacing) = lines.iter().find(|l| l.starts_with("pacing: ")) else {
        let said = last_said(&shell, &["beize-testclient-frames", "beize-compositor"])?;
        bail!(
            "no pacing summary (exit status {status}): {lines:?}; the client and the compositor \
             last said: {said:?}"
        );
    };
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
    let ui = placed(&shell, "beize-ui-test", 1)?;
    wait_for_screen(paths, &machine, content(ui), UI_SAMPLE, "beize-ui")?;
    // An app without a desktop file: its id stands for its name.
    wait_for_shell_line(&shell, |l| l == "menu bar: beize-ui-test")?;
    wait_for_shell_line(&shell, |l| {
        l.starts_with("dock: beize-terminal running, ") && l.contains("beize-ui-test running")
    })?;
    say!("  ok: Beize UI's window shows its sample below its title bar as the reference has it");

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
    wait_for_panels(&machine)?;
    wait_for_screen(
        paths,
        &machine,
        content(ui),
        UI_SAMPLE,
        "beize-ui-after-shell",
    )?;
    say!(
        "  ok: killed, the shell came back with its menu bar and dock; the desktop and the \
         window stayed"
    );

    // Beize UI's title bar (ADR 0028 §2): dragged, it moves the window by
    // exactly the drag; a double click maximizes the window, another
    // brings it back; its close button ends the program.
    let ((x, y), (w, h)) = ui;
    let grip = (i32::try_from(x + w / 2)?, i32::try_from(y + TITLE_BAR / 2)?);
    let to = (grip.0 + UI_MOVE_BY.0, grip.1 + UI_MOVE_BY.1);
    drag(&qmp, screen, grip, to, true)?;
    park(&qmp, screen)?;
    let moved = (
        usize::try_from(i32::try_from(x)? + UI_MOVE_BY.0)?,
        usize::try_from(i32::try_from(y)? + UI_MOVE_BY.1)?,
    );
    wait_for_screen(
        paths,
        &machine,
        content((moved, (w, h))),
        UI_SAMPLE,
        "beize-ui-moved",
    )?;
    click(&qmp, screen, to, 2)?;
    park(&qmp, screen)?;
    let full = (screen.0, screen.1 - MENU_BAR - DOCK_ZONE - TITLE_BAR);
    wait_for_configure(&shell, "beize-ui-test", full)?;
    // On the maximized window's bar, away from its buttons.
    let bar = (
        i32::try_from(screen.0 / 2)?,
        i32::try_from(MENU_BAR + TITLE_BAR / 2)?,
    );
    click(&qmp, screen, bar, 2)?;
    park(&qmp, screen)?;
    wait_for_configure(&shell, "beize-ui-test", (w, h - TITLE_BAR))?;
    wait_for_screen(
        paths,
        &machine,
        content((moved, (w, h))),
        UI_SAMPLE,
        "beize-ui-restored",
    )?;
    // Its buttons, as the screen shows them now (the window active, the
    // pointer away): the compositor's frame has to show the same.
    let restored = machine.logs.join("beize-ui-restored-screen.ppm");
    let restored_screen = Image::read_ppm(&fs::read(&restored)?)?;
    let ui_shown = restored_screen.crop(content((moved, (w, h))).0, content((moved, (w, h))).1)?;
    let ui_buttons = restored_screen.crop(
        (moved.0 + BUTTONS.0, moved.1),
        (BUTTONS.1 - BUTTONS.0, TITLE_BAR),
    )?;
    // The yellow circle minimizes it: off the screen; its app's tile in the
    // dock brings it back to its place.
    let minimize = (
        i32::try_from(moved.0 + CLOSE_X + CIRCLE_STEP)?,
        i32::try_from(moved.1 + TITLE_BAR / 2)?,
    );
    click(&qmp, screen, minimize, 1)?;
    park(&qmp, screen)?;
    wait_for_compositor_line(&shell, "minimized beize-ui-test")?;
    wait_for_gone(
        &machine,
        content((moved, (w, h))),
        &ui_shown,
        "beize-ui-minimized",
    )?;
    let apps = last_dock(&shell)?;
    let tile = apps
        .iter()
        .position(|a| a == "beize-ui-test running")
        .with_context(|| format!("beize-ui-test is not in the dock: {apps:?}"))?;
    click(&qmp, screen, dock_tile(screen, apps.len(), tile)?, 1)?;
    park(&qmp, screen)?;
    wait_for_compositor_line(&shell, "brought back beize-ui-test")?;
    wait_for_screen(
        paths,
        &machine,
        content((moved, (w, h))),
        UI_SAMPLE,
        "beize-ui-back",
    )?;
    // The close button, the red circle.
    let close = (
        i32::try_from(moved.0 + CLOSE_X)?,
        i32::try_from(moved.1 + TITLE_BAR / 2)?,
    );
    click(&qmp, screen, close, 1)?;
    park(&qmp, screen)?;
    let (_, lines) = shell.capture(
        "for i in $(seq 50); do systemctl --user -q is-active beize-ui-test || break; \
         sleep 0.2; done; systemctl --user is-active beize-ui-test; \
         journalctl --user -u beize-ui-test -o cat | tail -n 1",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "done"),
        "the close button did not end Beize UI's test program: {lines:?}"
    );
    say!(
        "  ok: Beize UI's title bar moved the window by exactly the drag, a double click \
         maximized it and back, the yellow circle minimized it and the dock brought it back, \
         its close button closed it"
    );

    framed(&shell, &machine, &qmp, screen, &ui_buttons)?;

    // With no terminal left, the dock's first tile starts one.
    let (_, lines) = shell.capture("systemctl --user stop 'app-beize\\x2dterminal@*'")?;
    ensure!(
        terminals(&shell)?.is_empty(),
        "the terminals did not stop: {lines:?}"
    );
    let apps = wait_for_dock(&shell, |apps| {
        apps.first().is_some_and(|a| a == "beize-terminal")
    })?;
    click(&qmp, screen, dock_tile(screen, apps.len(), 0)?, 1)?;
    park(&qmp, screen)?;
    wait_for_terminals(&shell, 1)?;
    wait_for_shell_line(&shell, |l| l.starts_with("dock: beize-terminal running"))?;
    say!("  ok: with no terminal running, the dock's tile started one");

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

/// A client that asks for the compositor's frame (xdg-decoration, ADR
/// 0028 §2): it is given it, the frame is centred below the menu bar with
/// the bar above the client's pattern, its buttons the same as Beize UI's
/// to the pixel; the bar moves the window by exactly the drag, a double
/// click maximizes it below the bar and back, its close button ends the
/// client.
fn framed(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    ui_buttons: &Image,
) -> Result<()> {
    const UNIT: &str = "beize-testclient-framed";
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --server-side --seconds 60 \
         && for i in $(seq 50); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; journalctl --user -u {UNIT} -o cat"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "decoration server_side"),
        "the client that asked for the compositor's frame was not given it: {lines:?}"
    );
    // The test client's third window: the first moved, the second drew
    // frames.
    let (at, size) = placed(shell, "beize-testclient", 3)?;
    let frame_height = size.1 + TITLE_BAR;
    let centred = (
        (screen.0 - size.0) / 2,
        MENU_BAR + (screen.1 - MENU_BAR - DOCK_ZONE - frame_height) / 2 + TITLE_BAR,
    );
    ensure!(
        at == centred,
        "the framed client's content is at {},{}, not below its bar in a frame centred below \
         the menu bar, at {},{}",
        at.0,
        at.1,
        centred.0,
        centred.1
    );
    wait_for_pattern(shell, machine, at, size, "framed")?;
    let buttons = |(x, y): (usize, usize)| {
        (
            (x + BUTTONS.0, y - TITLE_BAR),
            (BUTTONS.1 - BUTTONS.0, TITLE_BAR),
        )
    };
    wait_for_image(
        machine,
        buttons(at),
        Some(ui_buttons),
        "Beize UI's buttons",
        "framed-buttons",
    )?;
    say!(
        "  ok: a client that asked for the compositor's frame got it, centred below the menu bar, \
         its pattern exactly below the bar, the buttons the same as Beize UI's to the pixel"
    );

    let on_bar = |(x, y): (usize, usize)| -> Result<(i32, i32)> {
        Ok((
            i32::try_from(x + BAR_GRIP_X)?,
            i32::try_from(y - TITLE_BAR / 2)?,
        ))
    };
    let grip = on_bar(at)?;
    let to = (grip.0 + FRAMED_MOVE_BY.0, grip.1 + FRAMED_MOVE_BY.1);
    drag(qmp, screen, grip, to, true)?;
    park(qmp, screen)?;
    let moved = (
        usize::try_from(i32::try_from(at.0)? + FRAMED_MOVE_BY.0)?,
        usize::try_from(i32::try_from(at.1)? + FRAMED_MOVE_BY.1)?,
    );
    wait_for_pattern(shell, machine, moved, size, "framed-moved")?;
    click(qmp, screen, on_bar(moved)?, 2)?;
    park(qmp, screen)?;
    let full = (screen.0, screen.1 - MENU_BAR - DOCK_ZONE - TITLE_BAR);
    wait_for_configure(shell, UNIT, full)?;
    let below_bar = (0, MENU_BAR + TITLE_BAR);
    wait_for_pattern(shell, machine, below_bar, full, "framed-maximized")?;
    click(qmp, screen, on_bar(below_bar)?, 2)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, size, "framed-restored")?;
    let close = (
        i32::try_from(moved.0 + CLOSE_X)?,
        i32::try_from(moved.1 - TITLE_BAR / 2)?,
    );
    click(qmp, screen, close, 1)?;
    park(qmp, screen)?;
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 50); do systemctl --user -q is-active {UNIT} || break; sleep 0.2; done; \
         systemctl --user is-active {UNIT}; journalctl --user -u {UNIT} -o cat | tail -n 1"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "done"),
        "the close button of the compositor's frame did not end the client: {lines:?}"
    );
    say!(
        "  ok: the compositor's bar moved the window by exactly the drag, a double click \
         maximized it below the bar and back, its close button closed it"
    );
    Ok(())
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

/// Where the compositor placed the `count`th window of `app` and its size,
/// waiting for it.
fn placed(shell: &Shell, app: &str, count: usize) -> Result<Window> {
    let mut found = Vec::new();
    for _ in 0..50 {
        let (_, lines) = shell.capture(PLACED)?;
        found = lines
            .iter()
            .filter_map(|l| placement(l, app))
            .collect::<Vec<_>>();
        if found.len() >= count {
            return Ok(found[count - 1]);
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "the compositor placed {} windows of {app}, not {count}",
        found.len()
    )
}

/// A window: its top left corner on the screen and its size.
type Window = ((usize, usize), (usize, usize));

/// The window of `app` that `line` (`window <app> <w>x<h> at <x>,<y>`)
/// names.
fn placement(line: &str, app: &str) -> Option<Window> {
    let rest = line.strip_prefix("window ")?.strip_prefix(app)?;
    let (size, at) = rest.trim_start().split_once(" at ")?;
    let (w, h) = size.split_once('x')?;
    let (x, y) = at.split_once(',')?;
    Some((
        (x.parse().ok()?, y.parse().ok()?),
        (w.parse().ok()?, h.parse().ok()?),
    ))
}

/// The screen's size, from a screendump.
fn screen_size(machine: &Machine) -> Result<(usize, usize)> {
    let file = machine.logs.join("desktop-screen.ppm");
    screendump(&machine.qmp(), &file)?;
    let screen = Image::read_ppm(&fs::read(&file)?)?;
    Ok((screen.width, screen.height))
}

/// What `units` last said but the pointer's moves, for a failure: the VM's
/// journal is gone with the VM.
fn last_said(shell: &Shell, units: &[&str]) -> Result<Vec<String>> {
    let units: String = units.iter().map(|u| format!(" -u {u}")).collect();
    let (_, said) = shell.capture(&format!(
        "journalctl --user -b -o cat{units} | grep -v '^pointer at' | tail -n 30"
    ))?;
    Ok(said)
}

/// The content of a window of Beize UI: below its title bar.
fn content(((x, y), (w, h)): Window) -> Window {
    ((x, y + TITLE_BAR), (w, h.saturating_sub(TITLE_BAR)))
}

/// Waits until `app` (a program printing `configure WxH` for each
/// configure, as Beize UI's test program does) was last configured to
/// `size`.
fn wait_for_configure(shell: &Shell, app: &str, size: (usize, usize)) -> Result<()> {
    let wanted = format!("configure {}x{}", size.0, size.1);
    let mut last = None;
    for _ in 0..50 {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -u {app} -o cat | grep '^configure ' | tail -n 1"
        ))?;
        last = lines.into_iter().find(|l| l.starts_with("configure "));
        if last.as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("{app} was not configured to {wanted}, last: {last:?}")
}

/// Puts the pointer back on the screen's top left corner, on the menu bar.
fn park(qmp: &Path, screen: (usize, usize)) -> Result<()> {
    drag(qmp, screen, (0, 0), (0, 0), false)
}

/// Waits until the test client's window `at` shows exactly its pattern in
/// `size`; if it does not, the screen and the difference are in the logs.
fn wait_for_pattern(
    shell: &Shell,
    machine: &Machine,
    at: (usize, usize),
    size: (usize, usize),
    name: &str,
) -> Result<()> {
    let file = machine.logs.join(format!("{name}-screen.ppm"));
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let compared = Image::read_ppm(&fs::read(&file)?)?
            .crop(at, size)
            .and_then(|window| {
                compare(
                    &window,
                    |x, y| expected(x, y, size.0, size.1),
                    "the client's pattern",
                    &difference,
                )
            });
        match compared {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                let said = last_said(shell, &["beize-testclient", "beize-compositor"])?;
                return Err(e.context(format!(
                    "the client's window, {}x{} at {},{}, on {}; the client and the compositor \
                     last said: {said:?}",
                    size.0,
                    size.1,
                    at.0,
                    at.1,
                    file.display()
                )));
            }
            Err(_) => {}
        }
    }
}

/// Waits until `window` on the screen shows exactly what `reference`
/// (relative to the repository) has, as openQA waits for a needle. If it
/// does not, the last window is `<name>-screen.png` in the logs, to look at
/// and to take over if the new look is wanted.
fn wait_for_screen(
    paths: &Paths,
    machine: &Machine,
    window: Window,
    reference: &str,
    name: &str,
) -> Result<()> {
    let wanted = match fs::read(paths.root.join(reference)) {
        Ok(data) => Some(Image::read_png(&data).with_context(|| format!("reading {reference}"))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {reference}")),
    };
    wait_for_image(machine, window, wanted.as_ref(), reference, name)
}

/// Waits until `window` on the screen shows exactly `wanted` (named `what`
/// in errors); if it does not, the last window is `<name>-screen.png` in
/// the logs.
fn wait_for_image(
    machine: &Machine,
    (at, size): Window,
    wanted: Option<&Image>,
    what: &str,
    name: &str,
) -> Result<()> {
    let file = machine.logs.join(format!("{name}-screen.ppm"));
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    let failed = loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?.crop(at, size)?;
        let compared = match wanted {
            None => Err(anyhow!("there is no {what} yet")),
            Some(r) if (r.width, r.height) != (screen.width, screen.height) => Err(anyhow!(
                "the window is {}x{}, {what} {}x{}",
                screen.width,
                screen.height,
                r.width,
                r.height
            )),
            Some(r) => compare(&screen, |x, y| r.pixel(x, y), what, &difference),
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
/// on its right, and the dock along the bottom; if they do not, the last
/// screen is in the logs.
fn wait_for_panels(machine: &Machine) -> Result<()> {
    let file = machine.logs.join("panels-screen.ppm");
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        match menu_bar(&screen).and_then(|()| dock(&screen).map(|_| ())) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                let shown = machine.logs.join("panels-screen.png");
                screen.write_png(&shown)?;
                return Err(e.context(format!("the screen is {}", shown.display())));
            }
            Err(_) => {}
        }
    }
}

/// Whether `screen` has the menu bar: its colour across the top but the
/// pointer, the app's name and the time, and the time's ink at the right
/// end. Which time it shows depends on the minute, which name the shell's
/// journal says; Beize UI's text itself is checked exactly with the windows.
fn menu_bar(screen: &Image) -> Result<()> {
    ensure!(
        screen.width > CLOCK_WIDTH + APP_NAME_WIDTH && screen.height > MENU_BAR,
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
            if x < APP_NAME_WIDTH {
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
    let below = (APP_NAME_WIDTH..clock).all(|x| near(screen.pixel(x, MENU_BAR), MENU_BAR_COLOR));
    ensure!(!below, "the menu bar is higher than {MENU_BAR} pixels");
    Ok(())
}

/// The dock in `screen`, centred along the bottom: its colour in its
/// middle row from edge to edge, but for its tiles, and what is below
/// showing at its round corner. Its width says how many apps it has; that
/// number.
fn dock(screen: &Image) -> Result<usize> {
    let near = |a: [u8; 3]| {
        a.iter()
            .zip(DOCK_COLOR)
            .all(|(a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE)
    };
    ensure!(
        screen.height > DOCK_ZONE + MENU_BAR,
        "a screen {} high has no room for the dock",
        screen.height
    );
    let top = screen.height - DOCK_ZONE;
    // Through the tiles, where the corners are far.
    let row = top + DOCK_HEIGHT / 2;
    let centre = screen.width / 2;
    // Leftwards from the middle, over tiles (never wider than one), to the
    // dock's last pixel of its colour.
    let mut left = None;
    let mut run = 0;
    for x in (0..=centre).rev() {
        if near(screen.pixel(x, row)) {
            left = Some(x);
            run = 0;
        } else {
            run += 1;
            if run > DOCK_TILE {
                break;
            }
        }
    }
    let left = left.context("no dock along the bottom of the screen")?;
    // The hairline is one pixel outside the colour.
    let x = left - 1;
    let width = 2 * (centre - x);
    let tiles = (width + DOCK_GAP).saturating_sub(2 * DOCK_PADDING) / (DOCK_TILE + DOCK_GAP);
    let expected = 2 * DOCK_PADDING + tiles * DOCK_TILE + tiles.saturating_sub(1) * DOCK_GAP;
    ensure!(
        tiles >= 1 && width.abs_diff(expected) <= 1,
        "the dock is {width} wide, which is no number of apps"
    );
    // A round corner shows what is below, as the pixel beside the dock.
    let same = |a: [u8; 3], b: [u8; 3]| {
        a.iter()
            .zip(b)
            .all(|(a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE)
    };
    ensure!(
        x > 0 && same(screen.pixel(x, top), screen.pixel(x - 1, top)),
        "the dock's corner is square"
    );
    Ok(tiles)
}

/// The middle of the `i`th of `apps` tiles of the dock.
fn dock_tile(screen: (usize, usize), apps: usize, i: usize) -> Result<(i32, i32)> {
    let width = 2 * DOCK_PADDING + apps * DOCK_TILE + apps.saturating_sub(1) * DOCK_GAP;
    let x = (screen.0 - width) / 2 + DOCK_PADDING + i * (DOCK_TILE + DOCK_GAP) + DOCK_TILE / 2;
    let y = screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE / 2;
    Ok((i32::try_from(x)?, i32::try_from(y)?))
}

/// Waits until the shell said a line `wanted` takes, in this boot.
fn wait_for_shell_line(shell: &Shell, wanted: impl Fn(&str) -> bool) -> Result<String> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let (_, lines) = shell.capture(SHELL_SAID)?;
        if let Some(line) = lines.iter().find(|l| wanted(l)) {
            return Ok(line.clone());
        }
        ensure!(
            Instant::now() < deadline,
            "the shell did not say what was waited for; it said: {lines:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// The apps of the shell's last `dock:` line.
fn last_dock(shell: &Shell) -> Result<Vec<String>> {
    let (_, lines) = shell.capture(SHELL_SAID)?;
    let line = lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("dock: "))
        .with_context(|| format!("the shell named no apps in its dock: {lines:?}"))?;
    Ok(line.split(", ").map(str::to_owned).collect())
}

/// Waits until the shell's last `dock:` line has apps that `wanted` takes.
fn wait_for_dock(shell: &Shell, wanted: impl Fn(&[String]) -> bool) -> Result<Vec<String>> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let apps = last_dock(shell)?;
        if wanted(&apps) {
            return Ok(apps);
        }
        ensure!(
            Instant::now() < deadline,
            "the dock did not become what was waited for: {apps:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// Waits until the compositor said `line`, in this boot.
fn wait_for_compositor_line(shell: &Shell, line: &str) -> Result<()> {
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 50); do journalctl --user -b -u beize-compositor.service -o cat \
         | grep -qx '{line}' && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep -cx '{line}'"
    ))?;
    ensure!(
        lines.iter().any(|l| l != "0"),
        "the compositor did not say \"{line}\": {lines:?}"
    );
    Ok(())
}

/// Waits until `window` on the screen no longer shows `shown` – the window
/// left; if it does not, the last screen is in the logs.
fn wait_for_gone(machine: &Machine, (at, size): Window, shown: &Image, name: &str) -> Result<()> {
    let file = machine.logs.join(format!("{name}-screen.ppm"));
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?.crop(at, size)?;
        if screen.pixels != shown.pixels {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let png = machine.logs.join(format!("{name}-screen.png"));
            screen.write_png(&png)?;
            bail!("the window still shows: {}", png.display());
        }
    }
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

    /// The part of the image `at` in `size` (a window on the screen).
    fn crop(&self, at: (usize, usize), size: (usize, usize)) -> Result<Self> {
        ensure!(
            at.0 + size.0 <= self.width && at.1 + size.1 <= self.height,
            "a window of {}x{} at {},{} is not within the screen of {}x{}",
            size.0,
            size.1,
            at.0,
            at.1,
            self.width,
            self.height
        );
        let mut pixels = Vec::with_capacity(size.0 * size.1 * 3);
        for y in at.1..at.1 + size.1 {
            let row = (y * self.width + at.0) * 3;
            pixels.extend_from_slice(&self.pixels[row..row + size.0 * 3]);
        }
        Ok(Self {
            width: size.0,
            height: size.1,
            pixels,
        })
    }
}

/// The client's pattern at (x, y) of a window `width`×`height`, as the
/// client draws it (`x < w / 2` is left).
fn expected(x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
    QUADRANTS[usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)]
}

/// Whether `window` shows exactly what is `wanted` (named `what` in
/// errors), but rounding and its round corners (`CORNER`); if not,
/// `difference` shows the pixels that differ in magenta over a dimmed
/// window.
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
    let (w, h) = (window.width, window.height);
    let corner =
        |x: usize, y: usize| (x < CORNER || x + CORNER >= w) && (y < CORNER || y + CORNER >= h);
    for y in 0..h {
        for x in 0..w {
            if corner(x, y) {
                continue;
            }
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
        // A round corner is left out.
        screen.pixels[3] = 0;
        compare(&screen, &difference).unwrap();
        // One pixel past it: a character that changed.
        screen.pixels[CORNER * 3] = 0;
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

    /// A dark screen with a dock of `apps` tiles, its corners round or not.
    fn with_dock(apps: usize, round: bool) -> Image {
        let (w, h) = (400, 200);
        let mut screen = Image {
            width: w,
            height: h,
            pixels: [0x1e; 3].repeat(w * h),
        };
        let width = 2 * DOCK_PADDING + apps * DOCK_TILE + (apps - 1) * DOCK_GAP;
        let (x0, y0) = ((w - width) / 2, h - DOCK_ZONE);
        for y in y0..y0 + DOCK_HEIGHT {
            for x in x0..x0 + width {
                let corner = round && (x - x0).min(x0 + width - 1 - x) < 3 && y - y0 < 3;
                let edge = x == x0 || x == x0 + width - 1 || y == y0;
                let c = if corner {
                    [0x1e; 3]
                } else if edge {
                    [0xd0; 3]
                } else {
                    DOCK_COLOR
                };
                let at = (y * w + x) * 3;
                screen.pixels[at..at + 3].copy_from_slice(&c);
            }
        }
        for i in 0..apps {
            let tx = x0 + DOCK_PADDING + i * (DOCK_TILE + DOCK_GAP);
            for y in y0 + DOCK_PADDING..y0 + DOCK_PADDING + DOCK_TILE {
                for x in tx..tx + DOCK_TILE {
                    let at = (y * w + x) * 3;
                    screen.pixels[at..at + 3].copy_from_slice(&[0x3a; 3]);
                }
            }
        }
        screen
    }

    #[test]
    fn counts_the_dock_by_its_width_and_wants_round_corners() {
        for apps in 1..=4 {
            assert_eq!(dock(&with_dock(apps, true)).unwrap(), apps);
        }
        assert!(dock(&with_dock(2, false)).is_err());
        let none = Image {
            pixels: vec![0x1e; 400 * 200 * 3],
            ..with_dock(1, true)
        };
        assert!(dock(&none).is_err());
    }

    #[test]
    fn finds_the_menu_bar_with_its_time_and_nothing_else() {
        let screen = desktop(400);
        menu_bar(&screen).unwrap();
        // A window below it, cut out of the screen.
        let window = desktop(200)
            .crop((150, MENU_BAR), (50, 40 - MENU_BAR))
            .unwrap();
        assert_eq!((window.width, window.height), (50, 40 - MENU_BAR));
        assert_eq!(window.pixel(0, 0), [0x1e; 3]);
        assert_eq!(window.pixel(13, 0), [0x1e; 3]);
        assert!(desktop(200).crop((151, MENU_BAR), (50, 1)).is_err());
        // A stray pixel on the bar, no time, a bar too high, no bar at all.
        let mut stray = desktop(400);
        stray.pixels[(5 * 400 + 300) * 3] = 0;
        assert!(menu_bar(&stray).is_err());
        let mut no_time = desktop(400);
        for x in 360..400 {
            for y in 0..MENU_BAR {
                let at = (y * 400 + x) * 3;
                no_time.pixels[at..at + 3].copy_from_slice(&MENU_BAR_COLOR);
            }
        }
        assert!(menu_bar(&no_time).is_err());
        let mut high = desktop(400);
        for x in 0..400 {
            let at = (MENU_BAR * 400 + x) * 3;
            high.pixels[at..at + 3].copy_from_slice(&MENU_BAR_COLOR);
        }
        assert!(menu_bar(&high).is_err());
        let none = Image {
            pixels: vec![0x1e; 400 * 40 * 3],
            ..desktop(400)
        };
        assert!(menu_bar(&none).is_err());
    }

    #[test]
    fn reads_where_the_compositor_placed_a_window() {
        let line = "window beize-testclient 640x480 at 320,172";
        assert_eq!(
            placement(line, "beize-testclient"),
            Some(((320, 172), (640, 480)))
        );
        assert_eq!(placement(line, "beize-terminal"), None);
        assert_eq!(
            placement("window beize-testclient 640x480", "beize-testclient"),
            None
        );
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
