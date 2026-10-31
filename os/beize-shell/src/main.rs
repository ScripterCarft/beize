//! `beize-shell` – Beize's shell (ADR 0028): the menu bar along the top
//! of the screen with the Beize menu and the control center, and the dock
//! along the bottom. A client of the compositor with Beize UI, the only one
//! it lets draw panels (wlr-layer-shell) and see the windows (beize-shell-v1,
//! ADR 0028 §1): both panels and their popups are windows of one app. Runs
//! as `beize-shell.service` in the user's desktop, restarted if it fails;
//! it ends with the session.
//!
//! The bar: macOS' menu bar in Beize's measures (`measures`) – Beize's mark
//! on the left, which opens the Beize menu, the active app's name beside it
//! in bold; on the right the network's symbol, the control center's, and
//! the date and time. As macOS' since 26 it has no background: white ink on
//! the scrim the compositor lays behind it, dark only where the background
//! stays very light, as the compositor measures it (ADR 0028 §3). With the
//! background switched on (`BarLook::Glass`, macOS' "Show menu bar
//! background") the bar lies on base glass instead, its ink for the
//! appearance. An open menu's button lies on a faint pill, as macOS marks
//! it. The dock lies on live glass, which the compositor draws below it
//! (§6).
//!
//! The Beize menu (`menu`, as macOS' Apple menu): sleep, restart, shut down
//! and log out, through logind (`beize_systemd::login`). Locking comes with
//! the lock screen (its own ADR); "About This Computer" and the settings
//! with their windows. Restart, Shut Down and Log Out ask first, as macOS'
//! with their ellipses (`beize_ui::dialog`), without its countdown: nothing
//! happens unattended. Sleep acts at once.

mod apps;
mod clock;
mod control_center;
mod dock;
mod network;

use std::collections::HashMap;
use std::process::ExitCode;

use beize_systemd::login::{self, Power};
use beize_ui::dialog::{self, Dialog};
use beize_ui::draw::Cap;
use beize_ui::input::{Button, KeyState, Pointer};
use beize_ui::menu::{Entry, Menu, Outcome};
use beize_ui::{
    Align, App, Appearance, Color, Edge, Family, Glass, Input, Material, Overlay, Panel, Popup,
    Rect, Scene, Style, Text, Watch, WindowId, measures,
};
use tracing::{info, warn};

use control_center::ControlCenter;
use dock::{Action, Dock};

const HEIGHT: u32 = measures::MENU_BAR;
/// The bar's text on a light background and on a dark one.
const INK_LIGHT: Color = Color::rgb(0x1d1d1d);
const INK_DARK: Color = Color::rgb(0xffffff);
/// Its size, macOS' 13 a little larger (the owner chose between 15 and
/// 16.25, macOS' at `SCALE`), and the space at the bar's ends: macOS' 12
/// at `SCALE` was 15, the owner wanted the ends further in (28.10.: 20).
/// The control center's right edge lies on it too, under the clock's.
const TEXT_SIZE: f32 = 15.0;
const MARGIN: f32 = 20.0;
/// The bar's symbols: Beize's mark and Phosphor's, as the mock-ups the
/// owner approved (28.10.); the space between the mark and the app's name.
/// The globe larger, its circle about 16 px (the owner's, 28.10.).
const SYMBOL: u32 = 18;
const SYMBOL_GAP: f32 = 22.0;
const GLOBE: u32 = 20;
/// The pill below an open menu's button, round at the ends, flat, no
/// glass: light grey at 34 % below white ink – lighter than the bar, as
/// Apple's (its open control-center button: the backdrop about 1.45 times as
/// light, measured), greyer and more opaque than white at 24 % so it shows,
/// the owner's on hardware, 31.10. –, black at 10 % below dark
/// ink, where the bar is light. 36 pt wide under the Beize menu (measured in
/// the HIG's menu-bar image); under the control center's the owner's
/// 54 × 28 px (28.10.; Apple's 46.5 × 24 pt at `SCALE`, 58 × 30, filled
/// the bar). The control center's button is its pill: the clock and the
/// globe keep Apple's space from it, about 6 pt to their ink in Apple's
/// image of it (2 px per pt), at `SCALE`.
const PILL_HEIGHT: f32 = 28.0;
const MENU_PILL: f32 = 36.0 * measures::SCALE;
const CONTROL_CENTER_PILL: f32 = 54.0;
const BUTTON_SPACE: f32 = 8.0;

