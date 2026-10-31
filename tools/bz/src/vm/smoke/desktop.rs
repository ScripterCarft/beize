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
//! Unplugged, the screen goes dark and the window stays; plugged in again,
//! it shows the window in its place (ADR 0029).
//! Beize UI's test program shows its sample – text, colours, blending –
//! below its title bar exactly as it draws it offscreen, the reference in
//! the repository. Killed, the shell comes back, its panels kept until the
//! new one is ready, and the window stays. The
//! title bar moves the window, maximizes it with a double click and back,
//! minimizes it, and closes it; the dock brings it back. A client that asks the compositor for its frame
//! (xdg-decoration) gets the same title bar, its buttons the same to the
//! pixel, above its pattern, centred with it; the bar moves, maximizes and
//! closes its window as Beize UI's does. A client's viewport crops and
//! scales its buffer (viewporter); an app hands on activation with a token
//! of its focus, and a token of a focus gone activates nothing
//! (xdg-activation). Windows have round corners
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
//! Beize's mark opens the Beize menu on its glass (ADR 0028 §3), logind
//! lets the session sleep, restart and shut down (but where logind keeps
//! the serial console as the user's display, `menus`); Escape closes it, and so
//! does a click elsewhere (the popup's grab), and its press on the control
//! center's button opens that instead. The control center's light/dark tile
//! switches the appearance, which the compositor keeps (§5, §6).
//! Signing out stops the desktop with its shell and terminals and takes
//! its environment back.
//! Signed in again, with a short time: without input, the screen fades to
//! black, and a key takes the fade back; a window with an idle inhibitor
//! keeps it on; once that is gone, the screen fades and goes off – the
//! CRTC off – and a key turns it on with the desktop.
//!
//! The owner's shell on the serial console looks on: the same user, so the
//! same service manager as the login on tty1.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::Installed;
use crate::paths::Paths;
use crate::say;
use crate::vm::console::{
    Answers, OWNER_CONSOLE_CREDENTIALS, SMOKE_PASSWORD, SMOKE_USER, button, click, drag,
    german_presses, hold, keys, right_click, screendump, swipe,
};
use crate::vm::debug::Shell;
use crate::vm::machine::{DESKTOP_MEMORY_MIB, Machine, test_medium};

/// The test client's `--viewport` window: its size, and where its
/// quadrants split – the part shown starts a quarter into the red one,
/// scaled 1.5 times (os/beize-testclient, `VIEWPORT_SOURCE`). Scaling
/// blends the pixels this close to a split.
const VIEWPORT_SIZE: (usize, usize) = (480, 360);
const VIEWPORT_SPLIT: (usize, usize) = (360, 270);
const VIEWPORT_SEAM: usize = 1;

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

/// The menu bar along the top of the screen (os/beize-draw, `measures`);
/// windows lie below it.
const MENU_BAR: usize = 32;
/// Where the active app's name is: the left end of the bar, at most this
/// wide (the shell's journal says which name it shows, `SHELL_SAID`).
const APP_NAME_WIDTH: usize = 200;
// The pointer, parked on the bar, lies in that end too.
const _: () = assert!(APP_NAME_WIDTH >= POINTER_SIZE);
/// The dock (os/beize-shell, `dock`): this high, this far above the bottom
/// of the screen, its tiles this wide with this gap and padding; windows
/// stay above it, so the window area ends `DOCK_ZONE` above the bottom.
const DOCK_HEIGHT: usize = 90;
const DOCK_MARGIN: usize = 6;
const DOCK_TILE: usize = 70;
const DOCK_GAP: usize = 0;
const DOCK_PADDING: usize = 10;
const DOCK_ZONE: usize = DOCK_HEIGHT + DOCK_MARGIN;
/// The menu bar has no background (ADR 0028 §3), the dock lies on glass
/// (§6): what shows there is the background, the scrim over it, blurred
/// glass – no colour to compare with, but soft: no pixel further than this
/// from its neighbour in a channel (a gradient's slope, grain, dithering).
const GLASS_SMOOTH: u8 = 6;
/// Liquid glass's edge at its sides (`quad.frag`, `SIDE`): its outermost
/// pixel lies below the screen RIM_OUT px outside and the glass RIM_IN px
/// inside by at least RIM_BELOW (the mean of the channels; ~13 over the
/// light background, as Apple's dock's dark edge).
const RIM_OUT: usize = 3;
const RIM_IN: usize = 4;
const RIM_BELOW: f64 = 4.0;
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

/// Where the status is: the right end of the bar, this wide – the network's
/// and the control center's symbols (18 px, 22 px apart) and the date and
/// time ("Wed 30 Sep 23:59" in Inter at 15 px is some 125 px), some 220 px
/// with the margin; the rest of the bar is the background.
const CLOCK_WIDTH: usize = 280;
/// The time's least ink, in pixels far from the background beside it
/// (`INK_FROM`), white or dark: well below the narrowest time,
/// "11:11", and well above none.
const CLOCK_INK: usize = 20;
const INK_FROM: u8 = 0x40;

/// The compositor's warning that Smithay could not become DRM master
/// itself: it already is, through the device logind handed over; and that it
/// could not remove the mode of a screen taken anew (`screen_comes_back`):
/// with the CRTC off, it takes "no mode" for one it made and removes blob 0.
/// Both reported to Smithay (~/upstream/smithay/ISSUES.md). Every other
/// warning fails.
const KNOWN_WARNINGS: [&str; 2] = [
    "Unable to become drm master, assuming unprivileged mode",
    "Failed to destroy old mode property blob: No such file or directory (os error 2)",
];

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
/// The compositor's summaries of runs of frames, in this boot.
const PACING: &str = "journalctl --user -b -u beize-compositor.service -o cat | grep '^pacing: '";

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
const RESIZE_BY: (i32, i32) = (60, 30);

/// Beize UI's title bar along the top of its windows (os/beize-ui,
/// `titlebar`), and how far the test drags a window by it.
const TITLE_BAR: usize = 32;
/// How far the fast resize drags Beize UI's window out (`fast_resize`), a
/// pixel a millisecond, and how soon it has to settle after the button is
/// up: 20 ms measured (about a frame at the VM's 60 Hz); the bound leaves
/// room for a loaded host. Drawing every size, the window was still
/// hundreds of sizes behind (988 instead of 800 px wide) when it ended.
const FAST_RESIZE_BY: i32 = 240;
const FAST_RESIZE_SETTLES: Duration = Duration::from_millis(250);
// Its 800x632 window stays clear of the dock (`MOVE_BY`).
const UI_MOVE_BY: (i32, i32) = (-100, 10);
/// The bar's circles at its left end, 23 px apart from 9 px on, close
/// centred 16 px in (os/beize-draw, `titlebar`); compared from the end of
/// the round corner on.
const BUTTONS: (usize, usize) = (CORNER, 77);
const CLOSE_X: usize = 16;
const CIRCLE_STEP: usize = 23;
/// With the pointer over the circles, a pixel of each that its mark covers
/// (os/beize-draw, `titlebar`, the GPU computes them): where close's
/// strokes cross, on minimize's line (which covers 0.7 of it), in zoom's
/// upper triangle – with the circle's fill and the mark's colour.
const MARKS: [Mark; 3] = [
    Mark {
        at: (15, 15),
        fill: [0xff, 0x5f, 0x57],
        ink: [0x4d, 0x00, 0x00],
    },
    Mark {
        at: (39, 15),
        fill: [0xfe, 0xbc, 0x2e],
        ink: [0x99, 0x57, 0x00],
    },
    Mark {
        at: (60, 14),
        fill: [0x28, 0xc8, 0x40],
        ink: [0x00, 0x65, 0x00],
    },
];

struct Mark {
    at: (usize, usize),
    fill: [u8; 3],
    ink: [u8; 3],
}
/// A place on a bar away from its circles.
const BAR_GRIP_X: usize = 120;
/// The windows' round corners (os/beize-compositor, `corners`): the
/// squares they lie in (radius 16, the continuous corner reaching 1.49
/// radii) are left out where windows are compared.
const CORNER: usize = 24;
/// How far the test drags the framed client's window by the compositor's
/// bar.
const FRAMED_MOVE_BY: (i32, i32) = (-120, 60);

/// The time without input after which the screen fades (`screen_goes_off`;
/// the compositor's `BEIZE_SCREEN_OFF_AFTER`), and its fade to black
/// (os/beize-compositor, `idle::FADE`); how long a part of it may take
/// beyond that, in a VM beside others.
const SCREEN_OFF_AFTER: Duration = Duration::from_secs(3);
const SCREEN_FADE: Duration = Duration::from_secs(10);
const SCREEN_OFF_SLACK: Duration = Duration::from_secs(5);
/// How far into the fade the screen is looked at, and by how much the
/// middle of the desktop (the background: no window) is darker then, at
/// least, in the mean of its channels: 30 % black over a background of
/// some 150 takes 45.
const FADE_LOOKED_AT: Duration = Duration::from_secs(3);
const FADE_DARKER: f64 = 20.0;
/// How long a client draws with the screen off, and how many frames tell
/// the screen's pace, some 60 a second, from a hidden window's, one
/// (`HIDDEN_FRAMES`, os/beize-compositor/src/state.rs). No exact count: the
/// VM's vCPUs stall (#41), CI once saw 6 for the expected 5.
const OFF_DRAWING: Duration = Duration::from_secs(3);
const OFF_FRAMES_MOST: usize = 20;

/// The load test's scene (os/beize-ui, `beize-load-test`): its windows, how
/// long it runs, and how many frames each window must have drawn at least
/// – in the VM about 35 a second, on CI's runner fewer; fewer than two a
/// second would mean it stalls. Long enough for the drag behind the dock
/// and the glass's check; what it measures counts on hardware only, which
/// runs it longer (`measure/hw-test.sh`).
const LOAD_WINDOWS: usize = 12;
const LOAD_SECONDS: u64 = 6;
const LOAD_LEAST_FRAMES: usize = 2 * LOAD_SECONDS as usize;
/// How far above the dock the test drags the scene's top window by its
/// bar: the window's content then lies behind the dock.
const LOAD_ABOVE_DOCK: usize = 150;

/// How long a screen may take to become what its reference shows.
const SCREEN_TIMEOUT: Duration = Duration::from_secs(10);

/// By when the compositor has ended a run of frames after its last one:
/// 250 ms without a frame (`IDLE`, os/beize-compositor/src/pacing.rs),
/// looked for every 500 ms (`PACING_CHECK`, state.rs), with room.
const RUN_ENDED: Duration = Duration::from_secs(1);
/// How long the still desktop is watched for waking.
const IDLE_SECONDS: u64 = 3;

/// A failed check puts the session's journal into the console's log
/// (`build/work/logs/boot`): the machine it leaves keeps none of it – two
/// red runs on 27.10. could not be explained for that.
struct JournalOnFailure<'a> {
    shell: &'a Shell,
    passed: bool,
}

impl Drop for JournalOnFailure<'_> {
    fn drop(&mut self) {
        if !self.passed {
            let _ = self.shell.capture(
                "journalctl --user -b -o short-monotonic --no-pager | grep -v ': pointer at ' \
                 | tail -n 600 | sed 's/^/BEIZE-USER-JOURNAL /'; \
                 journalctl -b -p warning -o short-monotonic --no-pager | tail -n 100 \
                 | sed 's/^/BEIZE-WARNING /'",
            );
        }
    }
}

pub(super) fn desktop(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-desktop")?;
    machine.test_medium = test_medium(paths)?;
    // Drags land on the pixel (`console::drag`).
    machine.tablet = true;
    machine.memory_mib = DESKTOP_MEMORY_MIB;
    ensure!(
        machine.test_medium.is_some(),
        "no test medium: the test client is missing"
    );
    // run0 asks for the owner's password: to unplug the screen before the
    // login and plug it in after, to mount the test medium, four times in
    // `screen_comes_back`, twice in `layout_follows` and once in
    // `still_and_signed_out` (twice when a minute began meanwhile).
    let shell = Shell::boot(
        &machine,
        &Answers::passwords(&[SMOKE_PASSWORD; 11]),
        &OWNER_CONSOLE_CREDENTIALS,
        false,
    )?;
    let mut journal = JournalOnFailure {
        shell: &shell,
        passed: false,
    };
    let qmp = machine.qmp();

    sign_in(&shell, &machine, &qmp)?;
    let terminal = screenshot_and_terminals(paths, &shell, &machine, &qmp)?;
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
    let grip = point(
        at,
        (i32::try_from(size.0 / 2)?, i32::try_from(CLIENT_TITLE / 2)?),
    )?;
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

    screen_comes_back(&shell, &machine, moved, grown)?;
    snap(&shell, &machine, &qmp, screen, moved, grown)?;
    switcher(&shell, &machine, &qmp, screen)?;
    fullscreen(&shell, &machine, &qmp, screen, moved, grown)?;
    dock_glass(&shell, &machine, &qmp, screen, moved, grown)?;
    frames(&shell)?;
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

    shell_restarts(paths, &shell, &machine, &qmp, ui)?;
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
    // With the pointer over the circles they show their marks.
    let over = (
        i32::try_from(moved.0 + CLOSE_X + CIRCLE_STEP)?,
        i32::try_from(moved.1 + TITLE_BAR / 2)?,
    );
    drag(&qmp, screen, over, over, false)?;
    wait_until_screen(&machine, "beize-ui-marks", |shown| marks(shown, moved))?;
    park(&qmp, screen)?;
    fast_resize(&shell, &qmp, screen, (moved, (w, h)))?;
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
         journalctl --user -u beize-ui-test -o cat | grep -x done | tail -n 1",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "done"),
        "the close button did not end Beize UI's test program: {lines:?}"
    );
    say!(
        "  ok: Beize UI's title bar moved the window by exactly the drag, a double click \
         maximized it and back, its circles showed their marks under the pointer, the yellow \
         circle minimized it and the dock brought it back, its close button closed it"
    );

    framed(&shell, &machine, &qmp, screen, &ui_buttons)?;
    viewport(&shell, &machine)?;
    activation(&shell)?;
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

    menus(&shell, &machine, &qmp, screen)?;
    context_menus(&shell, &machine, &qmp, screen)?;
    confirmation(&shell, &machine, &qmp, screen)?;
    app_menus(&shell, &machine, &qmp, screen)?;
    layout_follows(&shell)?;
    still_and_signed_out(&shell, &qmp, screen)?;
    screen_goes_off(&shell, &machine, &qmp)?;
    journal.passed = true;
    drop(journal);
    shell.stop()?;
    remove_screens(&machine.logs)?;
    machine.remove()
}

