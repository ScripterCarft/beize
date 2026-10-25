//! The desktop (ADR 0027 §6): the owner signs in on tty1 at the keyboard
//! and types `beize-session`; the compositor runs as the owner in its
//! sandbox, logind knows the session as a Wayland one. The shell (ADR 0028)
//! runs in its own sandbox, the compositor knows it, and its menu bar lies
//! along the top with the time, without a background of its own. The
//! desktop appears in one piece (ADR 0028 §6), once the shell's panels are
//! drawn, with no terminal; Ctrl+Alt+T opens one, and another. A command
//! typed into it shows the terminal's
//! sample as the reference in the repository has it. Windows float (ADR
//! 0028 §2): a client's window opens in its own size, centred below the
//! menu bar, shows exactly its pattern and is offered no panels; dragged by
//! its top it moves, dragged by its corner it grows, both by exactly the
//! drag, and maximized it fills the screen below the bar and comes back.
//! Beize UI's test program shows its sample – text, colours, blending –
//! below its title bar exactly as it draws it offscreen, the reference in
//! the repository. Killed, the shell comes back, its panels kept until the
//! new one is ready, and the window stays. The
//! title bar moves the window, maximizes it with a double click and back,
//! minimizes it, and closes it; the dock brings it back. A client that asks the compositor for its frame
//! (xdg-decoration) gets the same title bar, its buttons the same to the
//! pixel, above its pattern, centred with it; the bar moves, maximizes and
//! closes its window as Beize UI's does. Windows have round corners
//! (compared without them), maximized ones do not. The dock (ADR 0028 §4)
//! floats along the bottom with round corners on live glass, which shows a
//! window dragged behind it, the pinned terminal first and
//! every running app with it, the client with the icon its desktop file
//! names in the owner's hicolor theme; a click starts the terminal once
//! none runs.
//! The menu bar names the active app; the shell's list of windows is
//! offered to no app. The pointer is the theme's (ADR 0028 §8): the text
//! cursor the terminal names over its text (cursor-shape-v1), a resize
//! arrow on a window's edge, the arrow over a client that names none and
//! wherever a window closed under the pointer, without it moving.
//! The load test's scene (ADR 0028 §9) runs: many windows of Beize UI,
//! each drawing every frame, one of them half behind the dock, whose live
//! glass follows what moves there; every window draws throughout, and the
//! compositor sums its frames up. The VM checks that it works, not how fast
//! (lavapipe): the hardware test measures that.
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

/// The menu bar along the top of the screen (os/beize-shell, `HEIGHT`);
/// windows lie below it.
const MENU_BAR: usize = 24;
/// Where the active app's name is: the left end of the bar, at most this
/// wide (the shell's journal says which name it shows, `SHELL_SAID`).
const APP_NAME_WIDTH: usize = 200;
// The pointer, parked on the bar, lies in that end too.
const _: () = assert!(APP_NAME_WIDTH >= POINTER_SIZE);
/// The dock (os/beize-shell, `dock`): this high, this far above the bottom
/// of the screen, its tiles this wide with this gap and padding; windows
/// stay above it, so the window area ends `DOCK_ZONE` above the bottom.
const DOCK_HEIGHT: usize = 68;
const DOCK_MARGIN: usize = 5;
const DOCK_TILE: usize = 48;
const DOCK_GAP: usize = 4;
const DOCK_PADDING: usize = 10;
const DOCK_ZONE: usize = DOCK_HEIGHT + DOCK_MARGIN;
/// The menu bar has no background (ADR 0028 §3), the dock lies on glass
/// (§6): what shows there is the background, the scrim over it, blurred
/// glass – no colour to compare with, but soft: no pixel further than this
/// from its neighbour in a channel (a gradient's slope, grain, dithering).
const GLASS_SMOOTH: u8 = 6;
/// Without a background, the mean of a row changes by less than this
/// across the bar's lower edge (the scrim's slope; glass there stepped by
/// some 20).
const BAR_EDGE: f64 = 4.0;
/// Live glass shows a window behind it: the test client's blue and white,
/// blurred, move a channel of the dock's glass by more than this; over the
/// fullscreen window, its red or green leads the other channels by as much.
const GLASS_SHOWS: u8 = 12;
/// Where the theme's arrow has its body and its resize arrows and text
/// cursor have nothing (Bibata Modern Classic at 24 px: all 30 pixels of
/// the arrow, none of the others): this many pixels right of and below the
/// pointer's hotspot. The pointer shows the arrow when most of them differ
/// from the screen without it.
const ARROW_BODY: (std::ops::Range<usize>, std::ops::Range<usize>) = (3..9, 12..17);
const ARROW_SHOWS: usize = 20;
/// The test client's icon (`CLIENT_ICON`): one colour, so the dock shows
/// it in the middle of its tile as it is.
const CLIENT_ICON_COLOR: [u8; 3] = [0xff, 0x80, 0x00];
/// The test client's desktop file and its icon in the owner's data
/// directory (XDG), as an app installed for one user brings them.
const CLIENT_ICON: &str = "d=~/.local/share; mkdir -p $d/applications \
     $d/icons/hicolor/scalable/apps && printf '%s\\n' '[Desktop Entry]' Type=Application \
     Name=beize-testclient Icon=beize-test-icon Exec=/run/t/beize-testclient \
     > $d/applications/beize-testclient.desktop && printf '%s' \
     '<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\"><rect width=\"16\" \
     height=\"16\" fill=\"#ff8000\"/></svg>' \
     > $d/icons/hicolor/scalable/apps/beize-test-icon.svg";

