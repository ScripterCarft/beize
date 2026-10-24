//! The dock (ADR 0028 §4): floating, centred along the bottom edge, with
//! round corners – the pinned apps, then the running ones in the order
//! they opened, a dot below each that runs. macOS' defaults: no
//! magnification, not hidden.
//!
//! A click starts an app that does not run (`beize_systemd::apps`), and
//! otherwise brings its windows to the front, the one used last on top; if
//! all of them are minimized, the one minimized last comes back – as macOS'
//! Dock does. Like a button, an app acts when the press is released over
//! it.
//!
//! Its measures are macOS 26's, measured in Apple's press image of it (a
//! 14-inch MacBook Pro, 1512 pt across; CLAUDE.md §11), its corners the
//! continuous ones of `beize_vulkan`'s shader. Beize's apps show their
//! symbols (`AppIcon`), others their icons (hicolor, `apps`), an app
//! without one a tile with its initial. Light and opaque until the glass
//! (ADR 0028, Reihenfolge d).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::apps::App;
use beize_ui::{
    AppIcon, Color, DesktopWindow, DesktopWindowId, Family, Rect, Scene, Style, Text, icon,
};

/// Pinned when the desktop starts (ADR 0028 §4); more apps come as Beize
/// has them.
const PINNED: [&str; 1] = ["beize-terminal"];

/// An app's tile, square, and the space between tiles: measured 52 pt
/// from tile to tile, the icon 38.6 pt – 48 on Apple's icon grid.
pub const TILE: u32 = 48;
pub const GAP: u32 = 4;
/// Around the tiles: measured 9.7 pt to the middle of the dock's rim.
pub const PADDING: u32 = 10;
/// The dock's height: tile and padding above and below, the dot in the
/// lower padding.
pub const HEIGHT: u32 = TILE + 2 * PADDING;
/// Between the dock and the bottom of the screen, it floats: measured
/// 5.1 pt.
pub const MARGIN: u32 = 5;

/// Its corners: measured 20.4 pt on a dock 76.3 pt high in a screenshot
/// of macOS 27 (macOS 26 rounded it more), kept in proportion as the dock
/// grows with its tiles.
const RADIUS: f32 = HEIGHT as f32 * 20.4 / 76.3;
/// The icon on its tile, as Apple's icon grid: 824 of 1024, with the
/// corners of Beize's app symbols.
const ICON: f32 = TILE as f32 * 824.0 / 1024.0;
const ICON_RADIUS: f32 = ICON * icon::RADIUS;
/// Other apps' icons fill that square, on whole pixels.
pub const ICON_PIXELS: u32 = ICON.round() as u32;
/// The dot below a running app: measured 4.0 pt, its middle 3.7 pt above
/// the dock's bottom.
const DOT: f32 = 4.0;
const DOT_ABOVE: f32 = 3.7;

const BACKGROUND: Color = Color::rgb(0xf5f5f5);
/// A hairline around it, so it stands out on a light window below.
const RIM: Color = Color {
    a: 0x1f,
    ..Color::rgb(0x000000)
};
const TILE_COLOR: Color = Color::rgb(0x3a3a3c);
const INITIAL: Color = Color::rgb(0xffffff);
const RUNNING: Color = Color {
    a: 0x99,
    ..Color::rgb(0x1d1d1d)
};
/// Over a tile while it is pressed: darker, as macOS shows it.
const PRESSED: Color = Color {
    a: 0x40,
    ..Color::rgb(0x000000)
};

/// What a click on the dock asks for.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Start the app.
    Start(String),
    /// Bring these windows to the front, in this order (the last ends on
    /// top).
    Activate(Vec<DesktopWindowId>),
}

struct Item {
    app_id: String,
    name: String,
    icon: Option<PathBuf>,
    /// Its windows, in the order they opened.
    windows: Vec<DesktopWindow>,
}

#[derive(Default)]
pub struct Dock {
    items: Vec<Item>,
    /// When each window was last active, and last minimized (a running
    /// count, newer is larger).
    activated: HashMap<DesktopWindowId, u64>,
    minimized: HashMap<DesktopWindowId, u64>,
    count: u64,
    /// The window active at the last update.
    active: Option<DesktopWindowId>,
    pointer: Option<(f64, f64)>,
    pressed: Option<usize>,
}

impl Dock {
    pub fn new(names: &mut impl FnMut(&str) -> App) -> Dock {
        let mut dock = Dock::default();
        dock.update(&[], names);
        dock
    }