/// The VM's screen, as the kernel and the compositor name its connector.
const VM_SCREEN: &str = "Virtual-1";

/// The screen goes and comes back (ADR 0029): unplugged, the compositor
/// lets it go – nothing on it, the CRTC off – and keeps the window; plugged
/// in again, it takes the screen anew and shows the window in its place.
/// The test unplugs it and plugs it in with `off` and `detect` on its
/// connector's status and a change of the GPU's udev device, the event a
/// hotplug sends: virtio-gpu sends none for that itself (the kernel's probe
/// fires one only for drivers that poll,
/// `drm_helper_probe_single_connector_modes`).
fn screen_comes_back(
    shell: &Shell,
    machine: &Machine,
    at: (usize, usize),
    size: (usize, usize),
) -> Result<()> {
    let restored = Image::read_ppm(&fs::read(machine.logs.join("restored-screen.ppm"))?)?;
    let shown = restored.crop(at, size)?;
    // Twice, the second time in the mode of the first (virtio-gpu has 75 Hz
    // until probed): the compositor draws the returned screen itself.
    for round in ["", "-again"] {
        set_screen(shell, "off")?;
        wait_for_compositor_line(shell, &format!("screen {VM_SCREEN} is gone"))?;
        wait_for_gone(machine, (at, size), &shown, &format!("screen-gone{round}"))?;
        set_screen(shell, "detect")?;
        wait_for_compositor_line(shell, &format!("screen {VM_SCREEN}: .*, it had none"))?;
        wait_for_pattern(shell, machine, at, size, &format!("screen-back{round}"))?;
    }
    say!(
        "  ok: unplugged, the screen went dark and the window stayed; plugged in, it came back \
         with the window in its place, twice"
    );
    Ok(())
}

/// Unplugs the VM's screen (`off`) or plugs it in (`detect`), as root
/// (`screen_comes_back` says how).
fn set_screen(shell: &Shell, status: &str) -> Result<()> {
    let (code, lines) = shell.capture(&format!(
        "run0 sh -c 'c=$(echo /sys/class/drm/card*-{VM_SCREEN}) && echo {status} > \
         $c/status && udevadm trigger --action=change $(dirname $(readlink -f $c))'"
    ))?;
    ensure!(
        code == 0,
        "could not set the screen {status} (exit status {code}): {lines:?}"
    );
    Ok(())
}

/// The owner signs in on tty1 and the desktop starts: a Wayland session, the
/// compositor and the shell sandboxed, the shell known as such, the desktop
/// shown in one piece with its panels; the test medium mounted. Without a
/// screen the compositor waits, and the desktop starts once one comes.
fn sign_in(shell: &Shell, machine: &Machine, qmp: &Path) -> Result<()> {
    // With no screen connected, the compositor starts and waits for one;
    // the desktop starts once it comes (ADR 0029).
    set_screen(shell, "off")?;
    // At the keyboard, as the owner would: name, password, beize-session,
    // each when the console has had time to ask.
    for (typed, pause) in [(SMOKE_USER, 1), (SMOKE_PASSWORD, 2), ("beize-session", 0)] {
        keys(qmp, &german_presses(typed)?)?;
        thread::sleep(Duration::from_secs(pause));
    }
    wait_for_compositor_line(shell, "no screen connected: the desktop starts once one is")?;
    let (_, lines) = shell.capture(
        "systemctl --user is-active beize-session.target beize-compositor.service \
         beize-shell.service",
    )?;
    ensure!(
        lines == ["inactive", "activating", "inactive"],
        "without a screen, the compositor should wait and the desktop not start yet \
         (target, compositor, shell): {lines:?}"
    );
    set_screen(shell, "detect")?;
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
    // The compositor: the owner's process, in its system call filter, with
    // room for its clients' buffers (open files; 1024 ran out, 0.5.105).
    let (_, lines) = shell.capture(
        "p=$(systemctl --user show -P MainPID beize-compositor.service); \
         echo BEIZE-OWNER-$(stat -c %U /proc/$p); \
         while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
         done < /proc/$p/status; \
         echo BEIZE-FILES-$(awk '/^Max open files/ {print $4}' /proc/$p/limits)",
    )?;
    ensure!(
        lines
            .iter()
            .any(|l| l == &format!("BEIZE-OWNER-{SMOKE_USER}"))
            && lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
        "the compositor does not run as the owner in its system call filter: {lines:?}"
    );
    ensure!(
        lines.iter().any(|l| l == "BEIZE-FILES-524288"),
        "the compositor may not open 524288 files (its clients' buffers): {lines:?}"
    );
    say!(
        "  ok: the login on tty1 started the desktop: a Wayland session, the compositor as the \
         owner, sandboxed, with room for 524288 open files"
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
    wait_for_panels(machine)?;
    say!(
        "  ok: the shell runs sandboxed, the compositor knows it, its menu bar shows the time, \
         its dock floats along the bottom with round corners"
    );
    // In one piece: the desktop showed once the shell said its panels were
    // drawn, not without them after the wait (a warning, which fails).
    let shown = wait_for_compositor_line(
        shell,
        "desktop shown with the shell, [0-9]* ms after the screen",
    )?;
    say!("  ok: the desktop appeared with the shell's panels: {shown}");

    Ok(())
}

/// A screenshot equals the screen; Ctrl+Alt+T opens terminals, the typed
/// sample shows as its reference has it, the pointer over its text is the
/// text cursor. The second terminal's window.
fn screenshot_and_terminals(
    paths: &Paths,
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
) -> Result<Window> {
    // Super+Shift+3 saves the screen as it shows it: the PNG equals QEMU's
    // own screendump of the still desktop.
    let size = screenshot(shell, machine)?;
    say!(
        "  ok: Super+Shift+3 saved a screenshot, {}x{}, as the screen shows it",
        size.0,
        size.1
    );

    // The desktop starts with no terminal, as macOS' with no window;
    // Ctrl+Alt+T opens one and another: each in a unit of its own, with the
    // owner's shell in it.
    let none = terminals(shell)?;
    ensure!(
        none.is_empty(),
        "a terminal opened with the desktop: {none:?}"
    );
    keys(qmp, &["ctrl+alt+t"])?;
    let first = wait_for_terminals(shell, 1)?;
    keys(qmp, &["ctrl+alt+t"])?;
    let both = wait_for_terminals(shell, 2)?;
    ensure!(
        both.contains(&first[0]),
        "the first terminal ended when the second opened: {both:?}"
    );
    wait_for_shell_line(shell, |l| l == "dock: beize-terminal running")?;
    wait_for_shell_line(shell, |l| l == "menu bar: Terminal")?;
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
    let terminal = placed(shell, "beize-terminal", 2)?;
    wait_for_prompt(shell, second)?;
    keys(qmp, &german_presses(TERMINAL_SAMPLE_COMMAND)?)?;
    wait_for_screen(paths, machine, terminal, TERMINAL_SAMPLE, "terminal")?;
    say!(
        "  ok: the typed command ran in the terminal, its window shows the sample as the \
         reference has it"
    );
    // Over its text, the text cursor the terminal names.
    let ((x, y), (w, h)) = terminal;
    let middle = (i32::try_from(x + w / 2)?, i32::try_from(y + h / 2)?);
    drag(qmp, screen_size(machine)?, middle, middle, false)?;
    wait_for_compositor_line(shell, "cursor text: 1 frame at 24 px")?;
    park(qmp, screen_size(machine)?)?;
    say!("  ok: over the terminal's text the pointer is the theme's text cursor");

    Ok(terminal)
}

/// Waits until the journal of the user's session has `line` at least `count`
/// times.
fn wait_for_said(shell: &Shell, line: &str, count: usize) -> Result<()> {
    let mut seen = 0;
    for _ in 0..50 {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -b -o cat | grep -c -x '{line}' | sed 's/^/BEIZE-COUNT /'"
        ))?;
        seen = lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .unwrap_or(0);
        if seen >= count {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("the session said {line:?} {seen} times, not {count}")
}

/// The Beize menu's row `i` of its commands, below the bar (os/beize-ui's
/// measures): its 6 px, rows of 30 and the line of 14 before the last.
fn beize_menu_row(beize: (i32, i32), rows: i32, line: bool) -> (i32, i32) {
    let y = i32::try_from(MENU_BAR).unwrap_or(32) + 6 + rows * 30 + 15 + if line { 14 } else { 0 };
    (beize.0 + 40, y)
}

/// Restart asks first (ADR 0028 §3). Escape says no. A click on a window takes
/// the keyboard, so Return there answers nothing (the review's case: typing
/// must not restart the computer); a click on the dialog gives it back. On
/// Log Out, `still_and_signed_out` says yes with Return.
fn confirmation(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let (beize, _) = bar_buttons(shell)?;
    let restart = beize_menu_row(beize, 1, false);
    let middle = (
        screen.0 / 2,
        MENU_BAR + (screen.1 - MENU_BAR - DOCK_ZONE) / 2,
    );
    let spot = |image: &Image| image.pixel(middle.0, middle.1);
    let before = spot(&self::screen(machine, "confirm-before")?);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    let ask = |n: usize| -> Result<()> {
        let opened = shell_count(shell, "BeizeMenu open")?;
        click(qmp, screen, beize, 1)?;
        wait_for_shell_count(shell, "BeizeMenu open", opened + 1)?;
        thread::sleep(Duration::from_millis(300));
        click(qmp, screen, restart, 1)?;
        wait_for_shell_count(shell, "asking: Power(Restart)", n)?;
        wait_until_screen(machine, "confirm-open", |shown| {
            let p = spot(shown);
            ensure!(
                brightness(p) > brightness(before) + 8,
                "no dialog on glass at {middle:?}: {p:?}, {before:?} before"
            );
            Ok(())
        })
    };
    ask(1)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "confirm")?.write_png(&machine.logs.join("confirm.png"))?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "asked: Power(Restart), no", 1)?;

    ask(2)?;
    // In the terminal, far from the dialog: Return goes there.
    click(
        qmp,
        screen,
        (40, i32::try_from(MENU_BAR + TITLE_BAR)? + 40),
        1,
    )?;
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["ret"])?;
    thread::sleep(Duration::from_millis(500));
    ensure!(
        shell_count(shell, "asked: Power(Restart), yes")? == 0
            && shell_count(shell, "asked: Power(Restart), no")? == 1,
        "Return with a window active answered the dialog"
    );
    // Above the buttons, on the dialog: the keyboard is its again.
    let on_dialog = (i32::try_from(middle.0)?, i32::try_from(middle.1)? - 40);
    click(qmp, screen, on_dialog, 1)?;
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "asked: Power(Restart), no", 2)?;
    park(qmp, screen)?;
    say!(
        "  ok: Restart asked first; Escape said no; with a window clicked Return did not answer \
         the dialog, and a click on it gave it the keyboard back"
    );
    Ok(())
}

/// The active terminal's menus in the bar (ADR 0028 §3): moving from one open
/// menu to the next opens that, as do the arrows; Select All reaches the app;
/// File's New Window and Close Window work. A shell started anew shows them
/// again: they are kept with the window.
fn app_menus(shell: &Shell, machine: &Machine, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let menus = |line: String| -> Result<Vec<(i32, i32)>> {
        line.strip_prefix("menu bar menus: ")
            .context("not the bar's menus")?
            .split(' ')
            .map(|p| {
                let (x, y) = p.split_once(',').context("not a place")?;
                Ok((x.parse()?, y.parse()?))
            })
            .collect()
    };
    let places = |least: usize| -> Result<Vec<(i32, i32)>> {
        let (_, lines) = shell.capture(
            "journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: ' \
             | tail -n 1",
        )?;
        let found = lines
            .iter()
            .find(|l| l.starts_with("menu bar menus: "))
            .cloned()
            .context("the shell named no menus")?;
        let found = menus(found)?;
        ensure!(
            found.len() >= least,
            "the bar has {} menus: {found:?}",
            found.len()
        );
        Ok(found)
    };
    // The Beize menu, the app's name, File, Edit, Window.
    let bar = places(5)?;
    self::screen(machine, "app-menus-bar")?.write_png(&machine.logs.join("app-menus-bar.png"))?;
    let (file, edit) = (bar[2], bar[3]);
    let row = |at: (i32, i32), i: i32| {
        (
            at.0 + 30,
            i32::try_from(MENU_BAR).unwrap_or(32) + 6 + i * 30 + 15,
        )
    };
    let edit_open = "AppMenu(1) open";
    let n = shell_count(shell, edit_open)?;
    click(qmp, screen, edit, 1)?;
    wait_for_shell_count(shell, edit_open, n + 1)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "app-menu")?.write_png(&machine.logs.join("app-menu.png"))?;
    // Onto File's title: File opens, Edit closes.
    let m = shell_count(shell, "AppMenu(0) open")?;
    drag(qmp, screen, edit, file, false)?;
    wait_for_shell_count(shell, "AppMenu(0) open", m + 1)?;
    // The right arrow: Edit again.
    keys(qmp, &["right"])?;
    wait_for_shell_count(shell, edit_open, n + 2)?;
    // Select All, its third row, reaches the app.
    let selected = 1 + usize::from(true);
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(edit, 2), 1)?;
    wait_for_said(shell, "menu: SelectAll", selected)?;
    // New Window, then Close Window on that new window.
    let count = terminals(shell)?.len();
    let n = shell_count(shell, "AppMenu(0) open")?;
    click(qmp, screen, file, 1)?;
    wait_for_shell_count(shell, "AppMenu(0) open", n + 1)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(file, 0), 1)?;
    wait_for_terminals(shell, count + 1)?;
    thread::sleep(Duration::from_millis(500));
    let file_now = places(5)?[2];
    let n = shell_count(shell, "AppMenu(0) open")?;
    click(qmp, screen, file_now, 1)?;
    wait_for_shell_count(shell, "AppMenu(0) open", n + 1)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(file_now, 1), 1)?;
    wait_for_terminals(shell, count)?;
    park(qmp, screen)?;
    say!(
        "  ok: the terminal's menus showed in the bar; moving and the arrows went from menu to \
         menu; Select All, New Window and Close Window reached the app"
    );

    // A new shell hears them again.
    let said = |lines: Vec<String>| {
        lines
            .into_iter()
            .filter(|l| l.starts_with("menu bar menus: "))
            .count()
    };
    let (_, before) = shell
        .capture("journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: '")?;
    let before = said(before);
    shell.capture("systemctl --user kill -s KILL beize-shell.service")?;
    let mut now = before;
    for _ in 0..50 {
        let (_, lines) = shell.capture(
            "journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: '",
        )?;
        now = said(lines);
        if now > before && places(5).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    ensure!(
        now > before && places(5).is_ok(),
        "a shell started anew did not show the app's menus"
    );
    say!("  ok: a shell started anew showed the terminal's menus again");
    Ok(())
}