/// Where the time is: the right end of the bar, this wide. "00:00" in
/// Inter at 13 px is some 35 px; the rest of the bar is the background.
const CLOCK_WIDTH: usize = 80;
/// The time's least ink, in pixels far from the background beside it
/// (`INK_FROM`), white or dark: well below the narrowest time,
/// "11:11", and well above none.
const CLOCK_INK: usize = 20;
const INK_FROM: u8 = 0x40;

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
/// squares they lie in (radius 16, the continuous corner reaching 1.49
/// radii) are left out where windows are compared.
const CORNER: usize = 24;
/// How far the test drags the framed client's window by the compositor's
/// bar.
const FRAMED_MOVE_BY: (i32, i32) = (-120, 60);

/// The load test's scene (os/beize-ui, `beize-load-test`): its windows, how
/// long it runs, and how many frames each window must have drawn at least
/// – in the VM about 30 a second, on CI's runner fewer; fewer than two a
/// second would mean it stalls.
const LOAD_WINDOWS: usize = 12;
const LOAD_SECONDS: u64 = 15;
const LOAD_LEAST_FRAMES: usize = 30;
/// How far above the dock the test drags the scene's top window by its
/// bar: the window's content then lies behind the dock.
const LOAD_ABOVE_DOCK: usize = 150;

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
    // In one piece: the desktop showed once the shell said its panels were
    // drawn, not without them after the wait (a warning, which fails).
    let shown = wait_for_compositor_line(
        &shell,
        "desktop shown with the shell, [0-9]* ms after the screen",
    )?;
    say!("  ok: the desktop appeared with the shell's panels: {shown}");

    // Super+Shift+3 saves the screen as it shows it: the PNG equals QEMU's
    // own screendump of the still desktop.
    let size = screenshot(&shell, &machine)?;
    say!(
        "  ok: Super+Shift+3 saved a screenshot, {}x{}, as the screen shows it",
        size.0,
        size.1
    );

    // The desktop starts with no terminal, as macOS' with no window;
    // Ctrl+Alt+T opens one and another: each in a unit of its own, with the
    // owner's shell in it.
    let none = terminals(&shell)?;
    ensure!(
        none.is_empty(),
        "a terminal opened with the desktop: {none:?}"
    );
    keys(&qmp, &["ctrl+alt+t"])?;
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
        "  ok: the desktop started with no terminal, Ctrl+Alt+T opened one and another, each \
         with its shell; the dock shows the terminal running, the menu bar names it"
    );

    // The new terminal has the keyboard: typed into once its window is on
    // the screen (placed, it is active in the same step) and its shell
    // reads, the sample's command runs and the screen shows what it
    // printed. Typed before the window shows, as no one would, the first
    // keys went to the first terminal on a slow VM.
    let second = both
        .iter()
        .find(|u| **u != first[0])
        .context("no second terminal")?;
    let terminal = placed(&shell, "beize-terminal", 2)?;
    wait_for_prompt(&shell, second)?;
    keys(&qmp, &german_presses(TERMINAL_SAMPLE_COMMAND)?)?;
    wait_for_screen(paths, &machine, terminal, TERMINAL_SAMPLE, "terminal")?;
    say!(
        "  ok: the typed command ran in the terminal, its window shows the sample as the \
         reference has it"
    );
    // Over its text, the text cursor the terminal names.
    let ((x, y), (w, h)) = terminal;
    let middle = (i32::try_from(x + w / 2)?, i32::try_from(y + h / 2)?);
    drag(&qmp, screen_size(&machine)?, middle, middle, false)?;
    wait_for_compositor_line(&shell, "cursor text: 1 frame at 24 px")?;
    park(&qmp, screen_size(&machine)?)?;
    say!("  ok: over the terminal's text the pointer is the theme's text cursor");

    // A client started by the service manager, which knows the socket only
    // from beize-session; its desktop file names its icon.
    let (status, lines) = shell.capture(CLIENT_ICON)?;
    ensure!(
        status == 0,
        "could not write the test client's desktop file and icon: {lines:?}"
    );
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
    // On its right edge, the compositor's resize arrow.
    let edge = (
        i32::try_from(at.0 + size.0 + 2)?,
        i32::try_from(at.1 + size.1 / 2)?,
    );
    drag(&qmp, screen, edge, edge, false)?;
    wait_for_compositor_line(&shell, "cursor ew-resize: 1 frame at 24 px")?;
    // From the edge onto the client, which names no image: the arrow, not
    // the resize arrow it came with.
    let inside = (edge.0 - 60, edge.1);
    drag(&qmp, screen, edge, inside, false)?;
    let body = arrow_body(&machine, &qmp, screen, inside, "client-pointer")?;
    ensure!(
        body >= ARROW_SHOWS,
        "over the client, which names no pointer image, the pointer is not the arrow (its \
         body differs in {body} of 30 pixels); the screen is in the logs as client-pointer"
    );
    // The dock shows its icon from the hicolor theme (ADR 0028 §8).
    let apps = wait_for_dock(&shell, |apps| {
        apps.iter().any(|a| a == "beize-testclient running")
    })?;
    let tile = apps
        .iter()
        .position(|a| a == "beize-testclient running")
        .context("the test client left the dock")?;
    wait_for_dock_icon(&machine, dock_tile(screen, apps.len(), tile)?)?;
    say!(
        "  ok: the client's window opened in its own size, {}x{}, centred below the menu bar, \
         exactly its pattern; it is offered no panels; its edge shows the resize arrow, the \
         dock its icon",
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

    // Super+F (ADR 0028 §2): the window fills the whole screen, menu bar and
    // dock hidden. The pointer in the middle; at the top edge it shows the
    // menu bar on live glass (its ink dark, as on light glass), which takes
    // on the window's red and green above – base glass would show the
    // background –, at the bottom the dock. Super+F again, and the window
    // is back in its place.
    let middle = (screen.0 / 2, screen.1 / 2);
    let to = |(x, y): (usize, usize)| -> Result<(i32, i32)> {
        Ok((i32::try_from(x)?, i32::try_from(y)?))
    };
    drag(&qmp, screen, to(middle)?, to(middle)?, false)?;
    keys(&qmp, &["meta_l+f"])?;
    let difference = machine.logs.join("fullscreen-difference.ppm");
    wait_until_screen(&machine, "fullscreen", |shown| {
        // The pointer lies over the window there.
        let pointer = |x: usize, y: usize| x.abs_diff(middle.0) < 32 && y.abs_diff(middle.1) < 32;
        compare(
            shown,
            |x, y| {
                if pointer(x, y) {
                    shown.pixel(x, y)
                } else {
                    expected(x, y, screen.0, screen.1)
                }
            },
            "the fullscreen window",
            &difference,
        )
    })?;
    let bar_at = (middle.0 + 100, MENU_BAR / 2);
    // Below the bar's text, a quarter in from either side: the blur there
    // takes in one quadrant only.
    let red_at = (screen.0 / 4, MENU_BAR - 3);
    let green_at = (screen.0 * 3 / 4, MENU_BAR - 3);
    drag(&qmp, screen, to(middle)?, to((middle.0, 0))?, false)?;
    wait_until_screen(&machine, "fullscreen-menu-bar", |shown| {
        ensure!(
            !same(shown.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
            "the menu bar did not come at the top edge over the fullscreen window"
        );
        let (red, green) = (
            shown.pixel(red_at.0, red_at.1),
            shown.pixel(green_at.0, green_at.1),
        );
        ensure!(
            leads(red, 0) && leads(green, 1),
            "the menu bar's glass does not show the fullscreen window: {red:?} over its red, \
             {green:?} over its green"
        );
        Ok(())
    })?;
    wait_for_shell_line(&shell, |l| l == "menu bar on a Light background")?;
    let apps = last_dock(&shell)?.len();
    let tile = dock_tile(screen, apps, 0)?;
    let tile = (usize::try_from(tile.0)?, usize::try_from(tile.1)?);
    drag(
        &qmp,
        screen,
        to((middle.0, 0))?,
        to((middle.0, screen.1 - 1))?,
        false,
    )?;
    wait_until_screen(&machine, "fullscreen-dock", |shown| {
        ensure!(
            !same(
                shown.pixel(tile.0, tile.1),
                expected(tile.0, tile.1, screen.0, screen.1)
            ),
            "the dock did not come at the bottom edge over the fullscreen window"
        );
        ensure!(
            same(shown.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
            "the menu bar stayed over the fullscreen window with the pointer at the bottom"
        );
        Ok(())
    })?;
    drag(
        &qmp,
        screen,
        to((middle.0, screen.1 - 1))?,
        to(middle)?,
        false,
    )?;
    keys(&qmp, &["meta_l+f"])?;
    park(&qmp, screen)?;
    wait_for_pattern(&shell, &machine, moved, grown, "back from fullscreen")?;
    say!(
        "  ok: Super+F gave the window the whole screen; the menu bar came at the top edge on \
         live glass showing the window's colours, the dock at the bottom; again, and it was \
         back in its place"
    );

    // The dock's glass is live (ADR 0028 §6): dragged behind it, the window
    // shows through, blurred – base glass would stay as it was. Then back.
    let glass_at = (
        screen.0 / 2,
        screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE + 1,
    );
    let file = machine.logs.join("dock-glass-screen.ppm");
    screendump(&machine.qmp(), &file)?;
    let before = Image::read_ppm(&fs::read(&file)?)?.pixel(glass_at.0, glass_at.1);
    let grip = point(
        moved,
        (
            i32::try_from(grown.0 / 2)?,
            i32::try_from(CLIENT_TITLE / 2)?,
        ),
    )?;
    let behind = (
        i32::try_from(screen.0 / 2)? - i32::try_from(grown.0 / 2)? - i32::try_from(moved.0)?,
        i32::try_from(screen.1)? - i32::try_from(grown.1)? - i32::try_from(moved.1)?,
    );
    drag(&qmp, screen, grip, by(grip, behind), true)?;
    park(&qmp, screen)?;
    wait_for_live_glass(&machine, glass_at, before)?;
    drag(&qmp, screen, by(grip, behind), grip, true)?;
    park(&qmp, screen)?;
    wait_for_pattern(&shell, &machine, moved, grown, "back from the dock")?;
    say!("  ok: the dock's glass showed the window dragged behind it, blurred");

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
    // it once more and keeps the old one's panels until the new one is
    // ready (§6, a warning if it is not within the wait), the bar comes
    // back, the desktop and the window stay.
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
    wait_for_compositor_line(
        &shell,
        "the shell failed: its 2 panels stay until a new one is ready",
    )?;
    let replaced = wait_for_compositor_line(
        &shell,
        "a new shell is ready [0-9]* ms after the old one failed: its panels replace the kept ones",
    )?;
    wait_for_panels(&machine)?;
    wait_for_screen(
        paths,
        &machine,
        content(ui),
        UI_SAMPLE,
        "beize-ui-after-shell",
    )?;
    say!(
        "  ok: killed, the shell came back with its menu bar and dock, kept until then; the \
         desktop and the window stayed: {replaced}"
    );
    // Stopped, with no new shell yet, its panels are still on the screen
    // once the compositor has drawn without the shell – it said so, and
    // half a second of frames followed, well within the wait of seconds –,
    // and a shell started then replaces them.
    let (_, lines) = shell.capture(
        "k() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^the shell failed: '; }; systemctl --user stop beize-shell.service; \
         for i in $(seq 50); do [ \"$(k)\" -ge 2 ] && break; sleep 0.1; done; sleep 0.5; \
         echo BEIZE-KEPT-$(k); systemctl --user is-active beize-shell.service",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "BEIZE-KEPT-2"),
        "the shell did not stop, or the compositor did not keep its panels: {lines:?}"
    );
    let file = machine.logs.join("kept-screen.ppm");
    screendump(&qmp, &file)?;
    let kept = Image::read_ppm(&fs::read(&file)?)?;
    if let Err(e) = menu_bar(&kept).and_then(|()| dock(&kept).map(|_| ())) {
        let shown = machine.logs.join("kept-screen.png");
        kept.write_png(&shown)?;
        return Err(e.context(format!(
            "the stopped shell's panels did not stay; the screen is {}",
            shown.display()
        )));
    }
    let (_, lines) = shell.capture(
        "r() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^a new shell is ready '; }; systemctl --user start beize-shell.service; \
         for i in $(seq 50); do [ \"$(r)\" -ge 2 ] && break; sleep 0.2; done; \
         echo BEIZE-REPLACED-$(r)",
    )?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-REPLACED-2"),
        "a shell started after one stopped did not replace its kept panels: {lines:?}"
    );
    wait_for_panels(&machine)?;
    say!("  ok: stopped, the shell's panels stayed on the screen until a new one replaced them");

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
    load(&shell, &machine, &qmp, screen)?;

    // Over the second terminal where it covers only the desktop, its text
    // cursor; the terminals stop under the still pointer: the arrow.
    let ((x, y), (w, h)) = terminal;
    let over = (i32::try_from(x + w)? - 16, i32::try_from(y + h)? - 13);
    drag(&qmp, screen, over, over, false)?;
    thread::sleep(Duration::from_millis(500));
    // With no terminal left, the dock's first tile starts one.
    let (_, lines) = shell.capture("systemctl --user stop 'app-beize\\x2dterminal@*'")?;
    ensure!(
        terminals(&shell)?.is_empty(),
        "the terminals did not stop: {lines:?}"
    );
    let body = arrow_body(&machine, &qmp, screen, over, "closed-pointer")?;
    ensure!(
        body >= ARROW_SHOWS,
        "the terminal closed under the pointer, which still is not the arrow (its body \
         differs in {body} of 30 pixels); the screen is in the logs as closed-pointer"
    );
    say!(
        "  ok: over a client that names no image the pointer is the arrow, and the arrow \
         again where a window closed under it, without it moving"
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

/// The load test's scene (ADR 0028 §9): its top window, dragged by its bar
/// over the dock, lies half behind it, and the dock's live glass changes
/// with what moves there – base glass, or live glass blurred only once,
/// would stay. Every window draws throughout, the compositor sums the
/// frames up (`pacing:`, said for the record: lavapipe's times say nothing
/// of a GPU's).
fn load(shell: &Shell, machine: &Machine, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    const UNIT: &str = "beize-load-test";
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-load-test --windows {LOAD_WINDOWS} \
         --seconds {LOAD_SECONDS}"
    ))?;
    ensure!(
        status == 0,
        "could not start the load test (exit status {status}): {lines:?}"
    );
    let ((x, y), _) = placed(shell, UNIT, LOAD_WINDOWS)?;
    let grip = (
        i32::try_from(x + BAR_GRIP_X)?,
        i32::try_from(y + TITLE_BAR / 2)?,
    );
    let to = (
        i32::try_from(screen.0 / 2)?,
        i32::try_from(screen.1 - DOCK_ZONE - LOAD_ABOVE_DOCK)?,
    );
    drag(qmp, screen, grip, to, true)?;
    park(qmp, screen)?;
    // Settled behind the dock, then compared with itself a moment later.
    thread::sleep(Duration::from_secs(1));
    let glass_at = (
        screen.0 / 2,
        screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE + 1,
    );
    let file = machine.logs.join("load-glass-screen.ppm");
    screendump(qmp, &file)?;
    let before = Image::read_ppm(&fs::read(&file)?)?.pixel(glass_at.0, glass_at.1);
    wait_for_live_glass(machine, glass_at, before)?;

    let (_, lines) = shell.capture(&format!(
        "for i in $(seq {}); do systemctl --user -q is-active {UNIT} || break; sleep 0.2; \
         done; systemctl --user is-active {UNIT}; journalctl --user -u {UNIT} -o cat",
        (LOAD_SECONDS + 15) * 5
    ))?;
    let drawn: Vec<usize> = lines
        .iter()
        .filter_map(|l| {
            l.strip_prefix("window ")?
                .split_once(": ")?
                .1
                .split_once(' ')
        })
        .filter_map(|(frames, _)| frames.parse().ok())
        .collect();
    ensure!(
        lines.iter().any(|l| l.starts_with("load: "))
            && drawn.len() == LOAD_WINDOWS
            && drawn.iter().all(|&f| f >= LOAD_LEAST_FRAMES),
        "the load test's {LOAD_WINDOWS} windows did not each draw {LOAD_LEAST_FRAMES} frames \
         in {LOAD_SECONDS} s: {lines:?}"
    );
    // The compositor sums a run up a quarter second after its last frame.
    let (_, lines) = shell.capture(
        "sleep 1; journalctl --user -b -u beize-compositor.service -o cat \
         | grep '^pacing: ' | tail -n 1",
    )?;
    let pacing = lines
        .iter()
        .find(|l| l.starts_with("pacing: "))
        .context("the compositor summed up no frames")?;
    // The load's run, not an earlier one: as long as the scene, near enough.
    let seconds = pacing
        .split_once(" frames in ")
        .and_then(|(_, rest)| rest.split_once(" s")?.0.parse::<f64>().ok())
        .with_context(|| format!("no length in {pacing:?}"))?;
    ensure!(
        seconds >= (LOAD_SECONDS - 5) as f64,
        "the compositor's last run took {seconds} s, not the load test's {LOAD_SECONDS}: \
         {pacing}"
    );
    let least = drawn.iter().min().copied().unwrap_or(0);
    say!(
        "  ok: the load test's {LOAD_WINDOWS} windows each drew {least} frames or more in \
         {LOAD_SECONDS} s, the dock's live glass followed the one behind it; {pacing}"
    );
    Ok(())
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

/// Waits until the screen passes `check`; if it does not, the last screen
/// is `<name>-screen.png` in the logs.
fn wait_until_screen(
    machine: &Machine,
    name: &str,
    check: impl Fn(&Image) -> Result<()>,
) -> Result<()> {
    let file = machine.logs.join(format!("{name}-screen.ppm"));
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        thread::sleep(Duration::from_millis(500));
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        match check(&screen) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => {
                let shown = machine.logs.join(format!("{name}-screen.png"));
                screen.write_png(&shown)?;
                return Err(e.context(format!("the screen is {}", shown.display())));
            }
            Err(_) => {}
        }
    }
}

