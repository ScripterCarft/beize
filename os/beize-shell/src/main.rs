//! `beize-shell` – Beize's shell (ADR 0028): the menu bar along the top
//! of the screen; the dock and the control center follow. A client of the
//! compositor with Beize UI, the only one it lets draw panels
//! (wlr-layer-shell, ADR 0028 §1). Runs as `beize-shell.service` in the
//! user's desktop, restarted if it fails; it ends with the session.
//!
//! The bar for now: light (VISION: the shell is light), 24 px like macOS'
//! menu bar, the time on the right. Glass, the Beize menu, the active app
//! and the status come with the next parts (ADR 0028, Reihenfolge).

mod clock;

use std::process::ExitCode;

use beize_ui::{Color, Edge, Family, Rect, Scene, Style, Text, Window};
use tracing::info;

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
    let mut bar = Window::panel("menu-bar", Edge::Top, HEIGHT).map_err(|e| e.to_string())?;
    info!("menu bar on {}", bar.gpu_name());
    let minutes = clock::Minutes::new().map_err(|e| format!("the minute's timer: {e}"))?;
    loop {
        let events = bar
            .dispatch(None, &[minutes.fd()])
            .map_err(|e| e.to_string())?;
        if events.closed {
            // Its screen went: a failure, so the service manager starts the
            // shell again.
            return Err("the compositor closed the menu bar".into());
        }
        let tick = !events.ready.is_empty();
        if tick {
            minutes
                .take()
                .map_err(|e| format!("the minute's timer: {e}"))?;
        }
        if events.configured || tick {
            let size = bar.size();
            let scene = menu_bar(bar.text(), size, clock::now());
            bar.draw(&scene).map_err(|e| e.to_string())?;
        }
    }
}

/// The bar in `size`: the time on the right, centred in the height.
fn menu_bar(text: &mut Text, (w, h): (u32, u32), time: Option<clock::Time>) -> Scene {
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
    if let Some(time) = time {
        let style = Style {
            family: Family::Sans,
            size: 13.0,
            weight: 500.0,
            color: INK,
        };
        let layout = text.layout(&time.to_string(), &style, None);
        let x = (w - MARGIN - layout.width()).round();
        let y = ((h - layout.height()) / 2.0).round();
        scene.text(&layout, x, y);
    }
    scene
}