/// Context menus (ADR 0028 §3, §4): a right click in the terminal opens a
/// menu on the compositor's glass, over the dark terminal light as a menu is;
/// Escape closes it. A disabled command chooses nothing; with something
/// selected it does. A drag on its title bar closes the menu and moves the
/// window, as beside the shell's. The dock's tile has a menu above it: New
/// Window opens one, and Alt+F4 closes it.
fn context_menus(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    // Maximized, as the control center's test left it.
    let (x, y, w) = (0, i32::try_from(MENU_BAR)?, i32::try_from(screen.0)?);
    let at = (x + 200, y + i32::try_from(TITLE_BAR)? + 200);
    // A row of the menu, by the entries' places in os/beize-ui's measures.
    let row = |i: i32| (at.0 + 60, at.1 + 6 + i * 30 + 15);
    let inside = (at.0 + 180, at.1 + 6 + 30 + 15);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    let spot = |image: &Image| image.pixel(inside.0 as usize, inside.1 as usize);
    let before = spot(&self::screen(machine, "context-before")?);
    ensure!(
        brightness(before) < WINDOW_DARK,
        "the terminal is not at {inside:?} ({before:?}): the check would prove nothing"
    );
    right_click(qmp, screen, at)?;
    wait_until_screen(machine, "context-open", |shown| {
        let p = spot(shown);
        ensure!(
            brightness(p) > brightness(before) + 8,
            "no menu on glass over the terminal: {p:?}, {before:?} before"
        );
        Ok(())
    })?;
    self::screen(machine, "context-menu")?.write_png(&machine.logs.join("context-menu.png"))?;
    // Copy with nothing selected: nothing; the menu stays.
    click(qmp, screen, row(0), 1)?;
    thread::sleep(Duration::from_millis(300));
    ensure!(
        brightness(spot(&self::screen(machine, "context-disabled")?)) > brightness(before) + 8,
        "a disabled command closed the menu"
    );
    keys(qmp, &["esc"])?;
    wait_until_screen(machine, "context-closed", |shown| {
        ensure!(same(spot(shown), before), "Escape left the menu open");
        Ok(())
    })?;
    // Select All, then Copy is enabled.
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(2), 1)?;
    wait_for_said(shell, "menu: SelectAll", 1)?;
    park(qmp, screen)?;
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(0), 1)?;
    wait_for_said(shell, "menu: Copy", 1)?;
    // With the menu open, a drag on the title bar closes it and moves the
    // window (`press_ends_popup_grab`), off its maximized place; maximized
    // again after.
    let grip = (x + w / 2, y + i32::try_from(TITLE_BAR / 2)?);
    let moved = by(grip, (0, 60));
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    let over_bar = (x + w - 40, grip.1);
    let was = self::screen(machine, "context-bar-before")?
        .pixel(usize::try_from(over_bar.0)?, usize::try_from(over_bar.1)?);
    drag(qmp, screen, grip, moved, true)?;
    park(qmp, screen)?;
    wait_until_screen(machine, "context-dragged", |shown| {
        let p = shown.pixel(usize::try_from(over_bar.0)?, usize::try_from(over_bar.1)?);
        ensure!(
            !same(p, was),
            "the window did not move with the drag that closed its menu: {p:?} where its title bar was"
        );
        Ok(())
    })?;
    keys(qmp, &["meta_l+up"])?;
    say!(
        "  ok: a right click opened the terminal's menu on glass; a disabled command did nothing, \
         Escape closed it, with a selection Copy chose; a drag on the title bar closed it and \
         moved the window"
    );

    // The dock's tile: New Window, then Alt+F4 closes that window.
    let count = terminals(shell)?.len();
    let apps = last_dock(shell)?;
    let tile = dock_tile(screen, apps.len(), 0)?;
    right_click(qmp, screen, tile)?;
    wait_for_shell_count(shell, "DockMenu open", 1)?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "DockMenu closed", 1)?;
    right_click(qmp, screen, tile)?;
    wait_for_shell_count(shell, "DockMenu open", 2)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "dock-menu")?.write_png(&machine.logs.join("dock-menu.png"))?;
    // Above the tile: its window, a line, New Window, Quit.
    // From the tile's top, the panel's padding above it: Quit, then New Window.
    let new_window = (tile.0 + 40, tile.1 - 28 - 6 - 30 - 15);
    click(qmp, screen, new_window, 1)?;
    wait_for_shell_count(shell, "DockMenu closed", 2)?;
    wait_for_terminals(shell, count + 1)?;
    keys(qmp, &["alt+f4"])?;
    wait_for_terminals(shell, count)?;
    park(qmp, screen)?;
    say!("  ok: the dock's tile opened its menu, New Window a window, and Alt+F4 closed it");
    Ok(())
}

/// Alt+Tab (ADR 0028 §7), with the client's window in front of the two
/// terminals: a tap goes to the window before and back, whatever shows. Held,
/// the switcher shows the client's live picture in its first tile; Escape
/// changes nothing; Tab, Tab, Shift+Tab selects, the release of Alt chooses; a
/// click on a tile chooses.
fn switcher(shell: &Shell, machine: &Machine, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let compositor_count = |line: &str| -> Result<usize> {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -b -u beize-compositor.service -o cat | grep -c -x '{line}' \
             | sed 's/^/BEIZE-COUNT /'"
        ))?;
        Ok(lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .unwrap_or(0))
    };
    let chose = |app: &str| format!("switcher chose {app}");
    let tap = |app: &str| -> Result<()> {
        let before = compositor_count(&chose(app))?;
        keys(qmp, &["alt+tab"])?;
        wait_for_said(shell, &chose(app), before + 1)
    };
    tap("beize-terminal")?;
    tap("beize-testclient")?;
    say!("  ok: Alt+Tab went to the window used before, and again back");

    // Held: shown, the client's picture in the first tile.
    let shown = compositor_count("switcher shown")?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab"])?;
    wait_for_said(shell, "switcher shown", shown + 1)?;
    let line = wait_for_shell_line(shell, |l| l.starts_with("switcher: "))?;
    // "switcher: WxH with N windows, the first picture at Some((x, y, w, h))"
    let numbers: Vec<f64> = line
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|n| n.parse().ok())
        .collect();
    ensure!(numbers.len() == 7, "the switcher said {line:?}");
    let (w, h) = (numbers[0] as usize, numbers[1] as usize);
    ensure!(
        numbers[2] == 3.0,
        "the switcher has not 3 windows: {line:?}"
    );
    let area_h = screen.1 - MENU_BAR - DOCK_ZONE;
    let (left, top) = ((screen.0 - w) / 2, MENU_BAR + (area_h - h) / 2);
    let picture = (
        left + numbers[3] as usize,
        top + numbers[4] as usize,
        numbers[5] as usize,
        numbers[6] as usize,
    );
    let (cx, cy) = (picture.0 + picture.2 / 2, picture.1 + picture.3 / 2);
    // Two thirds out from the middle of the picture lie the client's four
    // colours; the window's own height keeps them inside however it fits.
    let (dx, dy) = (picture.2 / 8, picture.3 / 8);
    wait_until_screen(machine, "switcher", |shown| {
        for (at, colour) in [
            ((cx - dx, cy - dy), QUADRANTS[0]),
            ((cx + dx, cy - dy), QUADRANTS[1]),
            ((cx - dx, cy + dy), QUADRANTS[2]),
            ((cx + dx, cy + dy), QUADRANTS[3]),
        ] {
            let p = shown.pixel(at.0, at.1);
            ensure!(
                same(p, colour),
                "the switcher's first tile does not show the client: {p:?} at {at:?}, not {colour:?}"
            );
        }
        Ok(())
    })?;
    self::screen(machine, "switcher-open")?.write_png(&machine.logs.join("switcher.png"))?;
    // Escape: nothing chosen, nothing raised.
    let chosen = compositor_count(&chose("beize-terminal"))?;
    keys(qmp, &["esc"])?;
    wait_for_said(shell, "switcher hidden", shown + 1)?;
    hold(qmp, "alt", false)?;
    thread::sleep(Duration::from_millis(300));
    ensure!(
        compositor_count(&chose("beize-terminal"))? == chosen,
        "Escape chose a window"
    );
    // Tab, Tab, Shift+Tab: on the second window; Alt let go chooses it.
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab", "tab", "shift+tab"])?;
    hold(qmp, "alt", false)?;
    wait_for_said(shell, &chose("beize-terminal"), chosen + 1)?;
    tap("beize-testclient")?;
    // A click on the last tile chooses its terminal.
    let shown = compositor_count("switcher shown")?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab"])?;
    wait_for_said(shell, "switcher shown", shown + 1)?;
    thread::sleep(Duration::from_millis(300));
    let step = picture.2 + 16 + 10;
    let tile = (i32::try_from(cx + 2 * step)?, i32::try_from(cy)?);
    let chosen = compositor_count(&chose("beize-terminal"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_said(shell, &chose("beize-terminal"), chosen + 1)?;
    hold(qmp, "alt", false)?;
    park(qmp, screen)?;
    tap("beize-testclient")?;
    say!(
        "  ok: held, the switcher showed the client's live picture in its first tile; Escape \
         chose nothing, the release of Alt the selected window, a click its tile"
    );
    Ok(())
}

/// Snap (ADR 0028 §2, §7), on the client's window at `moved`, `grown`:
/// Super+arrows take the halves, maximize and go back, and the opposite
/// arrow from a half goes back to where it was. Held on the screen's edge in
/// a drag a preview shows with round corners, and the release takes the half;
/// dragged off, the window is its size from before under the pointer; into
/// the menu bar's strip it is maximized.
fn snap(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
    let area_h = screen.1 - MENU_BAR - DOCK_ZONE;
    let left = ((0, MENU_BAR), (screen.0 / 2, area_h));
    let right = ((screen.0 / 2, MENU_BAR), (screen.0 - screen.0 / 2, area_h));
    let whole = ((0, MENU_BAR), (screen.0, area_h));
    let steps: [(&str, Window, &str); 7] = [
        ("meta_l+left", left, "snap-left"),
        ("meta_l+right", (moved, grown), "snap-back"),
        ("meta_l+right", right, "snap-right"),
        ("meta_l+up", whole, "snap-maximized"),
        ("meta_l+left", left, "snap-left-maximized"),
        ("meta_l+down", (moved, grown), "snap-down"),
        ("meta_l+down", (moved, grown), "snap-down-again"),
    ];
    for (key, (at, size), name) in steps {
        keys(qmp, &[key])?;
        wait_for_pattern(shell, machine, at, size, name)?;
    }
    say!(
        "  ok: Super+arrows took each half, from the other half went back to the place from \
         before, maximized, from maximized took a half and went back"
    );

    // The drag: over the title strip, to the left edge, and held there.
    let grip = (
        i32::try_from(moved.0 + grown.0 / 2)?,
        i32::try_from(moved.1 + CLIENT_TITLE / 2)?,
    );
    // High in the left half, far above the window and its shadow.
    let inside = (screen.0 / 4, MENU_BAR + 40);
    let corner = (1, MENU_BAR + 1);
    let before = self::screen(machine, "snap-before")?;
    let edge = (0, grip.1);
    button(qmp, screen, grip, true)?;
    drag(qmp, screen, grip, edge, false)?;
    wait_until_screen(machine, "snap-preview", |shown| {
        let (lit, was) = (
            shown.pixel(inside.0, inside.1),
            before.pixel(inside.0, inside.1),
        );
        ensure!(
            lit.iter().zip(was).all(|(&l, w)| l > w),
            "no preview at {inside:?} with the pointer on the screen's edge: {lit:?}, {was:?} before"
        );
        let (tip, was) = (
            shown.pixel(corner.0, corner.1),
            before.pixel(corner.0, corner.1),
        );
        ensure!(
            same(tip, was),
            "the preview's corner is not round: {tip:?} at {corner:?}, {was:?} before"
        );
        Ok(())
    })?;
    // Off the edge again it goes, and the release leaves the window floating.
    let off = (120, grip.1);
    drag(qmp, screen, edge, off, false)?;
    wait_until_screen(machine, "snap-preview-gone", |shown| {
        let (now, was) = (
            shown.pixel(inside.0, inside.1),
            before.pixel(inside.0, inside.1),
        );
        ensure!(
            same(now, was),
            "the preview stayed: {now:?}, {was:?} before"
        );
        Ok(())
    })?;
    drag(qmp, screen, off, edge, false)?;
    button(qmp, screen, edge, false)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, left.0, left.1, "snap-dragged")?;
    // Off the half by its title strip: the size from before, under the pointer.
    let grip = (
        i32::try_from(left.1.0 / 2)?,
        i32::try_from(left.0.1 + CLIENT_TITLE / 2)?,
    );
    // Where all of it stays clear of the dock.
    let middle = (i32::try_from(screen.0 / 2)?, i32::try_from(MENU_BAR + 100)?);
    drag(qmp, screen, grip, middle, true)?;
    park(qmp, screen)?;
    // As far across it as the pointer was across the half: its middle.
    let centred = (
        screen.0 / 2 - grown.0 / 2,
        MENU_BAR + 100 - CLIENT_TITLE / 2,
    );
    wait_for_pattern(shell, machine, centred, grown, "snap-off")?;
    // Into the menu bar's strip: maximized; down, back to where that drag began.
    drag(qmp, screen, middle, (middle.0, 4), true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, whole.0, whole.1, "snap-top")?;
    keys(qmp, &["meta_l+down"])?;
    wait_for_pattern(shell, machine, centred, grown, "snap-top-back")?;
    // Back to where the test goes on from, by its strip.
    let home = (
        i32::try_from(moved.0 + grown.0 / 2)?,
        i32::try_from(moved.1 + CLIENT_TITLE / 2)?,
    );
    drag(qmp, screen, middle, home, true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "snap-end")?;
    say!(
        "  ok: on the screen's edge in a drag a round preview showed and went with the pointer, \
         the release took the half; dragged off, the window had its size from before; into the \
         menu bar's strip it was maximized"
    );
    Ok(())
}