/// Whether two pixels are the same but rounding.
fn same(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter()
        .zip(b)
        .all(|(a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE)
}

/// Whether `channel` of a pixel on live glass leads the other two by what
/// a window behind it shows (`GLASS_SHOWS`).
fn leads(pixel: [u8; 3], channel: usize) -> bool {
    (0..3)
        .filter(|&c| c != channel)
        .all(|c| pixel[channel] >= pixel[c].saturating_add(GLASS_SHOWS))
}

/// Whether two pixels are as close as neighbours on soft glass.
fn soft(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= GLASS_SMOOTH)
}

/// The mean of all channels of row `y` from `xs.start` to `xs.end`.
fn row_mean(screen: &Image, y: usize, xs: std::ops::Range<usize>) -> f64 {
    let n = xs.len().max(1) as f64;
    let sum: u64 = xs.flat_map(|x| screen.pixel(x, y)).map(u64::from).sum();
    sum as f64 / (3.0 * n)
}

/// Whether `screen` has the menu bar: the soft background across the top
/// but the pointer, the app's name and the time, the time's ink at the
/// right end, and no background of its own – no step at its lower edge.
/// Which time it shows depends on the minute, which name the shell's
/// journal says; Beize UI's text itself is checked exactly with the
/// windows.
fn menu_bar(screen: &Image) -> Result<()> {
    ensure!(
        screen.width > CLOCK_WIDTH + APP_NAME_WIDTH && screen.height > MENU_BAR,
        "a screen of {}x{} has no room for the menu bar",
        screen.width,
        screen.height
    );
    let clock = screen.width - CLOCK_WIDTH;
    let mut other = 0;
    let mut ink = 0;
    for y in 0..MENU_BAR {
        for x in APP_NAME_WIDTH..screen.width {
            let shown = screen.pixel(x, y);
            if x >= clock {
                let beside = screen.pixel(clock - 1, y);
                ink += usize::from(
                    shown
                        .iter()
                        .zip(beside)
                        .any(|(&a, b)| a.abs_diff(b) > INK_FROM),
                );
            } else if !soft(shown, screen.pixel(x - 1, y)) {
                other += 1;
            }
        }
    }
    ensure!(
        other == 0,
        "{other} pixels of the menu bar's strip are not the soft background"
    );
    ensure!(
        ink >= CLOCK_INK,
        "the menu bar shows no time: {ink} pixels of ink at its right end"
    );
    // No background: what lies behind runs on across its lower edge.
    let row = |y| row_mean(screen, y, APP_NAME_WIDTH..clock);
    let edge = (row(MENU_BAR - 1) - row(MENU_BAR)).abs();
    ensure!(
        edge < BAR_EDGE,
        "the menu bar has a background: the screen steps by {edge:.1} at its lower edge"
    );
    Ok(())
}

