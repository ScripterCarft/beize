//! `beize-shell` – Beize's shell (ADR 0028): the menu bar along the top
//! of the screen and the dock along the bottom; the control center
//! follows. A client of the compositor with Beize UI, the only one it lets
//! draw panels (wlr-layer-shell) and see the windows (beize-shell-v1, ADR
//! 0028 §1): both panels are windows of one app. Runs as
//! `beize-shell.service` in the user's desktop, restarted if it fails; it
//! ends with the session.
//!
//! The bar for now: light (VISION: the shell is light), 24 px like macOS'
//! menu bar, the active app's name on the left in bold, the time on the
//! right. Glass, the Beize menu and the status come with the next parts
//! (ADR 0028, Reihenfolge).

mod apps;
mod clock;
mod dock;

use std::collections::HashMap;
use std::process::ExitCode;

use beize_ui::input::{Button, Pointer};
use beize_ui::{App, Color, Edge, Family, Input, Panel, Rect, Scene, Style, Text};
use tracing::info;

use dock::{Action, Dock};

/// The menu bar's height at scale 1, as macOS'.
const HEIGHT: u32 = 24;
const BAR: Color = Color::rgb(0xf5f5f5);
const INK: Color = Color::rgb(0x1d1d1d);
/// Space at the bar's ends.
const MARGIN: f32 = 12.0;

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
            translucent: false,
        })
        .map_err(|e| e.to_string())?;
    // Names from the apps' desktop files, read once each.
    let mut known: HashMap<String, String> = HashMap::new();
    let mut names = |app_id: &str| {
        known
            .entry(app_id.to_owned())
            .or_insert_with(|| apps::name(app_id))
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
    let mut dock_width = dock.width();
    let mut active_name: Option<String> = None;
    loop {
        let mut dispatched = app
            .dispatch(None, &[minutes.fd()])
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
        if dispatched.desktop {
            let windows = app.desktop_windows();
            let before = dock.summary();
            dock.update(&windows, &mut names);
            if dock.summary() != before {
                info!("dock: {}", dock.summary());
                redraw_dock = true;
            }
            let name = windows.iter().find(|w| w.active).map(|w| names(&w.app_id));
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
            let scene = menu_bar(app.text(), size, active_name.as_deref(), clock::now());
            app.draw(bar, &scene).map_err(|e| e.to_string())?;
        }
        dock_shown |= dock_events.configured;
        if dock_shown && (redraw_dock || dock_events.configured) {
            let size = app.size(dock_panel);
            let scene = dock.draw(app.text(), size);
            app.draw(dock_panel, &scene).map_err(|e| e.to_string())?;
        }
    }
}

/// The bar in `size`: the active app's name on the left, the time on the
/// right, both centred in the height.
fn menu_bar(
    text: &mut Text,
    (w, h): (u32, u32),
    app: Option<&str>,
    time: Option<clock::Time>,
) -> Scene {
    let (w, h) = (w as f32, h as f32);
    let mut scene = Scene::new();
    scene.fill(
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        },
        BAR,
    );
    let style = |weight| Style {
        family: Family::Sans,
        size: 13.0,
        weight,
        color: INK,
    };
    if let Some(app) = app {
        // Bold, as macOS names the active app.
        let layout = text.layout(app, &style(700.0), None);
        let y = ((h - layout.height()) / 2.0).round();
        scene.text(&layout, MARGIN, y);
    }
    if let Some(time) = time {
        let layout = text.layout(&time.to_string(), &style(500.0), None);
        let x = (w - MARGIN - layout.width()).round();
        let y = ((h - layout.height()) / 2.0).round();
        scene.text(&layout, x, y);
    }
    scene
}