/// Super+F gives the client's window (at `moved`, `grown`) the whole screen,
/// the panels come at its edges, and again, it is back.
fn fullscreen(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
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
    drag(qmp, screen, to(middle)?, to(middle)?, false)?;
    keys(qmp, &["meta_l+f"])?;
    let difference = machine.logs.join("fullscreen-difference.ppm");
    wait_until_screen(machine, "fullscreen", |shown| {
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
    drag(qmp, screen, to(middle)?, to((middle.0, 0))?, false)?;
    wait_until_screen(machine, "fullscreen-menu-bar", |shown| {
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
    wait_for_shell_line(shell, |l| l == "menu bar on a Light background")?;
    let apps = last_dock(shell)?.len();
    let tile = dock_tile(screen, apps, 0)?;
    let tile = (usize::try_from(tile.0)?, usize::try_from(tile.1)?);
    drag(
        qmp,
        screen,
        to((middle.0, 0))?,
        to((middle.0, screen.1 - 1))?,
        false,
    )?;
    wait_until_screen(machine, "fullscreen-dock", |shown| {
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
        qmp,
        screen,
        to((middle.0, screen.1 - 1))?,
        to(middle)?,
        false,
    )?;
    keys(qmp, &["meta_l+f"])?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "back from fullscreen")?;
    say!(
        "  ok: Super+F gave the window the whole screen; the menu bar came at the top edge on \
         live glass showing the window's colours, the dock at the bottom; again, and it was \
         back in its place"
    );

    Ok(())
}

/// The dock's live glass shows the client's window dragged behind it.
fn dock_glass(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
    // The dock's glass is live (ADR 0028 §6): dragged behind it, the window
    // shows through, blurred – base glass would stay as it was. Then back.
    // Left of the middle, over the window's blue quarter: in the middle its
    // blue and white blur into about what the background was there; between
    // the dots of the dock's two apps.
    let glass_at = (
        screen.0 / 2 - 20,
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
    drag(qmp, screen, grip, by(grip, behind), true)?;
    park(qmp, screen)?;
    wait_for_live_glass(machine, glass_at, before)?;
    // For the owner: the dock over the window's blue and white quarters.
    self::screen(machine, "dock-over-window")?
        .write_png(&machine.logs.join("dock-over-window.png"))?;
    // For comparing glass before and after a change to its rendering: the
    // window's hard edge beside the dock, within and past the blur's reach
    // (30 px, ADR 0028 §6). The dock of two apps ends 80 px from the middle.
    let mut there = by(grip, behind);
    for (name, past) in [("dock-edge-20", 20), ("dock-edge-36", 36)] {
        // Its left edge lay half its width left of the middle.
        let next = (
            by(grip, behind).0 + i32::try_from(80 + past + grown.0 / 2)?,
            there.1,
        );
        drag(qmp, screen, there, next, true)?;
        there = next;
        park(qmp, screen)?;
        thread::sleep(Duration::from_secs(1));
        self::screen(machine, name)?.write_png(&machine.logs.join(format!("{name}.png")))?;
    }
    drag(qmp, screen, there, grip, true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "back from the dock")?;
    // With the window gone again it shows what it showed before: it blurs
    // what lies behind it, never what the screen held there – itself and
    // the dock's icons, tinted again with each blur.
    wait_for_glass_back(machine, glass_at, before)?;
    say!(
        "  ok: the dock's glass showed the window dragged behind it, blurred, and what it showed \
         before once it was gone"
    );

    Ok(())
}

/// The bar's menus (ADR 0028 §3, §5). The Beize menu opens on its glass –
/// light where the background showed before –, and logind lets the session
/// act; Escape closes it (the keyboard is the menu's), a click elsewhere
/// does too (the pointer is), and with it open a press on the control
/// center's button opens that. Its light/dark tile switches the appearance
/// and back; the compositor keeps the choice. With the menu open, a drag
/// on a window's title bar closes it and moves the window (owner, 28.10.:
/// the press went to the menu's grab, the move was refused). The release of
/// an Escape that closed the menu leaves the control center open. The screens of
/// the open
/// menus are in the logs for the owner (`beize-menu.png`,
/// `control-center-*.png`).
fn menus(shell: &Shell, machine: &Machine, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let (beize, centre) = bar_buttons(shell)?;
    // Inside the menu, below its first row's text.
    let inside = (usize::try_from(beize.0)? + 40, MENU_BAR + 10);
    let before = self::screen(machine, "menu-before")?.pixel(inside.0, inside.1);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    ensure!(
        brightness(before) < MENU_LIGHT,
        "the background at {},{} is {before:?}, as light as a menu: the check would prove nothing",
        inside.0,
        inside.1
    );
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 1)?;
    let may = wait_for_shell_line(shell, |l| l.starts_with("Beize menu may: "))?;
    let allowed = may.contains("restart yes") && may.contains("shut down yes");
    // polkit takes the user's display session for the shell, which runs
    // outside any session (sd_uid_get_display). logind elects it when a
    // session starts, not when beize-session makes tty1 a Wayland session
    // (SetType): the serial console, signed in first, stays the display –
    // without a seat, not local to polkit, which asks for a password. The
    // fix is upstream's to take (~/upstream/systemd-logind-display, sent by
    // the owner); until then exactly that case is said, any other fails.
    let (_, said) = shell.capture(
        "d=$(loginctl show-user $(id -u) -p Display --value); loginctl show-session $d \
         -p TTY -p Seat | paste -sd ' ' | sed 's/^/BEIZE-SAID /'",
    )?;
    let display = said
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SAID "))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let fields: Vec<&str> = display.split_whitespace().collect();
    let upstream = fields.contains(&"TTY=ttyS0") && fields.contains(&"Seat=");
    ensure!(
        allowed || upstream,
        "logind does not let the session act: {may} (display session: {display})"
    );
    if !allowed {
        say!(
            "  known: logind keeps the serial console as the display ({display}), polkit asks \
             for a password ({may}) – logind's SetType elects no display, upstream patch \
             prepared"
        );
    }
    wait_until_screen(machine, "beize-menu", |shown| {
        let p = shown.pixel(inside.0, inside.1);
        ensure!(
            brightness(p) >= MENU_LIGHT,
            "the menu at {},{} shows {p:?}, not its light glass",
            inside.0,
            inside.1
        );
        Ok(())
    })?;
    self::screen(machine, "beize-menu")?.write_png(&machine.logs.join("beize-menu.png"))?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "BeizeMenu closed", 1)?;
    // Elsewhere: the screen's middle (the terminal), far from the shell.
    let elsewhere = (i32::try_from(screen.0 / 2)?, i32::try_from(screen.1 / 2)?);
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 2)?;
    click(qmp, screen, elsewhere, 1)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 2)?;
    say!(
        "  ok: the Beize menu opened on its glass, logind answered ({}), and Escape or a click \
         elsewhere closed it",
        may.trim_start_matches("Beize menu may: ")
    );

    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 3)?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 3)?;
    wait_for_shell_count(shell, "ControlCenter open", 1)?;
    // The light/dark tile: the second row's first, below the bar and a gap
    // (os/beize-shell/src/control_center.rs).
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let tile = (
        i32::try_from(left + CONTROL_CENTER_TILE / 2)?,
        i32::try_from(
            MENU_BAR
                + CONTROL_CENTER_GAP
                + CONTROL_CENTER_TILE
                + CONTROL_CENTER_GAP
                + CONTROL_CENTER_TILE / 2,
        )?,
    );
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "control-center")?
        .write_png(&machine.logs.join("control-center-light.png"))?;
    let light = left_mean(&self::screen(machine, "light")?);
    click(qmp, screen, tile, 1)?;
    wait_for_compositor_line(shell, "appearance Dark, chosen")?;
    wait_for_shell_line(shell, |l| l == "appearance Dark")?;
    let kept = keep_says(shell)?;
    ensure!(kept == "dark", "the compositor kept {kept:?}, not dark");
    wait_until_screen(machine, "dark", |shown| {
        let dark = left_mean(shown);
        ensure!(
            dark < light,
            "the background is no darker in dark ({dark:.0}) than in light ({light:.0})"
        );
        Ok(())
    })?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "control-center")?
        .write_png(&machine.logs.join("control-center-dark.png"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_compositor_line(shell, "appearance Light, chosen")?;
    let kept = keep_says(shell)?;
    ensure!(kept == "light", "the compositor kept {kept:?}, not light");
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 1)?;
    say!(
        "  ok: a press on the control center's button closed the Beize menu and opened it; its \
         tile switched to dark and back, and the compositor kept the choice"
    );

    // With the menu open, a press on a window's title bar closes the menu
    // and drags the window at once (owner, 28.10.): the press is not the
    // menu's grab's but the window's, which asks to move with it.
    let (_, lines) = shell.capture(PLACED)?;
    let ((x, y), (w, _)) = lines
        .iter()
        .rev()
        .find_map(|l| placement(l, "beize-terminal"))
        .ok_or_else(|| anyhow::anyhow!("no terminal's window was placed"))?;
    let (x, y, w) = (i32::try_from(x)?, i32::try_from(y)?, i32::try_from(w)?);
    let grip = (x + w / 2, y + i32::try_from(TITLE_BAR / 2)?);
    // In the title bar's right end before, the desktop once moved off; left
    // of the window before, in its title bar once moved.
    let left_behind = (x + w - 40, grip.1);
    let arrived = (x - 40, grip.1 + MENU_DRAG_BY.1);
    let dark = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3 < WINDOW_DARK;
    let at = |image: &Image, (px, py): (i32, i32)| -> Result<[u8; 3]> {
        Ok(image.pixel(usize::try_from(px)?, usize::try_from(py)?))
    };
    let before = self::screen(machine, "drag-before")?;
    ensure!(
        dark(at(&before, left_behind)?) && !dark(at(&before, arrived)?),
        "the terminal's title bar is not where it was placed ({}x{} at {x},{y}): the check \
         would prove nothing",
        w,
        TITLE_BAR
    );
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 4)?;
    drag(qmp, screen, grip, by(grip, MENU_DRAG_BY), true)?;
    park(qmp, screen)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 4)?;
    wait_until_screen(machine, "dragged-from-menu", |shown| {
        let (gone, there) = (at(shown, left_behind)?, at(shown, arrived)?);
        ensure!(
            !dark(gone) && dark(there),
            "the terminal did not move with the drag that closed the menu: {gone:?} where its \
             title bar was, {there:?} where it should be"
        );
        Ok(())
    })?;
    say!(
        "  ok: with the Beize menu open, a drag on a window's title bar closed it and moved the window"
    );

    // Escape closes the menu, the control center opens while it is still
    // held: its release goes to the control center, which stays (owner,
    // 29.10.: it closed on the release).
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 5)?;
    hold(qmp, "esc", true)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 5)?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 2)?;
    hold(qmp, "esc", false)?;
    thread::sleep(Duration::from_secs(1));
    let closed = shell_count(shell, "ControlCenter closed")?;
    ensure!(
        closed == 1,
        "the control center closed when the Escape that closed the Beize menu was let go"
    );
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 2)?;
    say!(
        "  ok: the control center, opened while the Escape that closed the Beize menu was held, \
         stayed when it was let go"
    );
    liquid_glass(shell, machine, qmp, screen, centre)?;
    control_center_smoke(shell, machine, qmp, screen, centre)
}

/// The control center's liquid glass over the light background (ADR 0028
/// §6), where nothing else lies: the network capsule's tone is the one its
/// curve gives for what lies behind – about as light as that over midtones,
/// not grey smoke (0.5.118's glass came out at a third of it) – and white
/// ink keeps 3:1 on it (Apple's tiles ~3.4:1, measured); the round tile's
/// line of light at its top is never darker than either side of it (a grey
/// stroke) and brighter than at its side, which has none. Its screen is in
/// the logs for the owner (`control-center-liquid.png`).
fn liquid_glass(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    centre: (i32, i32),
) -> Result<()> {
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let top = MENU_BAR + CONTROL_CENTER_GAP;
    let capsule = (
        left..left + CONTROL_CENTER_WIDTH,
        top..top + CONTROL_CENTER_TILE,
    );
    // Right of the capsule's text, more than its curved edge inside.
    let clear_of_ink = (
        left + CONTROL_CENTER_WIDTH - 28..left + CONTROL_CENTER_WIDTH - 18,
        top + 30..top + 45,
    );
    let closed = self::screen(machine, "liquid-closed")?;
    let behind = mean_luminance(&closed, capsule.0.clone(), capsule.1.clone());
    ensure!(
        behind > LIQUID_KNEE + 0.05,
        "behind the control center the screen is not light ({behind:.3}): the check would prove \
         nothing"
    );
    let expected = liquid_tone(behind);
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 3)?;
    let radius = CONTROL_CENTER_TILE as f64 / 2.0;
    let round = (
        left as f64 + radius,
        (top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP) as f64 + radius,
    );
    // Where the edge's outward normal points at `degrees` (0 right, 90
    // down): the brightest pixel across the edge – the line lies ~1.3 px
    // in, so every pixel from 3.5 px inside to half a pixel out –, and how
    // far it stands above the lighter of the glass 8 px inside and the
    // screen 4 px outside (the mean of the channels).
    let line_at = |image: &Image, degrees: f64| {
        let (dx, dy) = (degrees.to_radians().cos(), degrees.to_radians().sin());
        let at = |r: f64| {
            let p = image.pixel((round.0 + dx * r) as usize, (round.1 + dy * r) as usize);
            (p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3) as i32
        };
        let peak = (0..=16)
            .map(|i| at(radius + 0.5 - 0.25 * f64::from(i)))
            .max()
            .unwrap_or(0);
        (peak, peak - at(radius - 8.0).max(at(radius + 4.0)))
    };
    let seen = std::cell::Cell::new((0.0, 0, 0));
    wait_until_screen(machine, "liquid", |shown| {
        let tile = mean_luminance(shown, clear_of_ink.0.clone(), clear_of_ink.1.clone());
        let contrast = 1.05 / (tile + 0.05);
        ensure!(
            (tile - expected).abs() <= expected * LIQUID_TOLERANCE,
            "the capsule's tone is {tile:.3} over {behind:.3} behind it, not {expected:.3} \
             (its curve)"
        );
        ensure!(
            contrast >= 3.0,
            "white ink keeps only {contrast:.2}:1 on the capsule"
        );
        let ((lit, above), (across, across_above)) = (line_at(shown, -90.0), line_at(shown, 0.0));
        ensure!(
            above >= LINE_ABOVE,
            "the round tile's line of light at its top is darker than a side of it ({lit}, \
             {above} over the lighter): a stroke, not light"
        );
        ensure!(
            above >= across_above + LINE_LIT_BY,
            "the round tile's line of light is not brighter at its top than at its side: {lit} \
             at the top, {across} at the side"
        );
        let (edge, outside) = (along_top(shown, left, top), along_top(shown, left, top - 3));
        ensure!(
            edge >= outside - TOP_EDGE_BELOW,
            "the capsule's outermost pixel at its top is {edge}, {outside} outside it: the \
             glass's grey above the line of light"
        );
        seen.set((tile, lit, across));
        Ok(())
    })?;
    self::screen(machine, "liquid-open")?
        .write_png(&machine.logs.join("control-center-liquid.png"))?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 3)?;
    let (tile, lit, across) = seen.get();
    say!(
        "  ok: the control center's liquid glass: over {behind:.3} behind it the capsule came \
         out at {tile:.3} ({expected:.3} by its curve, white ink {:.2}:1), its line of light \
         {lit} at the top, {across} at the side",
        1.05 / (tile + 0.05)
    );
    Ok(())
}