/// The dock in `screen`, centred along the bottom: in its middle row (the
/// only rows without tiles lie in its round corners) the background runs
/// soft from the screen's left edge to the dock's rim, which steps away
/// from it; at its round corner the background shows. Its width says how
/// many apps it has; that number.
fn dock(screen: &Image) -> Result<usize> {
    ensure!(
        screen.height > DOCK_ZONE + MENU_BAR,
        "a screen {} high has no room for the dock",
        screen.height
    );
    let top = screen.height - DOCK_ZONE;
    let row = top + DOCK_HEIGHT / 2;
    let centre = screen.width / 2;
    // Rightwards over the soft background to the first step: the dock.
    let mut left = 1;
    while left < centre && soft(screen.pixel(left - 1, row), screen.pixel(left, row)) {
        left += 1;
    }
    ensure!(left < centre, "no dock along the bottom of the screen");
    let width = 2 * (centre - left);
    let step = (DOCK_TILE + DOCK_GAP) as f64;
    let tiles = (((width + DOCK_GAP) as f64 - (2 * DOCK_PADDING) as f64) / step).round();
    let tiles = tiles.max(0.0) as usize;
    let expected = 2 * DOCK_PADDING + tiles * DOCK_TILE + tiles.saturating_sub(1) * DOCK_GAP;
    ensure!(
        tiles >= 1 && width.abs_diff(expected) <= 1,
        "the dock is {width} wide, which is no number of apps"
    );
    // A round corner shows what is below, as the pixel beside the dock.
    ensure!(
        soft(screen.pixel(left, top), screen.pixel(left - 1, top)),
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

/// Waits until the compositor said `line` (a basic regular expression for
/// the whole line), in this boot; the last line it took. If it does not,
/// the error has what the compositor and the shell said last.
fn wait_for_compositor_line(shell: &Shell, line: &str) -> Result<String> {
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 50); do journalctl --user -b -u beize-compositor.service -o cat \
         | grep -qx '{line}' && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep -x '{line}' \
         | sed 's/^/BEIZE-SAID /' | tail -n 1"
    ))?;
    if let Some(said) = lines.iter().find_map(|l| l.strip_prefix("BEIZE-SAID ")) {
        return Ok(said.to_owned());
    }
    let (_, last) = shell.capture(
        "journalctl --user -b -u beize-compositor.service -u beize-shell.service \
         -o short-monotonic | grep -v ': pointer at ' | tail -n 30",
    )?;
    bail!(
        "the compositor did not say \"{line}\"; last said:\n{}",
        last.join("\n")
    )
}