    /// The desktop's windows now; `names` gives an app's name and icon.
    pub fn update(&mut self, windows: &[DesktopWindow], names: &mut impl FnMut(&str) -> App) {
        for window in windows {
            if window.active && self.active != Some(window.id) {
                self.count += 1;
                self.activated.insert(window.id, self.count);
            }
            if !window.minimized {
                self.minimized.remove(&window.id);
            } else if !self.minimized.contains_key(&window.id) {
                self.count += 1;
                self.minimized.insert(window.id, self.count);
            }
        }
        self.active = windows.iter().find(|w| w.active).map(|w| w.id);
        let open = |id: &DesktopWindowId| windows.iter().any(|w| w.id == *id);
        self.activated.retain(|id, _| open(id));
        self.minimized.retain(|id, _| open(id));
        let mut items: Vec<Item> = PINNED
            .iter()
            .map(|&app_id| {
                let App { name, icon } = names(app_id);
                Item {
                    app_id: app_id.to_owned(),
                    name,
                    icon,
                    windows: Vec::new(),
                }
            })
            .collect();
        for window in windows {
            match items.iter_mut().find(|i| i.app_id == window.app_id) {
                Some(item) => item.windows.push(window.clone()),
                None => {
                    let App { name, icon } = names(&window.app_id);
                    items.push(Item {
                        app_id: window.app_id.clone(),
                        name,
                        icon,
                        windows: vec![window.clone()],
                    });
                }
            }
        }
        if items.len() != self.items.len() {
            self.pressed = None;
        }
        self.items = items;
    }

    /// The apps in it, for the log: `id`, `id running`.
    pub fn summary(&self) -> String {
        let items: Vec<String> = self
            .items
            .iter()
            .map(|i| {
                if i.windows.is_empty() {
                    i.app_id.clone()
                } else {
                    format!("{} running", i.app_id)
                }
            })
            .collect();
        items.join(", ")
    }

    /// Its width for the apps in it.
    pub fn width(&self) -> u32 {
        let n = self.items.len().max(1) as u32;
        2 * PADDING + n * TILE + (n - 1) * GAP
    }

    /// Where the tile of the `i`th app lies.
    fn tile(i: usize) -> Rect {
        Rect {
            x: (PADDING + i as u32 * (TILE + GAP)) as f32,
            y: PADDING as f32,
            w: TILE as f32,
            h: TILE as f32,
        }
    }

    fn item_at(&self, (x, y): (f64, f64)) -> Option<usize> {
        (0..self.items.len()).find(|&i| {
            let t = Dock::tile(i);
            let (x, y) = (x as f32, y as f32);
            x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h
        })
    }

    /// The pointer moved on the dock, or left it (None).
    pub fn hover(&mut self, at: Option<(f64, f64)>) {
        self.pointer = at;
    }

    /// The left button went down; whether the dock looks different now.
    pub fn press(&mut self) -> bool {
        self.pressed = self.pointer.and_then(|at| self.item_at(at));
        self.pressed.is_some()
    }

    /// The left button came up: what the app it went down on asks for,
    /// if the pointer is still over it.
    pub fn release(&mut self) -> Option<Action> {
        let pressed = self.pressed.take()?;
        if self.pointer.and_then(|at| self.item_at(at)) != Some(pressed) {
            return None;
        }
        let item = &self.items[pressed];
        if item.windows.is_empty() {
            return Some(Action::Start(item.app_id.clone()));
        }
        let mut shown: Vec<&DesktopWindow> = item.windows.iter().filter(|w| !w.minimized).collect();
        if shown.is_empty() {
            let last = item
                .windows
                .iter()
                .max_by_key(|w| (self.minimized.get(&w.id).copied().unwrap_or(0), w.id))?;
            return Some(Action::Activate(vec![last.id]));
        }
        shown.sort_by_key(|w| (self.activated.get(&w.id).copied().unwrap_or(0), w.id));
        Some(Action::Activate(shown.iter().map(|w| w.id).collect()))
    }