/// Liquid glass's tone with white ink on it (`quad.vert`, `liquid_tone`):
/// what lies behind as it is up to KNEE, above eased towards CAP.
fn liquid_tone(behind: f64) -> f64 {
    if behind <= LIQUID_KNEE {
        return behind;
    }
    LIQUID_KNEE + (LIQUID_CAP - LIQUID_KNEE) * (1.0 - (-(behind - LIQUID_KNEE) / 0.15).exp())
}

/// As `quad.vert`'s KNEE and CAP. The screen's tile differs from the curve
/// by its saturation, the milk and the frost's mean against the screen's:
/// within a quarter of it.
const LIQUID_KNEE: f64 = 0.22;
const LIQUID_CAP: f64 = 0.27;
const LIQUID_TOLERANCE: f64 = 0.25;
/// The line of light, in 8-bit steps (the mean of the channels): at the
/// top never below the lighter of its two sides, and above them by more
/// than at the side, which has no line (Apple's: light at the top and the
/// bottom, a dark edge at the sides). Over the light background, where the
/// glass is pulled down for its ink, it lies on what is outside it: some
/// 2 steps above it at the top (Apple's over white: 3), while a line on the
/// glass would come out ~15 below it.
const LINE_ABOVE: i32 = 0;
const LINE_LIT_BY: i32 = 6;
/// How far the outermost pixel along a capsule's top may lie below the
/// screen outside it, in the same steps: the frost's mean against the
/// screen's (the owner saw the glass's grey there, 30.10.).
const TOP_EDGE_BELOW: i32 = 2;

/// A row's mean of the channels along the top of the capsule at `left`.
fn along_top(image: &Image, left: usize, y: usize) -> i32 {
    let mid = left + CONTROL_CENTER_WIDTH / 2;
    let sum: u32 = (mid - 10..mid + 10)
        .map(|x| image.pixel(x, y).iter().map(|&c| u32::from(c)).sum::<u32>())
        .sum();
    (sum / 60) as i32
}

/// The mean relative luminance (linear light) of `xs` × `ys`.
fn mean_luminance(screen: &Image, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f64 {
    let decoded = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let [r, g, b] = screen.pixel(x, y);
            sum += 0.2126 * decoded(r) + 0.7152 * decoded(g) + 0.0722 * decoded(b);
            n += 1.0;
        }
    }
    sum / f64::max(n, 1.0)
}

/// The control center's clear glass and the smoke around it (ADR 0028 §5,
/// §6), over the terminal maximized behind it and full of text: beside and
/// between its tiles the text blurs; where the smoke ends – 112 to 130 px
/// from them, a blur below a hundredth of a pixel – the screen is as it was,
/// so no seam, no grain and never the background shows there; the bar's ink
/// stays sharp over it (the smoke lies behind the bar). Its screens are in
/// the logs for the owner (`control-center-smoke-*.png`). The terminal's
/// text cursor sits below the last line, far from where it compares.
fn control_center_smoke(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    centre: (i32, i32),
) -> Result<()> {
    let (_, lines) = shell.capture(PLACED)?;
    let ((x, y), (w, _)) = lines
        .iter()
        .rev()
        .find_map(|l| placement(l, "beize-terminal"))
        .ok_or_else(|| anyhow::anyhow!("no terminal's window was placed"))?;
    // Where the drag with the menu open left its title bar (`menus`).
    let grip = by(
        (i32::try_from(x + w / 2)?, i32::try_from(y + TITLE_BAR / 2)?),
        MENU_DRAG_BY,
    );
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let top = MENU_BAR + CONTROL_CENTER_GAP;
    let behind = (screen.0 - BAR_MARGIN - 4, top + CONTROL_CENTER_TILE / 2);
    let dark = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3 < WINDOW_DARK;
    click(qmp, screen, grip, 2)?;
    park(qmp, screen)?;
    wait_until_screen(machine, "smoke-maximized", |shown| {
        let p = shown.pixel(behind.0, behind.1);
        ensure!(
            dark(p),
            "the terminal is not maximized behind the control center: {p:?}"
        );
        Ok(())
    })?;
    let (unit, _) = terminals(shell)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no terminal runs"))?;
    // Lines of text across the screen, written to the shell's terminal.
    let (_, said) = shell.capture(&format!(
        "d=/sys/fs/cgroup$(systemctl --user show -P ControlGroup '{unit}'); t=; \
         for p in $(cat \"$d/cgroup.procs\"); do [ \"$(cat /proc/$p/comm)\" = bash ] && \
         t=/proc/$p/fd/1; done; l=$(printf 'Wm#8%.0s' $(seq 60)); \
         [ -n \"$t\" ] && for i in $(seq 40); do echo \"$l\"; done > \"$t\"; \
         echo BEIZE-TEXT-${{t:+written}}"
    ))?;
    ensure!(
        said.iter().any(|l| l == "BEIZE-TEXT-written"),
        "no text went to the terminal's shell: {said:?}"
    );
    thread::sleep(Duration::from_secs(1));
    let closed = self::screen(machine, "smoke-closed")?;
    // Beside the round tile, right of it; the bar's clock, left of the
    // control center's pill (its half, 27 px, and 8 px between); where the
    // smoke ends, left of the tiles.
    let beside = (
        left + CONTROL_CENTER_TILE + 10..screen.0 - BAR_MARGIN - 10,
        {
            let round = top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP;
            round + 5..round + CONTROL_CENTER_TILE - 5
        },
    );
    let clock_end = usize::try_from(centre.0)? - 35;
    let clock = (clock_end - 100..clock_end, 8..24);
    let fading = (left - 128..left - 112, MENU_BAR + TITLE_BAR + 8..330);
    for (what, (xs, ys)) in [("beside the tiles", &beside), ("where it ends", &fading)] {
        let there = detail(&closed, xs.clone(), ys.clone());
        ensure!(
            there >= TEXT_DETAIL,
            "the terminal shows no text {what} ({there:.1}): the check would prove nothing"
        );
    }
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 4)?;
    let sharp = detail(&closed, beside.0.clone(), beside.1.clone());
    wait_until_screen(machine, "smoke", |shown| {
        let blurred = detail(shown, beside.0.clone(), beside.1.clone());
        ensure!(
            blurred < sharp * SMOKE_KEEPS,
            "the text beside the tiles is not blurred: detail {blurred:.1}, {sharp:.1} without"
        );
        Ok(())
    })?;
    thread::sleep(Duration::from_millis(500));
    let open = self::screen(machine, "smoke-open")?;
    open.write_png(&machine.logs.join("control-center-smoke-light.png"))?;
    // Over the dark window the glass is lighter than what is outside: above
    // the top line it would stand as a grey row, more than half way up to it.
    let (edge, outside, lit) = (
        along_top(&open, left, top),
        along_top(&open, left, top - 3),
        along_top(&open, left, top + 1),
    );
    ensure!(
        (edge - outside) * 2 < lit - outside,
        "over the dark window the capsule's outermost pixel at its top is {edge}, between \
         {outside} outside it and {lit} on the line: the glass's grey above the line of light"
    );
    let (xs, ys) = &fading;
    let (mut most, mut differ) = (0, 0);
    for py in ys.clone() {
        for px in xs.clone() {
            let (a, b) = (closed.pixel(px, py), open.pixel(px, py));
            let d = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
            most = most.max(d);
            differ += usize::from(d > 0);
        }
    }
    let all = xs.len() * ys.len();
    ensure!(
        most <= 1 && differ * 50 <= all,
        "where the smoke ends the screen changed: by up to {most}, in {differ} of {all} pixels \
         (a seam, grain or the background)"
    );
    let (ink_before, ink) = (
        detail(&closed, clock.0.clone(), clock.1.clone()),
        detail(&open, clock.0.clone(), clock.1.clone()),
    );
    ensure!(
        ink > ink_before * 0.9,
        "the bar's clock is blurred over the smoke: detail {ink:.1}, {ink_before:.1} without"
    );
    // For the owner: the same in dark.
    let tile = (
        i32::try_from(left + CONTROL_CENTER_TILE / 2)?,
        i32::try_from(top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP + CONTROL_CENTER_TILE / 2)?,
    );
    let (darks, lights) = (
        shell_count(shell, "appearance Dark")?,
        shell_count(shell, "appearance Light")?,
    );
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Dark", darks + 1)?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "smoke-dark")?
        .write_png(&machine.logs.join("control-center-smoke-dark.png"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Light", lights + 1)?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 4)?;
    say!(
        "  ok: over a terminal the control center's smoke blurred the text beside its tiles \
         (detail {sharp:.1} -> {:.1}), left it as it was where it ends ({differ} of {all} \
         pixels by 1 at most) and the bar's clock sharp",
        detail(&open, beside.0.clone(), beside.1.clone())
    );
    Ok(())
}

/// How much the screen changes from pixel to pixel in `xs` × `ys`: the
/// mean step in brightness (the mean of the channels) to the right
/// neighbour – text has many, a blur few.
fn detail(screen: &Image, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f64 {
    let brightness = |p: [u8; 3]| p.iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0;
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.start..xs.end.saturating_sub(1) {
            sum += (brightness(screen.pixel(x + 1, y)) - brightness(screen.pixel(x, y))).abs();
            n += 1.0;
        }
    }
    sum / f64::max(n, 1.0)
}

/// Text shows at least this much detail (`detail`): a terminal's glyphs on
/// its dark ground, many times more than its ground alone.
const TEXT_DETAIL: f64 = 8.0;
/// The smoke beside the tiles keeps less than this share of the text's
/// detail (a Gaussian of 1.5 to 3 px there).
const SMOKE_KEEPS: f64 = 0.5;

/// How often the shell said `line`.
fn shell_count(shell: &Shell, line: &str) -> Result<usize> {
    let (_, lines) = shell.capture(SHELL_SAID)?;
    Ok(lines.iter().filter(|l| *l == line).count())
}

/// How far the drag with the Beize menu open moves the terminal: clear of
/// where its title bar's right end was, over 40 px left of where it began.
const MENU_DRAG_BY: (i32, i32) = (-160, 100);
/// A terminal's title bar is darker than this (mean of the channels); the
/// desktop in light is lighter.
const WINDOW_DARK: u32 = 90;

/// The mean of the channels at the screen's left edge, 100 pixels a side
/// at half its height: the background, beside the windows in the middle.
fn left_mean(screen: &Image) -> f64 {
    let y = screen.height / 2;
    let rows = y.saturating_sub(50)..(y + 50).min(screen.height);
    let n = rows.len().max(1) as f64;
    rows.map(|row| row_mean(screen, row, 8..108.min(screen.width)))
        .sum::<f64>()
        / n
}

/// How light a menu's glass is at least, as the mean of a pixel's channels:
/// its tint (#F2F2F2 at 85 %) over anything.
const MENU_LIGHT: u32 = 200;
/// The control center's measures in the VM, as the shell's (macOS' 12 and
/// 64 pt at Beize's 1.25, 6.5 % smaller): as wide as its columns used, two
/// while network and light/dark are its only tiles; its right edge where
/// the menu bar's margin ends.
const CONTROL_CENTER_GAP: usize = 14;
const CONTROL_CENTER_TILE: usize = 75;
const BAR_MARGIN: usize = 20;
const CONTROL_CENTER_WIDTH: usize = 2 * CONTROL_CENTER_TILE + CONTROL_CENTER_GAP;

/// Where the bar's buttons are, as the shell said last: the Beize menu's
/// and the control center's (the clock's width moves it).
fn bar_buttons(shell: &Shell) -> Result<((i32, i32), (i32, i32))> {
    let line = wait_for_shell_line(shell, |l| l.starts_with("menu bar buttons: "))?;
    let (_, lines) = shell.capture(&format!("{SHELL_SAID} | grep '^menu bar buttons: '"))?;
    let line = lines
        .iter()
        .rfind(|l| l.starts_with("menu bar buttons: "))
        .unwrap_or(&line);
    let points: Vec<(i32, i32)> = line
        .split("Some((")
        .skip(1)
        .filter_map(|part| {
            let (x, rest) = part.split_once(", ")?;
            let (y, _) = rest.split_once("))")?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect();
    match points[..] {
        [beize, centre] => Ok((beize, centre)),
        _ => bail!("the shell said no buttons: {line}"),
    }
}

/// Waits until the shell said `line` at least `count` times in this boot.
fn wait_for_shell_count(shell: &Shell, line: &str, count: usize) -> Result<()> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let (_, lines) = shell.capture(SHELL_SAID)?;
        let said = lines.iter().filter(|l| *l == line).count();
        if said >= count {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the shell said {line:?} {said} times, not {count}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// The appearance the compositor kept (~/.config/beize/appearance).
fn keep_says(shell: &Shell) -> Result<String> {
    let (_, lines) = shell.capture("cat ~/.config/beize/appearance | sed 's/^/BEIZE-SAID /'")?;
    Ok(lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SAID "))
        .unwrap_or_default()
        .to_owned())
}

/// The compositor sums up a client's run of frames, alone and in full.
fn frames(shell: &Shell) -> Result<()> {
    // Frames in a row: a second client draws 120, each on "frame done";
    // once they stop, the compositor sums up how they met their vblanks –
    // the next summary, alone: as many frames as it drew, not more. The
    // drag's frames end their run first; a short run is summed up only for
    // debugging, so there is nothing to wait for but its end.
    thread::sleep(RUN_ENDED);
    let summed = pacing_summaries(shell)?;
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit=beize-testclient-frames /run/t/beize-testclient --frames 120 \
         --seconds 20 && for i in $(seq 100); do [ \"$({PACING} | wc -l)\" -gt {summed} ] \
         && break; sleep 0.2; done; {PACING} | tail -n +{}",
        summed + 1
    ))?;
    let Some(pacing) = lines.iter().find(|l| l.starts_with("pacing: ")) else {
        let said = last_said(shell, &["beize-testclient-frames", "beize-compositor"])?;
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
    // A frame or two more for the menu bar (its minute) is no mix.
    ensure!(
        (100..=125).contains(&frames),
        "the compositor counted {frames} frames of the client's 120: {pacing}"
    );
    say!("  ok: the compositor measured the client's frames: {pacing}");
    presentation(shell)?;
    // Its window would go by itself later, in the middle of another check.
    shell.capture("systemctl --user stop beize-testclient-frames")?;
    fifo(shell)?;
    timed(shell)?;
    syncobj(shell)?;

    Ok(())
}

/// The client of `frames` heard for each frame when it showed
/// (wp_presentation): at its vblank, on CLOCK_MONOTONIC (clock 1), timed
/// by the hardware – flags only a vblank's time gets, not the "changed
/// nothing" path's –, times and sequence rising, each before it arrived.
/// And none was discarded: a client drawing on "frame done" hears it only
/// once its last frame is drawn, so it never replaces one not yet shown
/// (until 0.5.104 one in 120 was, when its commit came after drawing
/// began; three runs of three). And it hears it then, each before its frame
/// showed, as Weston answers at repaint: answered at the vblank (until
/// 0.5.107), a client at 240 Hz had ~1 ms left to draw its next.
fn presentation(shell: &Shell) -> Result<()> {
    let (_, lines) = shell.capture(
        "for i in $(seq 25); do journalctl --user -u beize-testclient-frames -o cat \
         | grep -q '^presentation: ' && break; sleep 0.2; done; \
         journalctl --user -u beize-testclient-frames -o cat \
         | grep -e '^presentation: ' -e '^frame done: '",
    )?;
    let line = lines
        .iter()
        .find(|l| l.starts_with("presentation: "))
        .context("the client said nothing of when its frames showed")?;
    for part in [
        "presentation: 120 of 120 shown, 0 discarded",
        "clock 1,",
        "on all: vsync hw_clock hw_completion,",
        "rising yes,",
        "before arrival yes",
    ] {
        ensure!(line.contains(part), "not \"{part}\": {line}");
    }
    ensure!(
        !line.contains("refresh 0 ns"),
        "no refresh with the frames: {line}"
    );
    let done = lines
        .iter()
        .find(|l| l.starts_with("frame done: "))
        .context("the client said nothing of when it heard \"frame done\"")?;
    ensure!(
        done == "frame done: 120 of 120 before they showed",
        "\"frame done\" not as soon as each frame was drawn, before it showed: {done}"
    );
    say!("  ok: the client heard when each frame showed: {line}; {done}");
    Ok(())
}

/// A client drawing as Mesa's Vulkan FIFO mode does (wp_fifo): without
/// waiting for "frame done", each commit waits in the compositor until the
/// one before it showed. What wp_fifo promises, without exception: all 120
/// show, none discarded, each at a later vblank than the one before.
///
/// How many show at the very next vblank is how fast the frames are drawn,
/// not the protocol: in the VM lavapipe draws on the guest's CPUs and the
/// compositor waits for it, so under load a frame takes longer than a
/// refresh and shows a vblank later (27.10.: 4 of 4 red runs under host
/// load had 12–35 such frames, the client late for none, drawing up to
/// 123 ms; the barrier lifted at the vblank, the frame drawn at once).
/// Until then a tolerance of two such frames was checked, which no run under
/// load could meet. What is checked of it is what an error of the
/// compositor's would do to all frames – the barrier lifted a vblank late,
/// the redraw after it not scheduled: then none shows at the next vblank.
/// How many do is measured on hardware (`measure/hw-test.sh`).
fn fifo(shell: &Shell) -> Result<()> {
    let said = client_summary(shell, "beize-testclient-fifo", "--fifo 120", "fifo: ")?;
    let presented = said.iter().find(|l| l.starts_with("presentation: "));
    let apart = said.iter().find(|l| l.starts_with("fifo: "));
    let (Some(presented), Some(apart)) = (presented, apart) else {
        bail!("no summary of the fifo client: {said:?}");
    };
    ensure!(
        presented.contains("presentation: 120 of 120 shown, 0 discarded"),
        "frames of the fifo client were lost: {presented}"
    );
    ensure!(
        presented.contains("rising yes"),
        "frames of the fifo client showed at the same vblank or out of turn: {presented}"
    );
    let count = apart
        .strip_prefix("fifo: ")
        .and_then(|s| s.split_once(' '))
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("no count in: {apart}"))?;
    ensure!(
        count > 0,
        "no frame of the fifo client showed at the vblank after the last: {apart}"
    );
    let starved = said
        .iter()
        .find(|l| l.ends_with("committed after the frame before showed"))
        .map_or("", String::as_str);
    say!("  ok: a fifo client's frames each showed, in turn: {apart}; {starved}");
    Ok(())
}

/// A client giving each frame a time (wp_commit_timing), 2.5 refreshes after
/// the last one showed: never before its time, though it was ready long
/// before. At the first vblank at or after it, as with `fifo`: what the
/// compositor would get wrong for all (the time let go a vblank late), not
/// how fast the VM draws.
fn timed(shell: &Shell) -> Result<()> {
    let said = client_summary(shell, "beize-testclient-timed", "--timed 30", "timed: ")?;
    let timed = said
        .iter()
        .find(|l| l.starts_with("timed: "))
        .with_context(|| format!("no summary of the timed client: {said:?}"))?;
    ensure!(
        timed.contains(" 0 before it,"),
        "frames of the timed client showed before their time: {timed}"
    );
    let on_time = timed
        .strip_prefix("timed: ")
        .and_then(|s| s.split_once(" of 29 "))
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("no count of 29 in: {timed}"))?;
    ensure!(
        on_time > 0,
        "no frame of the timed client showed at the first vblank at its time: {timed}"
    );
    say!("  ok: a client's frames showed at their time: {timed}");
    Ok(())
}

/// Explicit sync (linux-drm-syncobj): the test client's dmabufs (dumb
/// buffers of virtio-gpu; the VM's lavapipe takes them through udmabuf)
/// with timeline points it signals itself. Its checks are about order,
/// not time: a commit shows only after its acquire point signalled; a
/// buffer's release point signals (the kernel's time) no earlier than the
/// vblank of the frame that showed it, also when the next one replaced it
/// while that frame was on its way – which must have happened, or the
/// check caught nothing; a buffer without points is a protocol error.
fn syncobj(shell: &Shell) -> Result<()> {
    let said = client_summary(
        shell,
        "beize-testclient-syncobj",
        "--syncobj",
        "syncobj protocol: ",
    )?;
    let line = |name: &str| {
        said.iter()
            .find(|l| l.starts_with(name))
            .with_context(|| format!("no {name} line of the syncobj client: {said:?}"))
    };
    let acquire = line("syncobj acquire: ")?;
    ensure!(
        acquire == "syncobj acquire: held until signalled, shown after",
        "a commit showed before its acquire point signalled: {acquire}"
    );
    let release = line("syncobj release: ")?;
    let counts: Vec<u32> = release
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect();
    let [after, of, replaced] = counts[..] else {
        bail!("no counts in: {release}");
    };
    ensure!(
        release.contains(" after their vblank began,") && after == of && of > 0,
        "a release point signalled while the compositor could still read its buffer: {release}"
    );
    ensure!(
        replaced > 0,
        "no buffer was replaced while its frame was on its way, the release check caught nothing: {release}"
    );
    let protocol = line("syncobj protocol: ")?;
    ensure!(
        protocol == "syncobj protocol: no_acquire_point raised",
        "a buffer without sync points was taken: {protocol}"
    );
    say!("  ok: explicit sync: {acquire}; {release}; {protocol}");
    Ok(())
}

/// Runs the test client as `unit` with `args` until it printed a line
/// starting with `last`, then stops it; its summary lines (`name: …`).
fn client_summary(shell: &Shell, unit: &str, args: &str, last: &str) -> Result<Vec<String>> {
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit={unit} /run/t/beize-testclient {args} --seconds 20 \
         && for i in $(seq 100); do journalctl --user -u {unit} -o cat | grep -q '^{last}' \
         && break; sleep 0.2; done; journalctl --user -u {unit} -o cat \
         | grep -E '^(presentation|fifo|timed|(activation|syncobj) [a-z ]+): '; \
         systemctl --user stop {unit}"
    ))?;
    Ok(lines)
}