/// Takes a screenshot with Super+Shift+3 of the still screen and compares
/// it with QEMU's screendump before and after (again if the clock's minute
/// changed in between); returns its size.
fn screenshot(shell: &Shell, machine: &Machine) -> Result<(usize, usize)> {
    let qmp = machine.qmp();
    park(&qmp, screen_size(machine)?)?;
    thread::sleep(Duration::from_secs(1));
    let dump = |name: &str| -> Result<Image> {
        let file = machine.logs.join(format!("{name}.ppm"));
        screendump(&qmp, &file)?;
        Image::read_ppm(&fs::read(&file)?)
    };
    let wanted = "screenshot [0-9]*x[0-9]*: .*[.]png";
    let mut last: Option<String> = None;
    let mut taken = None;
    for _ in 0..2 {
        let before = dump("screenshot-before")?;
        keys(&qmp, &["meta_l+shift+3"])?;
        // The compositor's newest line, not the one of the last try.
        let deadline = Instant::now() + SCREEN_TIMEOUT;
        let said = loop {
            let said = wait_for_compositor_line(shell, wanted)?;
            if last.as_ref() != Some(&said) {
                break said;
            }
            ensure!(Instant::now() < deadline, "no second screenshot");
            thread::sleep(Duration::from_millis(200));
        };
        let after = dump("screenshot-after")?;
        if before.pixels == after.pixels {
            taken = Some((said, after));
            break;
        }
        last = Some(said);
    }
    let (said, after) = taken.context("the screen changed while each screenshot was taken")?;
    let file = said
        .split_once(": ")
        .map(|(_, f)| f)
        .context("no file in the compositor's line")?;
    let (status, lines) = shell.capture(&format!("base64 -w 76 '{file}'"))?;
    ensure!(status == 0, "could not read {file}: {lines:?}");
    // The kernel's console may cut into a line ("[   35.67] rcu: …"): base64
    // has no `[`, the rest of the line is the kernel's; the PNG's checksums
    // catch anything else.
    let text: String = lines
        .iter()
        .map(|l| l.split('[').next().unwrap_or_default())
        .collect();
    let shot = Image::read_png(&base64(&text)?).context("reading the screenshot")?;
    ensure!(
        (shot.width, shot.height) == (after.width, after.height),
        "the screenshot is {}x{}, the screen {}x{}",
        shot.width,
        shot.height,
        after.width,
        after.height
    );
    shot.write_png(&machine.logs.join("screenshot.png"))?;
    compare(
        &after,
        |x, y| shot.pixel(x, y),
        "the screenshot",
        &machine.logs.join("screenshot-difference.ppm"),
    )?;
    Ok((shot.width, shot.height))
}

