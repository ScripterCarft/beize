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
//!
//! The shell is polkit's agent for the user's session: an action that needs
//! the password asks in its dialog (`auth`, ADR 0031).

mod apps;
mod auth;
mod clock;
mod control_center;
mod dock;
mod network;
mod shell;
mod switcher;

use std::collections::HashMap;
use std::process::ExitCode;

use beize_systemd::login::{self, Power};
use beize_ui::dialog::{self, Dialog};
use beize_ui::draw::Cap;
use beize_ui::input::{Button, KeyState, Pointer};
use beize_ui::menu::{Entry, Menu, Outcome};
use beize_ui::{
    Align, App, Appearance, Color, DesktopPlace, Edge, Family, Glass, Input, Material, Overlay,
    Panel, Popup, Rect, Scene, Style, Text, Watch, WindowId, measures,
};
use tracing::{info, warn};

use control_center::ControlCenter;
use dock::{Action, Dock};
use switcher::Switcher;

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
/// glass: white at 12 % below white ink, subtle as macOS' (the owner, 31.10.,
/// on mock-ups); below the control center's button light grey at 34 % –
/// lighter than the bar, as Apple's (its open control-center button: the
/// backdrop about 1.45 times as light, measured), greyer and more opaque than
/// white at 24 % so it shows, the owner's on hardware, 31.10. –; black at 10 %
/// below dark ink, where the bar is light. 36 pt wide under the Beize menu (measured in
/// the HIG's menu-bar image); under the control center's the owner's
/// 54 × 28 px (28.10.; Apple's 46.5 × 24 pt at `SCALE`, 58 × 30, filled
/// the bar). The control center's button is its pill: the clock and the
/// globe keep Apple's space from it, about 6 pt to their ink in Apple's
/// image of it (2 px per pt), at `SCALE`.
const PILL_HEIGHT: f32 = 28.0;
const MENU_PILL: f32 = 36.0 * measures::SCALE;
const CONTROL_CENTER_PILL: f32 = 54.0;
const BUTTON_SPACE: f32 = 8.0;
/// The space either side of an app's menu title, which its open pill fills:
/// 21 to 22 pt between titles on a Mac (measured on the owner's image at 2 px
/// per pt, 31.10.), at `SCALE`.
const MENU_PADDING: f32 = 13.0;

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
    /// The active app's menu by its name, the system's for every app.
    AppName,
    /// A menu the active app describes, by its place.
    AppMenu(usize),
    /// The Window menu, the system's for every app.
    WindowMenu,
}

impl Open {
    /// Whether the bar's left side has it: moving onto another's button
    /// goes there (macOS).
    fn along_bar(self) -> bool {
        matches!(
            self,
            Open::BeizeMenu | Open::AppName | Open::AppMenu(_) | Open::WindowMenu
        )
    }
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
    let mut shell = shell::Shell::new()?;
    loop {
        shell.turn()?;
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
    Dialog::new(title, "Unsaved work may be lost.", action)
}

/// Opens `dialog` in place of any open, for `command` if it confirms one.
fn show_dialog(
    app: &mut App,
    asking: &mut Option<(WindowId, Dialog, Option<Command>)>,
    dialog: Dialog,
    command: Option<Command>,
) -> bool {
    if let Some((id, ..)) = asking.take() {
        app.close(id);
    }
    let overlay = Overlay {
        namespace: "confirmation",
        size: dialog.size(app.text()),
        keyboard: true,
    };
    match app.overlay(&overlay) {
        Ok(id) => {
            app.mark_dialog(id);
            *asking = Some((id, dialog, command));
            true
        }
        Err(e) => {
            warn!("a dialog: {e}");
            false
        }
    }
}

/// The system, as an app's About names it.
fn system_name() -> String {
    let text = std::fs::read_to_string("/usr/lib/os-release").unwrap_or_default();
    let field = text
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|name| name.trim_matches('"').to_owned());
    field.unwrap_or_else(|| "Beize".to_owned())
}