/// The shell killed and stopped comes back; its panels stay meanwhile and
/// Beize UI's window `ui` with them.
fn shell_restarts(
    paths: &Paths,
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    ui: Window,
) -> Result<()> {
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
        shell,
        "the shell failed: its 2 panels stay until a new one is ready",
    )?;
    let replaced = wait_for_compositor_line(
        shell,
        "a new shell is ready [0-9]* ms after the old one failed: its panels replace the kept ones",
    )?;
    wait_for_panels(machine)?;
    wait_for_screen(
        paths,
        machine,
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
    screendump(qmp, &file)?;
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
    wait_for_panels(machine)?;
    say!("  ok: stopped, the shell's panels stayed on the screen until a new one replaced them");

    Ok(())
}

/// The layout set with localectl applies at once, and the next one too:
/// localed renames a new file over the watched one, which is watched anew
/// (`keyboard::Watch`). Back to the owner's layout after.
fn layout_follows(shell: &Shell) -> Result<()> {
    let (_, lines) = shell.capture(
        "( n() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^keyboard layout '; }; \
         w() { for i in $(seq 50); do [ \"$(n)\" -gt \"$1\" ] && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep '^keyboard layout ' \
         | tail -n 1 | sed 's/^/BEIZE-LAYOUT /'; }; \
         . /etc/vconsole.conf; s=$(n); \
         a=$(n); run0 localectl set-x11-keymap --no-convert fr pc105; w $a; \
         a=$(n); run0 localectl set-x11-keymap --no-convert \"$XKBLAYOUT\" \"${XKBMODEL:-pc105}\" \
         \"$XKBVARIANT\" \"$XKBOPTIONS\"; w $a; sleep 0.5; echo BEIZE-APPLIED $(( $(n) - s )) )",
    )?;
    let said: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("BEIZE-LAYOUT "))
        .collect();
    ensure!(
        said.len() == 2 && said[0] == "keyboard layout fr" && said[1] != said[0],
        "the compositor did not follow two layouts set in a row: {lines:?}"
    );
    // localed's one write is two events (`State::reload_layout`).
    ensure!(
        lines.iter().any(|l| l == "BEIZE-APPLIED 2"),
        "the compositor applied two layouts other than twice: {lines:?}"
    );
    say!(
        "  ok: a keyboard layout set with localectl applied at once, and the next one too ({})",
        said[1]
    );
    Ok(())
}