/// Decodes base64 (RFC 4648), as `base64` writes it.
fn base64(text: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Result<u32> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("not base64: {:?}", char::from(c)),
        }
        .into())
    };
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        let mut n = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Ok(out)
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
/// Waits until the dock shows the test client's icon in the middle of its
/// tile at `at`; if it does not, the last screen is in the logs.
/// How many pixels of the arrow's body (`ARROW_BODY`) show with the
/// pointer at `at`: the screen then against the screen with the pointer
/// parked. The pointer is drawn into the frame in the VM. Parks it.
fn arrow_body(
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    at: (i32, i32),
    name: &str,
) -> Result<usize> {
    let with = machine.logs.join(format!("{name}-screen.ppm"));
    let without = machine.logs.join(format!("{name}-parked.ppm"));
    // A frame for the pointer's last move.
    thread::sleep(Duration::from_millis(300));
    screendump(qmp, &with)?;
    park(qmp, screen)?;
    thread::sleep(Duration::from_millis(300));
    screendump(qmp, &without)?;
    let (a, b) = (
        Image::read_ppm(&fs::read(&with)?)?,
        Image::read_ppm(&fs::read(&without)?)?,
    );
    let (x, y) = (usize::try_from(at.0)?, usize::try_from(at.1)?);
    let mut differ = 0;
    for dy in ARROW_BODY.1.clone() {
        for dx in ARROW_BODY.0.clone() {
            if a.pixel(x + dx, y + dy) != b.pixel(x + dx, y + dy) {
                differ += 1;
            }
        }
    }
    if differ < ARROW_SHOWS {
        a.write_png(&machine.logs.join(format!("{name}-screen.png")))?;
    }
    Ok(differ)
}