/// What lies behind the menu bar (ADR 0028 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarLook {
    /// No background, as macOS' since 26.
    Transparent,
    /// Base glass, as macOS' with "Show menu bar background".
    Glass,
}

impl BarLook {
    /// Transparent, as macOS starts. Until Beize's settings hold the
    /// choice, `BEIZE_MENU_BAR=glass` in the desktop's environment shows
    /// the background (like `BEIZE_APPEARANCE`).
    fn from_environment() -> BarLook {
        match std::env::var("BEIZE_MENU_BAR").as_deref() {
            Ok("glass") => BarLook::Glass,
            _ => BarLook::Transparent,
        }
    }
}

/// Which of the bar's menus is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    BeizeMenu,
    ControlCenter,
    DockMenu,
}

/// The Beize menu's commands, in its entries' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Power(Power),
    LogOut,
}

fn main() -> ExitCode {
    beize_ui::log::init("BEIZE_SHELL_LOG");
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut app = App::connect().map_err(|e| e.to_string())?;
    let bar = app
        .panel(&Panel {
            namespace: "menu-bar",
            edge: Edge::Top,
            width: 0,
            height: HEIGHT,
            margin: 0,
            translucent: true,
        })
        .map_err(|e| e.to_string())?;
    // Names and icons from the apps' desktop files, read once each.
    let mut known: HashMap<String, apps::App> = HashMap::new();
    let mut names = |app_id: &str| {
        known
            .entry(app_id.to_owned())
            .or_insert_with(|| apps::app(app_id, dock::ICON_PIXELS))
            .clone()
    };
    let mut dock = Dock::new(&mut names);
    let dock_panel = app
        .panel(&Panel {
            namespace: "dock",
            edge: Edge::Bottom,
            width: dock.width(),
            height: dock::HEIGHT,
            margin: dock::MARGIN,
            translucent: true,
        })
        .map_err(|e| e.to_string())?;
    app.watch_desktop().map_err(|e| e.to_string())?;
    info!("menu bar and dock on {}", app.gpu_name());
    let minutes = clock::Minutes::new().map_err(|e| format!("the minute's timer: {e}"))?;
    // Without it the network shows as it was at the start.
    let monitor = network::Monitor::new()
        .inspect_err(|e| warn!("watching the network: {e}"))
        .ok();
    let mut network = network::Status::now();
    info!("network: {network:?}");
    // Each panel is drawn once the compositor has configured it, and the
    // dock again only in the size it asked for last.
    let (mut bar_shown, mut dock_shown) = (false, false);
    // Once both are drawn, the compositor shows the desktop (ADR 0028 §6).
    let (mut bar_drawn, mut dock_drawn, mut ready) = (false, false, false);
    let mut dock_width = dock.width();
    let mut active_name: Option<String> = None;
    let mut appearance = app.appearance();
    let mut backdrop = app.menu_bar_backdrop();
    let look = BarLook::from_environment();
    info!("menu bar {look:?}");
    // What changed since a panel's last frame, drawn once it is ready.
    let (mut redraw_bar, mut redraw_dock) = (false, false);
    // The time the bar shows: the clock set within the minute (NTS at
    // boot) changes nothing to draw.
    let mut shown_time = None;
    // The bar's buttons as last drawn, and where the pointer is on it.
    let mut buttons = Buttons::default();
    let mut on_bar: Option<(f64, f64)> = None;
    // The open menu: which, its popup, and whether it waits to be drawn.
    let mut open: Option<(Open, WindowId)> = None;
    let mut redraw_open = false;
    let mut beize_menu = Menu::new(beize_menu_entries());
    let control_center = ControlCenter::new();
    // The dock tile's menu while it is open, with its entries' commands.
    let mut dock_menu: Option<(Menu, Vec<Option<dock::Command>>)> = None;
    let mut on_menu: Option<(f64, f64)> = None;
    // The confirmation open, what it confirms, and whether it waits to be
    // drawn.
    let mut asking: Option<(WindowId, Dialog, Command)> = None;
    let mut redraw_asking = false;
    loop {
        let mut watches = vec![Watch::Read(minutes.fd())];
        watches.extend(monitor.as_ref().map(|m| Watch::Read(m.fd())));
        let mut dispatched = app.dispatch(None, &watches).map_err(|e| e.to_string())?;
        drop(watches);
        let bar_events = dispatched.take(bar);
        let dock_events = dispatched.take(dock_panel);
        let open_events = open.map(|(_, id)| dispatched.take(id));
        let asking_events = asking.as_ref().map(|(id, ..)| dispatched.take(*id));
        if bar_events.closed || dock_events.closed {
            // Its screen went: a failure, so the service manager starts the
            // shell again.
            return Err("the compositor closed a panel".into());
        }
        let tick = dispatched.ready.contains(&0);
        if tick {
            minutes
                .take()
                .map_err(|e| format!("the minute's timer: {e}"))?;
        }
        if let Some(monitor) = &monitor
            && dispatched.ready.contains(&1)
        {
            monitor.take();
            let now = network::Status::now();
            if now != network {
                info!("network: {now:?}");
                network = now;
                redraw_bar = true;
                redraw_open |= open.is_some_and(|(o, _)| o == Open::ControlCenter);
            }
        }
        redraw_bar |= bar_events.configured || (tick && clock::now() != shown_time);
        redraw_dock |= dock_events.configured;
        if app.appearance() != appearance {
            appearance = app.appearance();
            info!("appearance {appearance:?}");
            redraw_bar = true;
            redraw_dock = true;
            redraw_open = open.is_some();
        }
        if app.menu_bar_backdrop() != backdrop {
            backdrop = app.menu_bar_backdrop();
            info!("menu bar on a {backdrop:?} background");
            redraw_bar = true;
        }
        if dispatched.desktop {
            let windows = app.desktop_windows();
            let before = dock.summary();
            dock.update(&windows, &mut names);
            if dock.summary() != before {
                info!("dock: {}", dock.summary());
                redraw_dock = true;
            }
            let name = windows
                .iter()
                .find(|w| w.active)
                .map(|w| names(&w.app_id).name);
            if name != active_name {
                info!("menu bar: {}", name.as_deref().unwrap_or("(no app)"));
                active_name = name;
                redraw_bar = true;
            }
        }
        // The open menu: what happened to it.
        if let (Some((which, id)), Some(events)) = (open, open_events) {
            redraw_open |= events.configured;
            let mut close = events.closed;
            let mut chosen = None;
            let mut tile_command = None;
            let width = app.size(id).0;
            for input in &events.input {
                if let Input::Pointer(Pointer::Enter(x, y) | Pointer::Motion(x, y)) = input {
                    on_menu = Some((*x, *y));
                }
                match which {
                    Open::BeizeMenu => match beize_menu.input(input, width) {
                        Outcome::None => {}
                        Outcome::Changed => redraw_open = true,
                        Outcome::Dismissed => close = true,
                        Outcome::Chosen(i) => {
                            close = true;
                            chosen = command_at(i);
                        }
                    },
                    Open::DockMenu => {
                        if let Some((menu, commands)) = &mut dock_menu {
                            match menu.input(input, width) {
                                Outcome::None => {}
                                Outcome::Changed => redraw_open = true,
                                Outcome::Dismissed => close = true,
                                Outcome::Chosen(i) => {
                                    close = true;
                                    tile_command = commands.get(i).cloned().flatten();
                                }
                            }
                        }
                    }
                    Open::ControlCenter => {
                        if let Input::Pointer(Pointer::Button {
                            button: Button::Left,
                            pressed: true,
                        }) = input
                            && let Some(at) = on_menu
                            && let Some(control_center::Action::Appearance(chosen)) =
                                control_center.click(at, appearance)
                        {
                            info!("control center: appearance {chosen:?}");
                            app.choose_appearance(chosen);
                        }
                        // Pressed, as the Beize menu's (`Menu::input`): the
                        // release of an Escape that closed that menu comes
                        // here when this opened while it was held.
                        if let Input::Key(key) = input
                            && key.sym == beize_ui::Keysym::Escape
                            && key.state != KeyState::Released
                        {
                            close = true;
                        }
                    }
                }
            }
            if close {
                info!("{which:?} closed");
                app.close(id);
                open = None;
                on_menu = None;
                beize_menu = Menu::new(beize_menu_entries());
                dock_menu = None;
                redraw_bar = true;
                match tile_command {
                    Some(dock::Command::Activate(window)) => app.activate_desktop_window(window),
                    Some(dock::Command::Start(app_id)) => beize_systemd::apps::start(&app_id),
                    Some(dock::Command::Quit(windows)) => {
                        for window in windows {
                            app.close_desktop_window(window);
                        }
                    }
                    None => {}
                }
                // Gone from the screen before logind acts – it may take a
                // while, and sleep would keep the menu to the wake.
                app.flush().map_err(|e| e.to_string())?;
                match chosen {
                    Some(Command::Power(Power::Sleep)) => choose(chosen),
                    Some(command) if asking.is_none() => {
                        let dialog = confirmation(command);
                        let overlay = Overlay {
                            namespace: "confirmation",
                            size: dialog.size(app.text()),
                            keyboard: true,
                        };
                        match app.overlay(&overlay) {
                            Ok(id) => {
                                info!("asking: {command:?}");
                                asking = Some((id, dialog, command));
                                redraw_asking = false;
                            }
                            Err(e) => warn!("asking: {e}"),
                        }
                    }
                    _ => {}
                }
            }
        }
        // The confirmation: its answer.
        if let Some(events) = asking_events
            && let Some((id, dialog, command)) = &mut asking
        {
            redraw_asking |= events.configured;
            let mut answer = events.closed.then_some(false);
            let size = app.size(*id);
            for input in &events.input {
                match dialog.input(input, size) {
                    dialog::Outcome::None => {}
                    dialog::Outcome::Changed => redraw_asking = true,
                    dialog::Outcome::Confirmed => answer = Some(true),
                    dialog::Outcome::Cancelled => answer = Some(false),
                }
            }
            if let Some(answer) = answer {
                let (id, command) = (*id, *command);
                info!("asked: {command:?}, {}", if answer { "yes" } else { "no" });
                app.close(id);
                asking = None;
                app.flush().map_err(|e| e.to_string())?;
                if answer {
                    choose(Some(command));
                }
            }
        }
        // A press elsewhere in the shell closes the open menu, as macOS'
        // (the compositor's grab closes it only for other apps' surfaces).
        let pressed = |events: &beize_ui::Events| {
            events.input.iter().any(|i| {
                matches!(
                    i,
                    Input::Pointer(Pointer::Button {
                        button: Button::Left | Button::Right,
                        pressed: true,
                    })
                )
            })
        };
        let was = open.map(|(o, _)| o);
        if (pressed(&bar_events) || pressed(&dock_events))
            && let Some((which, id)) = open.take()
        {
            info!("{which:?} closed");
            app.close(id);
            on_menu = None;
            beize_menu = Menu::new(beize_menu_entries());
            dock_menu = None;
            redraw_bar = true;
        }
        // The bar's buttons: a press opens their menu, or closes it.
        for input in &bar_events.input {
            let Input::Pointer(pointer) = input else {
                continue;
            };
            match *pointer {
                Pointer::Enter(x, y) | Pointer::Motion(x, y) => on_bar = Some((x, y)),
                Pointer::Leave => on_bar = None,
                Pointer::Button {
                    button: Button::Left,
                    pressed: true,
                } => {
                    let Some(at) = on_bar else {
                        continue;
                    };
                    let Some((pressed, anchor, align)) = buttons.at(at) else {
                        continue;
                    };
                    // Its own button again: closed above, it stays closed.
                    if was == Some(pressed) {
                        continue;
                    }
                    let size = match pressed {
                        Open::ControlCenter => control_center.size(),
                        _ => beize_menu.size(app.text()),
                    };
                    let popup = Popup {
                        anchor,
                        size,
                        align,
                        above: false,
                    };
                    match app.popup(bar, &popup) {
                        Ok(id) => {
                            info!("{pressed:?} open");
                            if pressed == Open::ControlCenter {
                                app.set_input(id, Some(&control_center.input()));
                            }
                            open = Some((pressed, id));
                            redraw_open = false;
                            redraw_bar = true;
                        }
                        Err(e) => warn!("opening {pressed:?}: {e}"),
                    }
                }
                _ => {}
            }
        }
        for input in &dock_events.input {
            let Input::Pointer(pointer) = input else {
                continue;
            };
            match *pointer {
                Pointer::Enter(x, y) | Pointer::Motion(x, y) => dock.hover(Some((x, y))),
                Pointer::Leave => dock.hover(None),
                Pointer::Button {
                    button: Button::Left,
                    pressed: true,
                } => redraw_dock |= dock.press(),
                // A tile's menu, above it.
                Pointer::Button {
                    button: Button::Right,
                    pressed: true,
                } => {
                    let Some(tile) = dock.menu() else {
                        continue;
                    };
                    let menu = Menu::new(tile.entries);
                    let popup = Popup {
                        anchor: tile.tile,
                        size: menu.size(app.text()),
                        align: Align::Centre,
                        above: true,
                    };
                    match app.popup(dock_panel, &popup) {
                        Ok(id) => {
                            info!("{:?} open", Open::DockMenu);
                            dock_menu = Some((menu, tile.commands));
                            open = Some((Open::DockMenu, id));
                            redraw_open = false;
                        }
                        Err(e) => warn!("opening a tile's menu: {e}"),
                    }
                }
                Pointer::Button {
                    button: Button::Left,
                    pressed: false,
                } => {
                    redraw_dock = true;
                    match dock.release() {
                        Some(Action::Start(app_id)) => beize_systemd::apps::start(&app_id),
                        Some(Action::Activate(windows)) => {
                            for window in windows {
                                app.activate_desktop_window(window);
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        if dock.width() != dock_width {
            // Drawn once the compositor configures the new width.
            dock_width = dock.width();
            app.set_panel_size(dock_panel, (dock_width, dock::HEIGHT));
            redraw_dock = false;
        }
        bar_shown |= bar_events.configured;
        if bar_shown && redraw_bar && app.ready(bar) {
            redraw_bar = false;
            let size = app.size(bar);
            // On glass the ink follows the appearance, as the dock's.
            let behind = match look {
                BarLook::Transparent => backdrop,
                BarLook::Glass => {
                    app.set_glass(bar, Some(bar_glass(size)));
                    appearance
                }
            };
            shown_time = clock::now();
            let (scene, drawn) = menu_bar(
                app.text(),
                size,
                &BarContent {
                    app: active_name.as_deref(),
                    time: shown_time.clone(),
                    network,
                    open: open.map(|(o, _)| o),
                },
                behind,
            );
            if drawn.centres() != buttons.centres() {
                // Where to click, for tests (the clock's width moves the
                // control center's).
                let [beize, centre] = drawn.centres();
                info!("menu bar buttons: Beize menu at {beize:?}, control center at {centre:?}");
            }
            buttons = drawn;
            app.draw(bar, &scene).map_err(|e| e.to_string())?;
            bar_drawn = true;
        }
        if let Some((which, id)) = open
            && redraw_open
            && app.ready(id)
        {
            redraw_open = false;
            let size = app.size(id);
            let (glass, scene) = match which {
                Open::BeizeMenu => (
                    Menu::glass(size),
                    beize_menu.draw(app.text(), size, appearance),
                ),
                Open::DockMenu => {
                    let Some((menu, _)) = &dock_menu else {
                        continue;
                    };
                    (Menu::glass(size), menu.draw(app.text(), size, appearance))
                }
                Open::ControlCenter => (
                    control_center.glass(),
                    control_center.draw(app.text(), network, appearance),
                ),
            };
            app.set_glass(id, Some(glass));
            app.draw(id, &scene).map_err(|e| e.to_string())?;
        }
        if let Some((id, dialog, _)) = &asking
            && redraw_asking
            && app.ready(*id)
        {
            redraw_asking = false;
            let id = *id;
            let size = app.size(id);
            app.set_glass(id, Some(Dialog::glass(size)));
            let scene = dialog.draw(app.text(), size, appearance);
            app.draw(id, &scene).map_err(|e| e.to_string())?;
        }
        dock_shown |= dock_events.configured;
        if dock_shown && redraw_dock && app.ready(dock_panel) {
            redraw_dock = false;
            let size = app.size(dock_panel);
            app.set_glass(dock_panel, Some(Dock::glass(size)));
            let scene = dock.draw(app.text(), size, appearance);
            app.draw(dock_panel, &scene).map_err(|e| e.to_string())?;
            dock_drawn = true;
        }
        if bar_drawn && dock_drawn && !ready {
            app.desktop_ready();
            ready = true;
            info!("ready: menu bar and dock drawn");
            // Once, for the journal, after the desktop shows: what logind
            // lets the Beize menu do.
            info!("Beize menu may: {}", may());
        }
    }
}

/// The Beize menu, as macOS' Apple menu has these: the computer's state,
/// then the user's session. Log Out names the user, as macOS does.
fn beize_menu_entries() -> Vec<Entry> {
    vec![
        Entry::item("Sleep"),
        Entry::item("Restart…"),
        Entry::item("Shut Down…"),
        Entry::Separator,
        Entry::item(format!("Log Out {}…", user_name())),
    ]
}

/// The command of the Beize menu's entry `i` (`beize_menu_entries`).
fn command_at(i: usize) -> Option<Command> {
    match i {
        0 => Some(Command::Power(Power::Sleep)),
        1 => Some(Command::Power(Power::Restart)),
        2 => Some(Command::Power(Power::ShutDown)),
        4 => Some(Command::LogOut),
        _ => None,
    }
}

/// What asks before `command` happens.
fn confirmation(command: Command) -> Dialog {
    let (title, action) = match command {
        Command::Power(Power::Restart) => ("Restart this computer?", "Restart"),
        Command::Power(_) => ("Shut down this computer?", "Shut Down"),
        Command::LogOut => ("Log out?", "Log Out"),
    };
    Dialog::new(title, "Unsaved work in open windows may be lost.", action)
}

/// What logind lets the Beize menu do now, for the journal: polkit's
/// answer for this session ("yes" without a password).
fn may() -> String {
    [
        ("sleep", Power::Sleep),
        ("restart", Power::Restart),
        ("shut down", Power::ShutDown),
    ]
    .iter()
    .map(|(name, power)| {
        let answer = login::can(*power).unwrap_or_else(|e| format!("({e})"));
        format!("{name} {answer}")
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// Does what the Beize menu chose.
fn choose(command: Option<Command>) {
    let Some(command) = command else {
        return;
    };
    info!("Beize menu: {command:?}");
    let done = match command {
        Command::Power(power) => login::power(power),
        Command::LogOut => login::log_out(),
    };
    if let Err(e) = done {
        warn!("{command:?}: {e}");
    }
}

/// The user's name as the account has it (its full name, else the login),
/// as macOS names the user in "Log Out".
fn user_name() -> String {
    // SAFETY: getpwuid's entry is read at once, before any other call to it
    // (the shell has one thread).
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return String::new();
        }
        let field = |p: *const libc::c_char| {
            (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
        };
        let full = field((*entry).pw_gecos)
            .and_then(|g| g.split(',').next().map(str::to_owned))
            .filter(|g| !g.is_empty());
        full.or_else(|| field((*entry).pw_name)).unwrap_or_default()
    }
}

/// The bar's glass (`BarLook::Glass`): all of it, square, without a rim –
/// it runs along the screen's edge –, base glass: no window lies behind it.
fn bar_glass((w, h): (u32, u32)) -> Glass {
    let all = Rect {
        x: 0.0,
        y: 0.0,
        w: w as f32,
        h: h as f32,
    };
    Glass::one(all, 0.0, false, Material::Base)
}

/// What the bar shows.
struct BarContent<'a> {
    app: Option<&'a str>,
    time: Option<String>,
    network: network::Status,
    open: Option<Open>,
}

/// The bar's buttons as drawn: where each lies (its pill), for a press.
#[derive(Default)]
struct Buttons {
    beize_menu: Option<Rect>,
    control_center: Option<Rect>,
    width: f32,
}

impl Buttons {
    /// Each button's centre, in whole pixels.
    fn centres(&self) -> [Option<(i32, i32)>; 2] {
        let centre = |r: Option<Rect>| {
            r.map(|r| {
                (
                    (r.x + r.w / 2.0).round() as i32,
                    (r.y + r.h / 2.0).round() as i32,
                )
            })
        };
        [centre(self.beize_menu), centre(self.control_center)]
    }

    /// The button at `(x, y)` on the bar, what its menu opens below and
    /// how it lines up: the Beize menu from its button's left edge, the
    /// control center a gap from the screen's right edge.
    fn at(&self, (x, y): (f64, f64)) -> Option<(Open, Rect, Align)> {
        let (x, y) = (x as f32, y as f32);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= 0.0 && y < HEIGHT as f32;
        if let Some(r) = self.beize_menu.filter(inside) {
            let anchor = Rect {
                x: r.x,
                y: 0.0,
                w: r.w,
                h: HEIGHT as f32,
            };
            return Some((Open::BeizeMenu, anchor, Align::Left));
        }
        if self.control_center.is_some_and(|r| inside(&r)) {
            let right = self.width - MARGIN;
            let anchor = Rect {
                x: right - 1.0,
                y: 0.0,
                w: 1.0,
                h: HEIGHT as f32,
            };
            return Some((Open::ControlCenter, anchor, Align::Right));
        }
        None
    }
}

/// The bar in `size`, on a `backdrop` light or dark: Beize's mark and the
/// active app's name on the left, the network, the control center and the
/// time on the right, all centred in the height; the rest transparent. And
/// where its buttons lie.
fn menu_bar(
    text: &mut Text,
    (w, h): (u32, u32),
    content: &BarContent<'_>,
    backdrop: Appearance,
) -> (Scene, Buttons) {
    let (w, h) = (w as f32, h as f32);
    let mut scene = Scene::new();
    scene.clear(Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    });
    let ink = match backdrop {
        Appearance::Light => INK_LIGHT,
        Appearance::Dark => INK_DARK,
    };
    let pill_color = match backdrop {
        Appearance::Light => Color {
            a: 26,
            ..Color::rgb(0)
        },
        Appearance::Dark => Color {
            a: 87,
            ..Color::rgb(0xe1_e1_e1)
        },
    };
    let symbol = SYMBOL as f32;
    let symbol_y = ((h - symbol) / 2.0).round();
    // A pill `width` wide centred on a symbol at `x`, for its button.
    let pill = |x: f32, width: f32| Rect {
        x: (x + symbol / 2.0 - width / 2.0).round(),
        y: ((h - PILL_HEIGHT) / 2.0).round(),
        w: width.round(),
        h: PILL_HEIGHT.round(),
    };
    let style = |weight| Style {
        family: Family::Sans,
        size: TEXT_SIZE,
        weight,
        color: ink,
    };
    let centred = |height: f32| ((h - height) / 2.0).round();

    let logo_x = (MARGIN + 4.0).round();
    let beize_button = pill(logo_x, MENU_PILL);
    if content.open == Some(Open::BeizeMenu) {
        scene.rounded(beize_button, beize_button.h / 2.0, pill_color);
    }
    logo(&mut scene, logo_x, symbol_y, symbol, ink);
    if let Some(app) = content.app {
        // Bold, as macOS names the active app.
        let layout = text.layout(app, &style(700.0), None);
        scene.text(
            &layout,
            logo_x + symbol + SYMBOL_GAP,
            centred(layout.height()),
        );
    }

    let mut right = w - MARGIN;
    if let Some(time) = &content.time {
        let layout = text.layout(time, &style(500.0), None);
        right -= layout.width();
        scene.text(&layout, right.round(), centred(layout.height()));
        right -= BUTTON_SPACE;
    }
    let cc_button = Rect {
        x: (right - CONTROL_CENTER_PILL).round(),
        y: ((h - PILL_HEIGHT) / 2.0).round(),
        w: CONTROL_CENTER_PILL,
        h: PILL_HEIGHT,
    };
    if content.open == Some(Open::ControlCenter) {
        scene.rounded(cc_button, cc_button.h / 2.0, pill_color);
    }
    let cc_x = (cc_button.x + (cc_button.w - symbol) / 2.0).round();
    scene.symbol("toggle-left-fill", cc_x, symbol_y, SYMBOL, ink);
    let globe = GLOBE as f32;
    let network_x = (cc_button.x - BUTTON_SPACE - globe).round();
    let network_symbol = if content.network.connected {
        "globe-simple"
    } else {
        "globe-simple-x"
    };
    scene.symbol(
        network_symbol,
        network_x,
        ((h - globe) / 2.0).round(),
        GLOBE,
        ink,
    );
    let buttons = Buttons {
        beize_menu: Some(beize_button),
        control_center: Some(cc_button),
        width: w,
    };
    (scene, buttons)
}

/// Beize's mark (the owner, 28.10.: two bars with round ends, slanting up,
/// one above the other like a B) in a square of `size` at `(x, y)`: two
/// strokes with round caps, sharp at any size. Drawn on a 16-unit grid.
fn logo(scene: &mut Scene, x: f32, y: f32, size: f32, color: Color) {
    let k = size / 16.0;
    let at = |u: f32, v: f32| [x + u * k, y + v * k];
    let width = 3.8 * k;
    scene.stroke(at(4.0, 6.6), at(11.0, 3.8), width, Cap::Round, color);
    scene.stroke(at(4.0, 12.4), at(12.6, 9.0), width, Cap::Round, color);
}