/// No warnings, the still desktop does not wake, and signing out stops it.
fn still_and_signed_out(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let (_, lines) =
        shell.capture("journalctl --user -b -u beize-compositor.service -p warning -o cat")?;
    let unexpected: Vec<&String> = lines
        .iter()
        .filter(|l| !l.is_empty() && !KNOWN_WARNINGS.contains(&l.as_str()))
        .collect();
    ensure!(
        unexpected.is_empty(),
        "the compositor warned: {unexpected:?}"
    );
    say!("  ok: no warnings from the compositor (but Smithay's known ones)");

    // Still, nothing of the desktop wakes (ADR 0027 §6: events, no
    // polling): for IDLE_SECONDS the compositor, the shell and the terminal
    // switch to no other task, not one of their threads – measured off the
    // minute, when the menu bar's clock ticks (measured again if one began
    // meanwhile: the run0 below lengthens the window by its password, and
    // once, 27.10., it ended past the minute; again if ntp-daemon set the
    // clock, which rightly wakes the shell's clock, CANCEL_ON_SET: 867 ms on
    // 31.10.), once the terminal's first
    // frames are done – and with no test program left, whose window going
    // by itself would wake them rightly (a test client did, 25.10.) – nor
    // one still stopping. Woken, each thread that switched is named
    // (`BEIZE-WOKE`) with what the journal says meanwhile (`BEIZE-JOURNAL`).
    // Meanwhile the system goes on: a service with DynamicUser= starts, for
    // which systemd takes /etc/.pwd.lock – while the compositor watched all
    // of /etc for its keyboard layout, that woke it (main thread once, in
    // ~1 of 9 runs 25.–27.10., when ntpd-rs or another such service started).
    let (_, left) = shell.capture(
        "systemctl --user list-units --state=active,activating,deactivating,reloading --plain \
         --no-legend 'beize-testclient*' 'beize-ui-test*' 'beize-load-test*' \
         | sed 's/^/BEIZE-LEFT /'",
    )?;
    let left: Vec<&String> = left
        .iter()
        .filter(|l| l.starts_with("BEIZE-LEFT "))
        .collect();
    ensure!(
        left.is_empty(),
        "test programs still run before the desktop is watched still: {left:?}"
    );
    thread::sleep(RUN_ENDED);
    let (_, lines) = shell.capture(&format!(
        "c() {{ cat /proc/$1/task/*/status | awk '/ctxt_switches/ {{s += $2}} END {{print s}}'; }}; \
         for try in 1 2; do \
         while [ $((10#$(date +%S))) -gt $((58 - {IDLE_SECONDS})) ]; do sleep 0.5; done; \
         minute=$(date +%H%M); \
         : > /tmp/idle-threads; for n in beize-compositor beize-shell beize-terminal; do \
         p=$(pgrep -f \"^/usr/lib/beize/$n\" | head -n 1); echo \"$n $p $(c $p)\"; \
         for t in /proc/$p/task/*; do echo \"$n ${{t##*/}} $(tr ' ' _ < $t/comm) \
         $(awk '/ctxt_switches/ {{s += $2}} END {{print s}}' $t/status)\" >> /tmp/idle-threads; \
         done; done > /tmp/idle; since=$(date +%s.%N); \
         run0 systemd-run -q -p DynamicUser=yes true; \
         sleep {IDLE_SECONDS}; \
         [ \"$(date +%H%M)\" = \"$minute\" ] \
         && ! journalctl -b --since=@$since -q -o cat | grep -q 'Jumped offset' && break; \
         echo BEIZE-NEW-MINUTE; \
         sleep {settle}; done; \
         while read -r n p s; do echo \"BEIZE-IDLE $n $(( $(c $p) - s ))\"; done < /tmp/idle; \
         while read -r n t name s; do \
         now=$(awk '/ctxt_switches/ {{s += $2}} END {{print s}}' /proc/*/task/$t/status 2>/dev/null); \
         [ \"${{now:-$s}}\" != \"$s\" ] && echo \"BEIZE-WOKE $n $name/$t $(( ${{now:-$s}} - s ))\"; \
         done < /tmp/idle-threads; \
         journalctl -b --since=@$since -o short-monotonic --no-pager -q | sed 's/^/BEIZE-JOURNAL /'; \
         true",
        // Before measuring again, what the new minute drew settles, as
        // before the first.
        settle = RUN_ENDED.as_secs()
    ))?;
    let woke: Vec<(&str, u64)> = lines
        .iter()
        .filter_map(|l| {
            let (name, switches) = l.strip_prefix("BEIZE-IDLE ")?.split_once(' ')?;
            Some((name, switches.parse().ok()?))
        })
        .collect();
    ensure!(
        woke.len() == 3 && woke.iter().all(|&(_, n)| n == 0),
        "the still desktop woke: task switches in {IDLE_SECONDS} s {woke:?}; {lines:?}"
    );
    say!(
        "  ok: still, the compositor, the shell and the terminal did not wake once in \
         {IDLE_SECONDS} s"
    );

    // Signing out, from the Beize menu with its confirmation: logind ends the
    // session, beize-session stops the desktop and, last of all, takes the
    // environment back: waited for that, the rest is done too.
    let (beize, _) = bar_buttons(shell)?;
    click(qmp, screen, beize, 1)?;
    thread::sleep(Duration::from_millis(500));
    click(qmp, screen, beize_menu_row(beize, 3, true), 1)?;
    wait_for_shell_count(shell, "asking: LogOut", 1)?;
    thread::sleep(Duration::from_millis(500));
    keys(qmp, &["ret"])?;
    let (_, lines) = shell.capture(
        "for i in $(seq 50); do systemctl --user show-environment | grep -q '^XDG_SESSION_ID=' \
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
    let left = terminals(shell)?;
    ensure!(
        left.is_empty(),
        "terminals still run after signing out: {left:?}"
    );
    say!(
        "  ok: signing out stopped the desktop, its shell and terminals and took its environment \
         back"
    );
    Ok(())
}

/// The screen goes off without input (os/beize-compositor, `idle`), in a
/// session of its own with `SCREEN_OFF_AFTER` – the desktop's other
/// checks would see it go off. The owner signs in again on tty1. Without
/// input the screen fades to black, darker a while in, and a key takes
/// the fade back: the desktop as it was. The test client's window with an
/// idle inhibitor keeps the screen from fading for twice the time; once
/// the client ends, the screen fades, goes off – the kernel says the
/// connector is off (DPMS) – and a key turns it on with the desktop.
/// Nothing warns but Smithay's known warnings.
fn screen_goes_off(shell: &Shell, machine: &Machine, qmp: &Path) -> Result<()> {
    let after = SCREEN_OFF_AFTER.as_secs();
    let (_, lines) = shell.capture(&format!(
        "systemctl --user set-environment BEIZE_SCREEN_OFF_AFTER={after} && \
         for i in $(seq 50); do pgrep -t tty1 -x agetty > /dev/null && break; sleep 0.2; done; \
         echo BEIZE-SINCE-$(date +%s.%N)"
    ))?;
    let since = lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SINCE-"))
        .with_context(|| format!("no time to look from: {lines:?}"))?
        .to_owned();
    // The new getty's prompt, then as at the first sign-in.
    thread::sleep(Duration::from_secs(1));
    for (typed, pause) in [(SMOKE_USER, 1), (SMOKE_PASSWORD, 2), ("beize-session", 0)] {
        keys(qmp, &german_presses(typed)?)?;
        thread::sleep(Duration::from_secs(pause));
    }
    let said =
        |line: &str, count: usize, within: Duration| said_since(shell, &since, line, count, within);
    said(
        &format!("the screen goes off after {after} s without input"),
        1,
        SCREEN_TIMEOUT,
    )?;
    said(
        "desktop shown with the shell, [0-9]* ms after the screen",
        1,
        SCREEN_TIMEOUT,
    )?;
    ensure!(
        dpms(shell)? == "On",
        "the screen is not on after signing in"
    );

    // Without input it fades; darker a while in; a key takes it back.
    const FADES: &str = "no input for [0-9]* s: the screen fades to black";
    said(FADES, 1, SCREEN_OFF_AFTER + SCREEN_OFF_SLACK)?;
    thread::sleep(FADE_LOOKED_AT);
    let faded = screen(machine, "fading")?;
    keys(qmp, &["shift"])?;
    said(
        "input: the fade to black is taken back",
        1,
        SCREEN_OFF_SLACK,
    )?;
    wait_for_panels(machine)?;
    let desktop = screen(machine, "fade-taken-back")?;
    let darker = centre_mean(&desktop) - centre_mean(&faded);
    ensure!(
        darker >= FADE_DARKER,
        "{} s into the fade the screen was not darker than the desktop by {FADE_DARKER} \
         but by {darker:.1}; the screens are in the logs as fading and fade-taken-back",
        FADE_LOOKED_AT.as_secs()
    );
    say!(
        "  ok: without input for {after} s the screen faded to black, {darker:.0} darker {} s \
         in, and a key took the fade back",
        FADE_LOOKED_AT.as_secs()
    );

    // A window with an idle inhibitor keeps it from fading, for twice the
    // time; ended, the screen fades and goes off.
    let (_, lines) = shell.capture(
        "systemd-run --user --unit=beize-testclient-inhibit /run/t/beize-testclient --inhibit \
         --seconds 60 && for i in $(seq 50); do journalctl --user -u beize-testclient-inhibit \
         -o cat | grep -q '^configure ' && break; sleep 0.2; done; \
         journalctl --user -u beize-testclient-inhibit -o cat",
    )?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure ")),
        "the test client with an idle inhibitor got no window: {lines:?}"
    );
    thread::sleep(2 * SCREEN_OFF_AFTER + SCREEN_OFF_SLACK / 5);
    let fades = said_since(shell, &since, FADES, 0, Duration::ZERO)?;
    ensure!(
        fades == 1,
        "the screen faded with a window that inhibits idling on it ({fades} fades)"
    );
    shell.capture("systemctl --user stop beize-testclient-inhibit")?;
    said(FADES, 2, SCREEN_OFF_AFTER + SCREEN_OFF_SLACK)?;
    said("the screen is off", 1, SCREEN_FADE + SCREEN_OFF_SLACK)?;
    let state = dpms(shell)?;
    ensure!(
        state == "Off",
        "the compositor said the screen is off, the kernel says {state}"
    );
    say!(
        "  ok: a window with an idle inhibitor kept the screen on for {} s; once it ended, the \
         screen faded and went off (DPMS Off)",
        2 * after
    );

    // Off, the screen is gone for clients: one drawing as Mesa's FIFO does
    // may draw as hidden windows do, not in the screen's pace.
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --wait --unit=beize-testclient-off /run/t/beize-testclient \
         --fifo 1000 --seconds {}; journalctl --user -u beize-testclient-off -o cat \
         | grep '^drawing: '",
        OFF_DRAWING.as_secs()
    ))?;
    let drawn: usize = lines
        .iter()
        .find_map(|l| l.strip_prefix("drawing: ")?.split_once(' ')?.0.parse().ok())
        .with_context(|| format!("the client said nothing of its drawing: {lines:?}"))?;
    ensure!(
        (1..=OFF_FRAMES_MOST).contains(&drawn),
        "with the screen off a client drew {drawn} frames in {} s, not between 1 and \
         {OFF_FRAMES_MOST}",
        OFF_DRAWING.as_secs()
    );
    ensure!(
        dpms(shell)? == "Off",
        "a client's window turned the screen on"
    );
    say!(
        "  ok: off, a client drew {drawn} frames in {} s, as hidden windows do",
        OFF_DRAWING.as_secs()
    );

    // A key turns it on, with the desktop as it was.
    keys(qmp, &["shift"])?;
    said("input: the screen is on again", 1, SCREEN_OFF_SLACK)?;
    wait_for_panels(machine)?;
    let state = dpms(shell)?;
    ensure!(
        state == "On",
        "after a key the kernel says the screen is {state}"
    );
    let back = screen(machine, "screen-on")?;
    ensure!(
        (centre_mean(&back) - centre_mean(&desktop)).abs() < 1.0,
        "the desktop did not come back as it was; the screens are in the logs as \
         fade-taken-back and screen-on"
    );
    let (_, lines) = shell.capture(&format!(
        "journalctl --user -u beize-compositor.service --since=@{since} -p warning -o cat"
    ))?;
    let unexpected: Vec<&String> = lines
        .iter()
        .filter(|l| !l.is_empty() && !KNOWN_WARNINGS.contains(&l.as_str()))
        .collect();
    ensure!(
        unexpected.is_empty(),
        "the compositor warned: {unexpected:?}"
    );
    say!("  ok: a key turned the screen on with the desktop as it was");

    // Signed out as in `still_and_signed_out`: the environment goes last.
    let (_, lines) = shell.capture(
        "s=$(systemctl --user show-environment | sed -n 's/^XDG_SESSION_ID=//p'); \
         loginctl terminate-session \"$s\"; \
         for i in $(seq 50); do systemctl --user show-environment | grep -q '^XDG_SESSION_ID=' \
         || break; sleep 0.2; done; systemctl --user unset-environment BEIZE_SCREEN_OFF_AFTER; \
         systemctl --user is-active beize-compositor.service",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive"),
        "signing out did not stop the desktop: {lines:?}"
    );
    Ok(())
}