    /// The dock in `(w, h)`: transparent outside its round corners.
    pub fn draw(&self, text: &mut Text, (w, h): (u32, u32)) -> Scene {
        let (w, h) = (w as f32, h as f32);
        let all = Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        };
        let mut scene = Scene::new();
        scene.clear(all);
        scene.rounded(all, RADIUS, RIM);
        let inner = Rect {
            x: 1.0,
            y: 1.0,
            w: w - 2.0,
            h: h - 2.0,
        };
        scene.rounded(inner, RADIUS - 1.0, BACKGROUND);
        let style = Style {
            family: Family::Sans,
            size: 22.0,
            weight: 600.0,
            color: INITIAL,
        };
        for (i, item) in self.items.iter().enumerate() {
            let tile = Dock::tile(i);
            let icon = Rect {
                x: tile.x + (tile.w - ICON) / 2.0,
                y: tile.y + (tile.h - ICON) / 2.0,
                w: ICON,
                h: ICON,
            };
            if let Some(symbol) = AppIcon::of(&item.app_id) {
                symbol.draw(&mut scene, icon);
            } else if let Some(file) = &item.icon {
                let at = |start: f32, side: f32| start + (side - ICON_PIXELS as f32) / 2.0;
                scene.image(file, at(tile.x, tile.w), at(tile.y, tile.h), ICON_PIXELS);
            } else {
                scene.rounded(icon, ICON_RADIUS, TILE_COLOR);
                let initial: String = item.name.chars().take(1).collect();
                let layout = text.layout(&initial.to_uppercase(), &style, None);
                scene.text(
                    &layout,
                    (tile.x + (tile.w - layout.width()) / 2.0).round(),
                    (tile.y + (tile.h - layout.height()) / 2.0).round(),
                );
            }
            if self.pressed == Some(i) && self.pointer.and_then(|at| self.item_at(at)) == Some(i) {
                scene.rounded(icon, ICON_RADIUS, PRESSED);
            }
            if !item.windows.is_empty() {
                let dot = Rect {
                    x: tile.x + (tile.w - DOT) / 2.0,
                    y: h - DOT_ABOVE - DOT / 2.0,
                    w: DOT,
                    h: DOT,
                };
                scene.rounded(dot, DOT / 2.0, RUNNING);
            }
        }
        scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, app: &str, active: bool, minimized: bool) -> DesktopWindow {
        DesktopWindow {
            id: DesktopWindowId(id),
            app_id: app.into(),
            title: String::new(),
            active,
            minimized,
        }
    }

    fn names(id: &str) -> App {
        App {
            name: id.to_owned(),
            icon: None,
        }
    }

    /// Pressed and released over the `i`th app.
    fn click(dock: &mut Dock, i: usize) -> Option<Action> {
        let t = Dock::tile(i);
        dock.hover(Some((f64::from(t.x) + 1.0, f64::from(t.y) + 1.0)));
        assert!(dock.press());
        dock.release()
    }

    #[test]
    fn a_pinned_app_that_does_not_run_starts() {
        let mut dock = Dock::new(&mut names);
        assert_eq!(dock.summary(), "beize-terminal");
        assert_eq!(dock.width(), 2 * PADDING + TILE);
        assert_eq!(
            click(&mut dock, 0),
            Some(Action::Start("beize-terminal".into()))
        );
        // Released beside the app: nothing.
        let t = Dock::tile(0);
        dock.hover(Some((f64::from(t.x) + 1.0, f64::from(t.y) + 1.0)));
        assert!(dock.press());
        dock.hover(Some((0.0, 0.0)));
        assert_eq!(dock.release(), None);
    }

    #[test]
    fn running_apps_follow_the_pinned_ones() {
        let mut dock = Dock::new(&mut names);
        dock.update(
            &[
                window(0, "beize-ui-test", false, false),
                window(1, "beize-terminal", true, false),
                window(2, "other", false, false),
            ],
            &mut names,
        );
        assert_eq!(
            dock.summary(),
            "beize-terminal running, beize-ui-test running, other running"
        );
        assert_eq!(dock.width(), 2 * PADDING + 3 * TILE + 2 * GAP);
    }

    #[test]
    fn a_click_brings_the_windows_the_one_used_last_on_top() {
        let mut dock = Dock::new(&mut names);
        let t = "beize-terminal";
        // Window 1 active, then 0.
        dock.update(
            &[window(0, t, false, false), window(1, t, true, false)],
            &mut names,
        );
        dock.update(
            &[window(0, t, true, false), window(1, t, false, false)],
            &mut names,
        );
        assert_eq!(
            click(&mut dock, 0),
            Some(Action::Activate(vec![
                DesktopWindowId(1),
                DesktopWindowId(0)
            ]))
        );
        // One minimized: only the other comes to the front.
        dock.update(
            &[window(0, t, false, true), window(1, t, true, false)],
            &mut names,
        );
        assert_eq!(
            click(&mut dock, 0),
            Some(Action::Activate(vec![DesktopWindowId(1)]))
        );
    }

    #[test]
    fn with_all_minimized_the_one_minimized_last_comes_back() {
        let mut dock = Dock::new(&mut names);
        let t = "beize-terminal";
        dock.update(
            &[window(0, t, false, false), window(1, t, false, true)],
            &mut names,
        );
        dock.update(
            &[window(0, t, false, true), window(1, t, false, true)],
            &mut names,
        );
        assert_eq!(
            click(&mut dock, 0),
            Some(Action::Activate(vec![DesktopWindowId(0)]))
        );
    }
}
