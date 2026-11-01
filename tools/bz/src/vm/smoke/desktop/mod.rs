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

mod frames;
mod glass;
mod image;
mod menus;
mod session;
mod wait;
mod windows;

use self::frames::*;
use self::glass::*;
use self::image::*;
use self::menus::*;
use self::session::*;
use self::wait::*;
use self::windows::*;

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
/// Where the active app's name and its menus are, the system's Window with
/// them: the left end of the bar, at most this wide (the shell's journal says
/// which name it shows, `SHELL_SAID`).
const APP_NAME_WIDTH: usize = 340;
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
/// Of the compositor's warnings, those that say something here. A page flip
/// later than its watch, 1 s, that did arrive is the VM's vCPUs standing (#41;
/// 1079 ms on 1.11.), no driver's fault; one that never arrives stays red.
fn unexpected_warnings(lines: &[String]) -> Vec<&String> {
    let arrived = lines
        .iter()
        .any(|l| l.starts_with("page flip arrived after all"));
    lines
        .iter()
        .filter(|l| {
            !l.is_empty()
                && !KNOWN_WARNINGS.contains(&l.as_str())
                && !(arrived
                    && (l.starts_with("page flip timed out")
                        || l.starts_with("page flip arrived after all")))
        })
        .collect()
}

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
const SCREEN_FADE: Duration = Duration::from_secs(3);
const SCREEN_OFF_SLACK: Duration = Duration::from_secs(5);
/// How long anything awaited may take, at most: long, as a wait that comes
/// costs nothing, and the VM's vCPUs can stand still for 21 s (#41). No check
/// here asserts a pace (`beize-no-vm-timing`): that is measured on hardware.
const DEADLINE: Duration = Duration::from_secs(60);
/// By how much the middle of the desktop (the background: no window) gets
/// darker in the fade, at least, in the mean of its channels: 30 % black over
/// a background of some 150 takes 45.
const FADE_DARKER: f64 = 20.0;
/// How long a client draws with the screen off, and how many frames at most:
/// its first two come without a wait, then one each `HIDDEN_FRAMES`
/// (os/beize-compositor/src/state.rs), and 3 s hold three or four of those,
/// by where they begin: 5 here, 6 in the nights of 28. to 31.10. A stall only
/// makes fewer. At the screen's pace it was 188.
const OFF_DRAWING: Duration = Duration::from_secs(3);
const OFF_FRAMES_MOST: usize = 6;

/// The load test's scene (os/beize-ui, `beize-load-test`): its windows, how
/// long it runs, and how many frames each window must have drawn at least –
/// one: it draws at all; how fast is measured on hardware, as a stand of the
/// VM's vCPUs (#41) makes any pace here a guess. Long enough for the drag behind the dock
/// and the glass's check; what it measures counts on hardware only, which
/// runs it longer (`measure/hw-test.sh`).
const LOAD_WINDOWS: usize = 12;
const LOAD_SECONDS: u64 = 6;
const LOAD_LEAST_FRAMES: usize = 1;
/// How far above the dock the test drags the scene's top window by its
/// bar: the window's content then lies behind the dock.
const LOAD_ABOVE_DOCK: usize = 150;

/// How long a screen may take to become what its reference shows.
const SCREEN_TIMEOUT: Duration = DEADLINE;

/// By when the compositor has ended a run of frames after its last one:
/// 250 ms without a frame (`IDLE`, os/beize-compositor/src/pacing.rs),
/// looked for every 500 ms (`PACING_CHECK`, state.rs), with room.
const RUN_ENDED: Duration = Duration::from_secs(1);
/// How long the still desktop is watched for waking.
const IDLE_SECONDS: u64 = 3;
/// How long the minute's frame is still at work when a minute began. The window
/// must not begin in it: 1 in 20 runs landed there and read as "the desktop woke"
/// (measured 1.11.: 4 to 29 task switches in the 3 s from :00, 0 from :10). The
/// bar's clock draws, and its frame arms the timer that sums up frames
/// (`PACING_CHECK`, 500 ms).
const AFTER_TICK: u64 = 3;

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

/// Says how long each part of the desktop's scenario took: where the smoke
/// test's minutes go.
struct Phases(Instant);

impl Phases {
    fn done(&mut self, part: &str) {
        say!("  time: {part} {:.1} s", self.0.elapsed().as_secs_f64());
        self.0 = Instant::now();
    }
}

pub(super) fn desktop(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut phases = Phases(Instant::now());
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
    phases.done("boot and sign-in");
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
        "for i in $(seq 300); do journalctl --user -u beize-testclient -o cat | grep -q '^configure ' \
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
    phases.done("client windows");
    snap(&shell, &machine, &qmp, screen, moved, grown)?;
    phases.done("snap");
    switcher(&shell, &machine, &qmp, screen)?;
    phases.done("switcher");
    fullscreen(&shell, &machine, &qmp, screen, moved, grown)?;
    phases.done("fullscreen");
    dock_glass(&shell, &machine, &qmp, screen, moved, grown)?;
    phases.done("dock glass");
    frames(&shell)?;
    phases.done("frames");
    // Beize UI: its window through Vulkan's WSI shows the sample as the
    // reference has it – the same renderer, glyphs and blending offscreen.
    let (_, lines) = shell.capture(
        "systemctl --user stop beize-testclient \
         && systemd-run --user --unit=beize-ui-test /run/t/beize-ui-test --seconds 60 \
         && for i in $(seq 300); do journalctl --user -u beize-ui-test -o cat \
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
        "for i in $(seq 300); do systemctl --user -q is-active beize-ui-test || break; \
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
    hidden_window(&shell, &qmp)?;
    not_forward(&shell)?;
    protocol_errors(&shell)?;
    grabs(&shell, &qmp)?;
    load(&shell, &machine, &qmp, screen)?;
    phases.done("Beize UI and load");

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
    phases.done("menus");
    context_menus(&shell, &machine, &qmp, screen)?;
    phases.done("context menus");
    confirmation(&shell, &machine, &qmp, screen)?;
    phases.done("confirmation");
    app_menus(&shell, &machine, &qmp, screen)?;
    phases.done("app menus");
    layout_follows(&shell)?;
    phases.done("layout");
    still_and_signed_out(&shell, &qmp, screen)?;
    phases.done("still and sign-out");
    screen_goes_off(&shell, &machine, &qmp)?;
    phases.done("screen off");
    journal.passed = true;
    drop(journal);
    shell.stop()?;
    remove_screens(&machine.logs)?;
    machine.remove()
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
        let w = 700;
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
        stray.pixels[(5 * w + 380) * 3] = 0;
        assert!(menu_bar(&stray).is_err());
        let none = Image {
            pixels: BACKGROUND.repeat(w * 40),
            ..desktop(w)
        };
        assert!(menu_bar(&none).is_err());
        let mut with_background = desktop(w);
        for x in 0..w - CLOCK_WIDTH {
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