/// Waits until the compositor has said `line` (a pattern, as grep's) at
/// least `count` times since `since` (seconds since the epoch), at most
/// `within`; how often it has. `count` 0: only how often.
fn said_since(
    shell: &Shell,
    since: &str,
    line: &str,
    count: usize,
    within: Duration,
) -> Result<usize> {
    let deadline = Instant::now() + within;
    loop {
        let (_, lines) = shell.capture(&format!(
            "echo BEIZE-COUNT-$(journalctl --user -u beize-compositor.service --since=@{since} \
             -o cat | grep -cx '{line}')"
        ))?;
        let said: usize = lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT-"))
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("could not count \"{line}\": {lines:?}"))?;
        if said >= count {
            return Ok(said);
        }
        if Instant::now() >= deadline {
            let (_, last) = shell.capture(&format!(
                "journalctl --user -u beize-compositor.service --since=@{since} \
                 -o short-monotonic | grep -v ': pointer at ' | tail -n 30"
            ))?;
            bail!(
                "the compositor said \"{line}\" {said} times, not {count}, within {} s; last \
                 said:\n{}",
                within.as_secs(),
                last.join("\n")
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The VM screen's connector state as the kernel has it: `On` or `Off`.
fn dpms(shell: &Shell) -> Result<String> {
    let (_, lines) = shell.capture(&format!(
        "echo BEIZE-DPMS-$(cat /sys/class/drm/card*-{VM_SCREEN}/dpms)"
    ))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-DPMS-"))
        .map(str::to_owned)
        .with_context(|| format!("no DPMS state: {lines:?}"))
}

/// What the screen shows now, kept in the logs as `name`.
fn screen(machine: &Machine, name: &str) -> Result<Image> {
    let file = screen_file(machine, name);
    screendump(&machine.qmp(), &file)?;
    Image::read_ppm(&fs::read(&file)?)
}

/// The mean of the channels in the middle of the screen, 100 pixels a
/// side: the background, where no window opened.
fn centre_mean(screen: &Image) -> f64 {
    let (x, y) = (screen.width / 2, screen.height / 2);
    let rows = y.saturating_sub(50)..(y + 50).min(screen.height);
    let n = rows.len().max(1) as f64;
    rows.map(|row| {
        row_mean(
            screen,
            row,
            x.saturating_sub(50)..(x + 50).min(screen.width),
        )
    })
    .sum::<f64>()
        / n
}

/// The screen dumps of a passed run go (3 MB each, some 30 a run); a failed
/// run keeps them beside its logs, with the PNGs it wrote for the owner.
fn remove_screens(logs: &Path) -> Result<()> {
    for entry in fs::read_dir(logs)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "ppm") {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
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
    // Drawing all the time, each window waited for its last frame to show
    // before the next (Beize UI's clock): never two on their way. Drawing
    // ahead, they queue behind the compositor's frames – what made resizing
    // trail and keys come late on hardware (0.5.105, Vulkan FIFO with
    // wp_fifo).
    let ahead = lines
        .iter()
        .find_map(|l| {
            l.strip_prefix("load: ")?
                .split_once("at most ")?
                .1
                .split_once(' ')
        })
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("the load test said nothing of frames on their way: {lines:?}"))?;
    ensure!(
        ahead <= 1,
        "a window of the load test had {ahead} frames on their way at once, more than one: it \
         drew ahead of the compositor"
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
        seconds >= (LOAD_SECONDS - 1) as f64,
        "the compositor's last run took {seconds} s, not the load test's {LOAD_SECONDS}: \
         {pacing}"
    );
    let least = drawn.iter().min().copied().unwrap_or(0);
    say!(
        "  ok: the load test's {LOAD_WINDOWS} windows each drew {least} frames or more in \
         {LOAD_SECONDS} s, at most {ahead} on its way, the dock's live glass followed the one \
         behind it; {pacing}"
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
    // The test client's fifth window: the first moved, the second drew
    // frames, the third and fourth on fifo and on time.
    let (at, size) = placed(shell, "beize-testclient", 5)?;
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

/// A client crops and scales its buffer (wp_viewporter): its window has
/// the size it named and shows the part of its pattern it named – the
/// quadrants split at three quarters, not at the halves of the whole
/// buffer – but where scaling blends the seams.
fn viewport(shell: &Shell, machine: &Machine) -> Result<()> {
    const UNIT: &str = "beize-testclient-viewport";
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --viewport --seconds 60 \
         && for i in $(seq 50); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; journalctl --user -u {UNIT} -o cat"
    ))?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure ")),
        "the viewport client got no window (exit status {status}): {lines:?}"
    );
    // The test client's sixth window, after the framed one.
    let (at, size) = placed(shell, "beize-testclient", 6)?;
    ensure!(
        size == VIEWPORT_SIZE,
        "the viewport client's window is {}x{}, not the {}x{} it named",
        size.0,
        size.1,
        VIEWPORT_SIZE.0,
        VIEWPORT_SIZE.1
    );
    let difference = machine.logs.join("viewport-difference.ppm");
    let looked = poll_screen(machine, "viewport", |screen| {
        let window = screen.crop(at, size)?;
        compare(
            &window,
            |x, y| viewported(&window, x, y),
            "the part of the pattern the client named",
            &difference,
        )
    })?;
    if let Err((_, e)) = looked {
        let said = last_said(shell, &[UNIT, "beize-compositor"])?;
        return Err(e.context(format!(
            "the viewport client's window, {}x{} at {},{}, on {}; the client and the \
             compositor last said: {said:?}",
            size.0,
            size.1,
            at.0,
            at.1,
            screen_file(machine, "viewport").display()
        )));
    }
    shell.capture(&format!("systemctl --user stop {UNIT}"))?;
    say!(
        "  ok: a client's viewport cropped and scaled its buffer: {}x{}, the part it named",
        size.0,
        size.1
    );
    Ok(())
}

/// What the viewport client's `window` should show at `x`, `y`: the
/// quadrants split at `VIEWPORT_SPLIT`; beside a split, what it shows.
fn viewported(window: &Image, x: usize, y: usize) -> [u8; 3] {
    let (sx, sy) = VIEWPORT_SPLIT;
    if x.abs_diff(sx) <= VIEWPORT_SEAM || y.abs_diff(sy) <= VIEWPORT_SEAM {
        return window.pixel(x, y);
    }
    QUADRANTS[usize::from(x >= sx) + 2 * usize::from(y >= sy)]
}

/// Apps hand on activation with a token (xdg-activation) while the focus
/// it was made of lasts: a client's second window asks for its first with
/// a token of its own focus and gets it; a token of that focus once the
/// first has the keyboard, and one without a serial, activate nothing.
fn activation(shell: &Shell) -> Result<()> {
    let said = client_summary(
        shell,
        "beize-testclient-activation",
        "--activation",
        "activation no serial: ",
    )?;
    for wanted in [
        "activation active app: granted",
        "activation focus moved: refused",
        "activation no serial: refused",
    ] {
        ensure!(
            said.iter().any(|l| l == wanted),
            "not \"{wanted}\": {said:?}"
        );
    }
    say!(
        "  ok: an app handed on activation with a token of its focus; one of a focus gone, \
         and one without a serial, activated nothing"
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

/// `(x, y)` moved by `(dx, dy)`: where the pointer goes.
fn point((x, y): (usize, usize), (dx, dy): (i32, i32)) -> Result<(i32, i32)> {
    Ok((i32::try_from(x)? + dx, i32::try_from(y)? + dy))
}

/// The pointer's position `at` moved by `(dx, dy)`.
fn by((x, y): (i32, i32), (dx, dy): (i32, i32)) -> (i32, i32) {
    (x + dx, y + dy)
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

/// How many runs of frames the compositor has summed up in this boot.
fn pacing_summaries(shell: &Shell) -> Result<usize> {
    let (_, lines) = shell.capture(&format!("echo BEIZE-SUMMED-$({PACING} | wc -l)"))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SUMMED-")?.parse().ok())
        .with_context(|| format!("no count of the compositor's summaries: {lines:?}"))
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
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let looked = poll_screen(machine, name, |screen| {
        compare(
            &screen.crop(at, size)?,
            |x, y| expected(x, y, size.0, size.1),
            "the client's pattern",
            &difference,
        )
    });
    let Err((_, e)) = looked? else {
        return Ok(());
    };
    let said = last_said(shell, &["beize-testclient", "beize-compositor"])?;
    Err(e.context(format!(
        "the client's window, {}x{} at {},{}, on {}; the client and the compositor last said: \
         {said:?}",
        size.0,
        size.1,
        at.0,
        at.1,
        screen_file(machine, name).display()
    )))
}

/// How often a wait looks at the screen again.
const SCREEN_POLL: Duration = Duration::from_millis(100);

/// Where the screen a wait named `name` looked at last lies.
fn screen_file(machine: &Machine, name: &str) -> PathBuf {
    machine.logs.join(format!("{name}-screen.ppm"))
}

/// Looks at the screen until `check` passes on it – at once, then every
/// `SCREEN_POLL`, for at most `SCREEN_TIMEOUT` – and returns what it gave;
/// if it never does, the last screen and why. Each look is
/// `<name>-screen.ppm` in the logs. Looked at at once, a check must not
/// pass on the screen before what it waits for.
fn poll_screen<T>(
    machine: &Machine,
    name: &str,
    check: impl Fn(&Image) -> Result<T>,
) -> Result<std::result::Result<T, (Image, anyhow::Error)>> {
    let file = screen_file(machine, name);
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        match check(&screen) {
            Ok(value) => return Ok(Ok(value)),
            Err(e) if Instant::now() >= deadline => return Ok(Err((screen, e))),
            Err(_) => thread::sleep(SCREEN_POLL),
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
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let looked = poll_screen(machine, name, |screen| {
        let window = screen.crop(at, size)?;
        match wanted {
            None => Err(anyhow!("there is no {what} yet")),
            Some(r) if (r.width, r.height) != (window.width, window.height) => Err(anyhow!(
                "the window is {}x{}, {what} {}x{}",
                window.width,
                window.height,
                r.width,
                r.height
            )),
            Some(r) => compare(&window, |x, y| r.pixel(x, y), what, &difference),
        }
    });
    let Err((screen, e)) = looked? else {
        return Ok(());
    };
    // The window alone: to look at, and to take over as the reference.
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.crop(at, size)?.write_png(&shown)?;
    Err(e.context(format!("the window is {}", shown.display())))
}

/// Waits until the menu bar lies along the top of the screen with the time
/// on its right, and the dock along the bottom; if they do not, the last
/// screen is in the logs.
fn wait_for_panels(machine: &Machine) -> Result<()> {
    wait_until_screen(machine, "panels", |screen| {
        menu_bar(screen).and_then(|()| dock(screen).map(|_| ()))
    })
}

/// Waits until the screen passes `check`; if it does not, the last screen
/// is `<name>-screen.png` in the logs.
fn wait_until_screen(
    machine: &Machine,
    name: &str,
    check: impl Fn(&Image) -> Result<()>,
) -> Result<()> {
    let Err((screen, e)) = poll_screen(machine, name, check)? else {
        return Ok(());
    };
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.write_png(&shown)?;
    Err(e.context(format!("the screen is {}", shown.display())))
}

/// Whether the bar of the window at `window` shows its circles' marks
/// (`MARKS`): each pixel nearer the mark's colour than the fill's.
fn marks(screen: &Image, window: (usize, usize)) -> Result<()> {
    for Mark {
        at: (x, y),
        fill,
        ink,
    } in MARKS
    {
        let pixel = screen.pixel(window.0 + x, window.1 + y);
        let from = |c: [u8; 3]| -> u32 {
            pixel
                .iter()
                .zip(c)
                .map(|(p, c)| u32::from(p.abs_diff(c)))
                .sum()
        };
        ensure!(
            from(ink) < from(fill),
            "the circle's pixel {x},{y} on the bar shows {pixel:?}, not its mark {ink:?} \
             (the fill is {fill:?})"
        );
    }
    Ok(())
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
/// only rows without tiles lie in its round corners) its sides are the
/// first pair of pixels, mirrored about the middle, darker than what lies
/// outside and inside them – the glass's dark edge, as Apple's; at its
/// round corner the background shows. Its width says how many apps it has;
/// that number.
fn dock(screen: &Image) -> Result<usize> {
    ensure!(
        screen.height > DOCK_ZONE + MENU_BAR,
        "a screen {} high has no room for the dock",
        screen.height
    );
    let top = screen.height - DOCK_ZONE;
    let row = top + DOCK_HEIGHT / 2;
    let centre = screen.width / 2;
    let mean = |x: usize| {
        f64::from(
            screen
                .pixel(x, row)
                .iter()
                .map(|&c| u32::from(c))
                .sum::<u32>(),
        ) / 3.0
    };
    // The edge's pixel at `x` dark, the outside lying `outward` of it.
    let edge = |x: usize, outward: bool| {
        let (out, inside) = if outward {
            (x + RIM_OUT, x - RIM_IN)
        } else {
            (x - RIM_OUT, x + RIM_IN)
        };
        mean(x) + RIM_BELOW <= mean(out).min(mean(inside))
    };
    // Rightwards from the screen's left edge to the first such pair.
    let left = (RIM_OUT..centre - RIM_IN)
        .find(|&x| edge(x, false) && edge(2 * centre - 1 - x, true))
        .ok_or_else(|| anyhow::anyhow!("no dock along the bottom of the screen"))?;
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
/// Resizes Beize UI's window by its right edge as fast as a 1000 Hz mouse
/// reports, out by `FAST_RESIZE_BY` and back: it has to settle in its size
/// at once after the button is up, not draw the hundreds of sizes in
/// between one frame after another (0.5.105 on the owner's PC: the window
/// trailed the pointer ever more, until its buffers filled the GPU).
fn fast_resize(shell: &Shell, qmp: &Path, screen: (usize, usize), window: Window) -> Result<()> {
    let ((x, y), (w, h)) = window;
    let edge = (i32::try_from(x + w + 2)?, i32::try_from(y + h / 2)?);
    let out = (edge.0 + FAST_RESIZE_BY, edge.1);
    swipe(qmp, screen, &[edge, out, edge], Duration::from_millis(1))?;
    park(qmp, screen)?;
    let wanted = format!("resized beize-ui-test to {w}x{h}, ");
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 50); do journalctl --user -b -u beize-compositor.service -o cat \
         | grep -q '^{wanted}' && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep '^resized ' | tail -n 1"
    ))?;
    let said = lines
        .iter()
        .find(|l| l.starts_with("resized "))
        .with_context(|| format!("the compositor ended no resize: {lines:?}"))?;
    let ms: u64 = said
        .strip_prefix(&wanted)
        .and_then(|rest| rest.split_once(" ms"))
        .and_then(|(ms, _)| ms.parse().ok())
        .with_context(|| format!("the window did not settle in {w}x{h}: \"{said}\""))?;
    ensure!(
        ms <= FAST_RESIZE_SETTLES.as_millis() as u64,
        "resized fast, the window settled {ms} ms after the button was released, more than \
         {} ms: it drew the sizes it was dragged through one by one",
        FAST_RESIZE_SETTLES.as_millis()
    );
    say!(
        "  ok: resized as fast as a 1000 Hz mouse, out by {FAST_RESIZE_BY} px and back, the \
         window settled {ms} ms after the button was released"
    );
    Ok(())
}

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
    let looked = poll_screen(machine, name, |screen| {
        ensure!(
            screen.crop(at, size)?.pixels != shown.pixels,
            "the window still shows"
        );
        Ok(())
    })?;
    let Err((screen, e)) = looked else {
        return Ok(());
    };
    let png = machine.logs.join(format!("{name}-screen.png"));
    screen.crop(at, size)?.write_png(&png)?;
    Err(e.context(png.display().to_string()))
}

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
    wait_until_screen(machine, "live-glass", |screen| {
        let shown = screen.pixel(at.0, at.1);
        ensure!(
            shown
                .iter()
                .zip(before)
                .any(|(&a, b)| a.abs_diff(b) > GLASS_SHOWS),
            "the dock's glass shows {shown:?} at {},{} with a window behind it, as {before:?} \
             without: it is not live",
            at.0,
            at.1
        );
        Ok(())
    })
}

/// Waits until the dock's glass at `at` shows `before` again, to the
/// rounding (`CHANNEL_TOLERANCE`): the window gone from behind it. The
/// screen then is in the logs as `glass-back-screen.png`.
fn wait_for_glass_back(machine: &Machine, at: (usize, usize), before: [u8; 3]) -> Result<()> {
    wait_until_screen(machine, "glass-back", |screen| {
        let shown = screen.pixel(at.0, at.1);
        ensure!(
            shown
                .iter()
                .zip(before)
                .all(|(&a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE),
            "the dock's glass shows {shown:?} at {},{} with the window gone, not {before:?} as \
             before it came: it blurred what the screen held there",
            at.0,
            at.1
        );
        Ok(())
    })
}

/// Waits until the dock shows the test client's icon in the middle of its
/// tile at `at`; if it does not, the last screen is in the logs.
fn wait_for_dock_icon(machine: &Machine, at: (i32, i32)) -> Result<()> {
    let (x, y) = (usize::try_from(at.0)?, usize::try_from(at.1)?);
    wait_until_screen(machine, "dock-icon", |screen| {
        let shown = screen.pixel(x, y);
        ensure!(
            shown
                .iter()
                .zip(CLIENT_ICON_COLOR)
                .all(|(&a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE),
            "the dock shows {shown:?} at {x},{y}, not the test client's icon {CLIENT_ICON_COLOR:?}"
        );
        Ok(())
    })
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
                // The dark edge at the sides, the line of light at the top.
                let side = (x - x0).min(x0 + width - 1 - x) == 0;
                let rim = y - y0 == 1;
                let c = if corner {
                    BACKGROUND
                } else if side {
                    [0x50, 0x58, 0x70]
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
        // Room for the app's name and the date.
        let w = 600;
        let screen = desktop(w);
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
        let mut stray = desktop(w);
        stray.pixels[(5 * w + 300) * 3] = 0;
        assert!(menu_bar(&stray).is_err());
        let none = Image {
            pixels: BACKGROUND.repeat(w * 40),
            ..desktop(w)
        };
        assert!(menu_bar(&none).is_err());
        let mut with_background = desktop(w);
        for x in 0..360 {
            for y in 0..MENU_BAR {
                let at = (y * w + x) * 3;
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