/// The active app's menu by its name, the system's for every app (ADR 0028 §3),
/// as macOS': about it, hiding, quitting. Hiding takes windows off the screen as
/// minimizing does.
fn app_name_menu(
    window: &beize_ui::DesktopWindow,
    windows: &[beize_ui::DesktopWindow],
    names: &mut impl FnMut(&str) -> apps::App,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let name = match names(&window.app_id).name {
        name if !name.is_empty() => name,
        _ if !window.title.is_empty() => window.title.clone(),
        _ => "App".to_owned(),
    };
    // A window whose app named none is an app of its own.
    let same_app = |w: &beize_ui::DesktopWindow| {
        w.id == window.id || (!w.app_id.is_empty() && w.app_id == window.app_id)
    };
    let of = |own: bool| -> Vec<_> {
        windows
            .iter()
            .filter(|w| same_app(w) == own && !w.minimized)
            .map(|w| w.id)
            .collect()
    };
    let (own, others) = (of(true), of(false));
    let hidden: Vec<_> = windows
        .iter()
        .filter(|w| w.minimized)
        .map(|w| w.id)
        .collect();
    let any_hidden = !hidden.is_empty();
    // The window in use stays in front of those that come back.
    let show_all = hidden.into_iter().chain([window.id]).collect();
    let all_own = windows
        .iter()
        .filter(|w| same_app(w))
        .map(|w| w.id)
        .collect();
    let entries = vec![
        Entry::item(format!("About {name}")),
        Entry::Separator,
        Entry::item(format!("Hide {name}")),
        Entry::item("Hide Others").enabled(!others.is_empty()),
        Entry::item("Show All").enabled(any_hidden),
        Entry::Separator,
        Entry::item(format!("Quit {name}")),
    ];
    let commands = vec![
        Some(dock::Command::About(name)),
        None,
        Some(dock::Command::Minimize(own)),
        Some(dock::Command::Minimize(others)),
        Some(dock::Command::Show(show_all)),
        None,
        Some(dock::Command::Quit(all_own)),
    ];
    (entries, commands)
}

/// The Window menu, the system's for every app, as macOS': the active window's,
/// then its app's windows to the front; the group with a symbol last, as Apple's
/// HIG has symbols for a whole group or none.
fn window_menu(
    window: &beize_ui::DesktopWindow,
    dock: &Dock,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let own = if window.app_id.is_empty() {
        vec![window.id]
    } else {
        dock.front_to_top(&window.app_id)
    };
    let entries = vec![
        Entry::item("Minimize"),
        Entry::item("Zoom"),
        Entry::item("Bring All to Front"),
        Entry::Separator,
        Entry::item("Move & Resize").symbol("square-half").submenu(),
    ];
    let commands = vec![
        Some(dock::Command::Minimize(vec![window.id])),
        Some(dock::Command::Zoom(window.id)),
        Some(dock::Command::Show(own)),
        None,
        None,
    ];
    (entries, commands)
}

/// Move & Resize's menu.
fn move_and_resize_menu(
    window: beize_ui::DesktopWindowId,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let entries = vec![
        Entry::item("Left").shortcut("Super+Left"),
        Entry::item("Right").shortcut("Super+Right"),
        Entry::item("Return to Previous Size"),
    ];
    let to = |place| Some(dock::Command::MoveTo(window, place));
    let commands = vec![
        to(DesktopPlace::Left),
        to(DesktopPlace::Right),
        to(DesktopPlace::Previous),
    ];
    (entries, commands)
}