/// Waits until the dock's glass at `at` shows something else than
/// `before`, by more than `GLASS_SHOWS` in a channel: a window behind it.
/// The screen then is in the logs as `live-glass-screen.png`.
fn wait_for_live_glass(machine: &Machine, at: (usize, usize), before: [u8; 3]) -> Result<()> {
    let file = machine.logs.join("live-glass-screen.ppm");
    let png = machine.logs.join("live-glass-screen.png");
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        let shown = screen.pixel(at.0, at.1);
        if shown
            .iter()
            .zip(before)
            .any(|(&a, b)| a.abs_diff(b) > GLASS_SHOWS)
        {
            screen.write_png(&png)?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            screen.write_png(&png)?;
            bail!(
                "the dock's glass shows {shown:?} at {},{} with a window behind it, as \
                 {before:?} without: it is not live; the screen is {}",
                at.0,
                at.1,
                png.display()
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_dock_icon(machine: &Machine, at: (i32, i32)) -> Result<()> {
    let file = machine.logs.join("dock-icon-screen.ppm");
    let (x, y) = (usize::try_from(at.0)?, usize::try_from(at.1)?);
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        let shown = screen.pixel(x, y);
        let near = shown
            .iter()
            .zip(CLIENT_ICON_COLOR)
            .all(|(&a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE);
        if near {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let png = machine.logs.join("dock-icon-screen.png");
            screen.write_png(&png)?;
            bail!(
                "the dock shows {shown:?} at {x},{y}, not the test client's icon {:?}; the \
                 screen is {}",
                CLIENT_ICON_COLOR,
                png.display()
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

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

    /// The background in the tests: a soft blue, darker than glass.
    const BACKGROUND: [u8; 3] = [0x80, 0x9a, 0xd0];

    /// Glass at `x`: lighter than the background, its colour drifting
    /// slowly as a blurred background's does.
    fn glass(x: usize) -> [u8; 3] {
        [0xb0 + (x / 40 % 4) as u8, 0xc8, 0xf0]
    }

    /// A screen of `width`×40 with the menu bar – no background of its
    /// own, ink for a time at its right end – and a pointer.
    fn desktop(width: usize) -> Image {
        let mut screen = Image {
            width,
            height: 40,
            pixels: BACKGROUND.repeat(width * 40),
        };
        let mut set = |x: usize, y: usize, c: [u8; 3]| {
            let at = (y * width + x) * 3;
            screen.pixels[at..at + 3].copy_from_slice(&c);
        };
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

    /// The background with a dock of `apps` tiles on glass, a rim of
    /// light along its edge, its corners round or not.
    fn with_dock(apps: usize, round: bool) -> Image {
        let (w, h) = (400, 200);
        let mut screen = Image {
            width: w,
            height: h,
            pixels: BACKGROUND.repeat(w * h),
        };
        let width = 2 * DOCK_PADDING + apps * DOCK_TILE + (apps - 1) * DOCK_GAP;
        let (x0, y0) = ((w - width) / 2, h - DOCK_ZONE);
        for y in y0..y0 + DOCK_HEIGHT {
            for x in x0..x0 + width {
                let corner = round && (x - x0).min(x0 + width - 1 - x) < 3 && y - y0 < 3;
                let rim = (x - x0).min(x0 + width - 1 - x) < 2 || y - y0 < 2;
                let c = if corner {
                    BACKGROUND
                } else if rim {
                    [0xe8, 0xf0, 0xff]
                } else {
                    glass(x)
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
            pixels: BACKGROUND.repeat(400 * 200),
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
        assert_eq!(window.pixel(0, 0), BACKGROUND);
        assert_eq!(window.pixel(13, 0), BACKGROUND);
        assert!(desktop(200).crop((151, MENU_BAR), (50, 1)).is_err());
        // A stray pixel on the bar, no time, a background of its own.
        let mut stray = desktop(400);
        stray.pixels[(5 * 400 + 300) * 3] = 0;
        assert!(menu_bar(&stray).is_err());
        let none = Image {
            pixels: BACKGROUND.repeat(400 * 40),
            ..desktop(400)
        };
        assert!(menu_bar(&none).is_err());
        let mut with_background = desktop(400);
        for x in 0..360 {
            for y in 0..MENU_BAR {
                let at = (y * 400 + x) * 3;
                with_background.pixels[at..at + 3].copy_from_slice(&glass(x));
            }
        }
        assert!(menu_bar(&with_background).is_err());
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
    fn decodes_base64() {
        assert_eq!(base64("QmVpemU=").unwrap(), b"Beize");
        assert_eq!(base64("QmVp").unwrap(), b"Bei");
        assert_eq!(base64("QmU=").unwrap(), b"Be");
        assert!(base64("Qm-p").is_err());
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
