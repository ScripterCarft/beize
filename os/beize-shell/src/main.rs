//! `beize-shell` – Beize's shell (ADR 0028): the menu bar along the top
//! of the screen and the dock along the bottom; the control center
//! follows. A client of the compositor with Beize UI, the only one it lets
//! draw panels (wlr-layer-shell) and see the windows (beize-shell-v1, ADR
//! 0028 §1): both panels are windows of one app. Runs as
//! `beize-shell.service` in the user's desktop, restarted if it fails; it
//! ends with the session.
//!
//! The bar for now: macOS' menu bar in Beize's measures (`measures`), the
//! active app's name on the left in bold, the date and time on the right. As macOS' since 26 it has no
//! background: white ink on the scrim the compositor lays behind it, dark
//! only where the background stays very light, as the compositor measures
//! it (ADR 0028 §3). With the background switched on (`BarLook::Glass`,
//! macOS' "Show menu bar background") the bar lies on base glass instead,
//! its ink for the appearance. The dock lies on live glass, which the
//! compositor draws below it (§6). The Beize menu and the status come with
//! the next parts (ADR 0028, Reihenfolge).

mod apps;
mod clock;
mod dock;

use std::collections::HashMap;
use std::process::ExitCode;

use beize_ui::input::{Button, Pointer};
use beize_ui::{
    App, Appearance, Color, Edge, Family, Glass, Input, Panel, Rect, Scene, Style, Text, Watch,
    measures,
};
use tracing::info;

use dock::{Action, Dock};

const HEIGHT: u32 = measures::MENU_BAR;
/// The bar's text on a light background and on a dark one.
const INK_LIGHT: Color = Color::rgb(0x1d1d1d);
const INK_DARK: Color = Color::rgb(0xffffff);
/// Its size, macOS' 13 a little larger (the owner chose between 15 and
/// 16.25, macOS' at `SCALE`), and the space at the bar's ends, macOS' 12.
const TEXT_SIZE: f32 = 15.0;
const MARGIN: f32 = 12.0 * measures::SCALE;

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
    loop {
        let mut dispatched = app
            .dispatch(None, &[Watch::Read(minutes.fd())])
            .map_err(|e| e.to_string())?;
        let bar_events = dispatched.take(bar);
        let dock_events = dispatched.take(dock_panel);
        if bar_events.closed || dock_events.closed {
            // Its screen went: a failure, so the service manager starts the
            // shell again.
            return Err("the compositor closed a panel".into());
        }
        let tick = !dispatched.ready.is_empty();
        if tick {
            minutes
                .take()
                .map_err(|e| format!("the minute's timer: {e}"))?;
        }
        let mut redraw_bar = bar_events.configured || tick;
        let mut redraw_dock = false;
        if app.appearance() != appearance {
            appearance = app.appearance();
            info!("appearance {appearance:?}");
            redraw_bar = true;
            redraw_dock = true;
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
        if bar_shown && redraw_bar {
            let size = app.size(bar);
            // On glass the ink follows the appearance, as the dock's.
            let behind = match look {
                BarLook::Transparent => backdrop,
                BarLook::Glass => {
                    app.set_glass(bar, Some(bar_glass(size)));
                    appearance
                }
            };
            let scene = menu_bar(
                app.text(),
                size,
                active_name.as_deref(),
                clock::now(),
                behind,
            );
            app.draw(bar, &scene).map_err(|e| e.to_string())?;
            bar_drawn = true;
        }
        dock_shown |= dock_events.configured;
        if dock_shown && (redraw_dock || dock_events.configured) {
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
        }
    }
}

/// The bar's glass (`BarLook::Glass`): all of it, square, without a rim –
/// it runs along the screen's edge –, base glass: no window lies behind it.
fn bar_glass((w, h): (u32, u32)) -> Glass {
    Glass {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        },
        radius: 0.0,
        rim: false,
        live: false,
    }
}

/// The bar in `size`, on a `backdrop` light or dark: the active app's name
/// on the left, the time on the right, both centred in the height; the
/// rest transparent.
fn menu_bar(
    text: &mut Text,
    (w, h): (u32, u32),
    app: Option<&str>,
    time: Option<String>,
    backdrop: Appearance,
) -> Scene {
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
    let style = |weight, color| Style {
        family: Family::Sans,
        size: TEXT_SIZE,
        weight,
        color,
    };
    let mut put = |scene: &mut Scene, s: &str, weight: f32, x: Option<f32>| {
        let layout = text.layout(s, &style(weight, ink), None);
        let x = x.unwrap_or(w - MARGIN - layout.width()).round();
        let y = ((h - layout.height()) / 2.0).round();
        scene.text(&layout, x, y);
    };
    if let Some(app) = app {
        // Bold, as macOS names the active app.
        put(&mut scene, app, 700.0, Some(MARGIN));
    }
    if let Some(time) = time {
        put(&mut scene, &time, 500.0, None);
    }
    scene
}