/// A menu an app describes, with each entry's command.
fn app_menu_commands(
    window: beize_ui::DesktopWindowId,
    menu: &beize_ui::menu::AppMenu,
) -> (Vec<Entry>, Vec<Option<dock::Command>>) {
    let entries = menu
        .entries
        .iter()
        .map(|(entry, _)| entry.clone())
        .collect();
    let commands = menu
        .entries
        .iter()
        .map(|(entry, id)| match entry {
            Entry::Item(_) => Some(dock::Command::App(window, *id)),
            Entry::Separator => None,
        })
        .collect();
    (entries, commands)
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

/// Starts the app `app_id` from the dock (`dock`): with an activation
/// token for the click, which its first window shows with and grows out of
/// the app's symbol (ADR 0032 §4); the app starts once the token is made,
/// after the next dispatch. Where the compositor makes none, at once.
fn start(app: &mut App, dock: WindowId, app_id: &str) {
    if !app.ask_activation_token(dock, app_id) {
        beize_systemd::apps::start(app_id, None);
    }
}

/// Does what the Beize menu chose.
fn choose(command: Option<Command>) {
    let Some(command) = command else {
        return;
    };
    info!("Beize menu: {command:?}");
    on_the_side(move || {
        let done = match command {
            Command::Power(power) => login::power(power),
            Command::LogOut => login::log_out(),
        };
        if let Err(e) = done {
            warn!("{command:?}: {e}");
        }
    });
}

/// A call that waits for another service, on a thread of its own with its own
/// bus connection: logind or polkit may take up to sd-bus' 25 s to answer, and
/// the bar and dock must go on meanwhile (`beize_systemd::apps::start` too).
fn on_the_side(job: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().spawn(job) {
        warn!("starting a thread: {e}");
    }
}

/// The user's name as the account has it (its full name, else the login),
/// as macOS names the user in "Log Out".
fn user_name() -> String {
    // SAFETY: getpwuid's entry is read at once, before any other call to it
    // (only this thread calls it; `on_the_side` does not).
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
    /// The titles of the menus the active app describes.
    menus: &'a [String],
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
    /// The menus along the bar's left side, in order, and how each opens.
    along: Vec<(Open, Rect, Align)>,
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

    /// The menus along the bar's left side, in order.
    fn along_bar(&self) -> Vec<(Open, Rect, Align)> {
        self.along.clone()
    }

    /// The button at `(x, y)` on the bar, what its menu opens below and
    /// how it lines up: the Beize menu from its button's left edge, the
    /// control center a gap from the screen's right edge.
    fn at(&self, (x, y): (f64, f64)) -> Option<(Open, Rect, Align)> {
        let (x, y) = (x as f32, y as f32);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= 0.0 && y < HEIGHT as f32;
        if let Some(found) = self.along.iter().find(|(_, r, _)| inside(r)) {
            return Some(*found);
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
    let menu_pill = match backdrop {
        Appearance::Light => pill_color,
        Appearance::Dark => Color {
            a: 31,
            ..Color::rgb(0xff_ff_ff)
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
        scene.rounded(beize_button, beize_button.h / 2.0, menu_pill);
    }
    logo(&mut scene, logo_x, symbol_y, symbol, ink);
    let bar_anchor = |r: Rect| Rect {
        x: r.x,
        y: 0.0,
        w: r.w,
        h,
    };
    let mut along = vec![(Open::BeizeMenu, bar_anchor(beize_button), Align::Left)];
    if let Some(app) = content.app {
        // Bold, as macOS names the active app; then its menus, each its text
        // on a pill wider by `MENU_PADDING` a side.
        let mut x = logo_x + symbol + SYMBOL_GAP;
        let mut item =
            |title: &str, weight: f32, which: Open, text: &mut Text, scene: &mut Scene| {
                let layout = text.layout(title, &style(weight), None);
                let button = Rect {
                    x: (x - MENU_PADDING).round(),
                    y: ((h - PILL_HEIGHT) / 2.0).round(),
                    w: (layout.width() + 2.0 * MENU_PADDING).round(),
                    h: PILL_HEIGHT,
                };
                if content.open == Some(which) {
                    scene.rounded(button, button.h / 2.0, menu_pill);
                }
                scene.text(&layout, x.round(), centred(layout.height()));
                along.push((which, bar_anchor(button), Align::Left));
                x += layout.width() + 2.0 * MENU_PADDING;
            };
        item(app, 700.0, Open::AppName, text, &mut scene);
        for (i, title) in content.menus.iter().enumerate() {
            item(title, 500.0, Open::AppMenu(i), text, &mut scene);
        }
        item("Window", 500.0, Open::WindowMenu, text, &mut scene);
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
        along,
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
