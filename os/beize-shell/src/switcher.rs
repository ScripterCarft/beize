//! Alt+Tab's window (ADR 0028 §7): the compositor keeps the keys and says
//! which windows and which is selected (`App::desktop_switcher`); the shell
//! lays them out as tiles on menu glass, each a place for the window's live
//! picture, which the compositor draws there (`App::show_thumbnails`), its
//! app's icon and its title. The tiles shrink until all fit. A click on a
//! tile chooses it; a pointer moved onto one selects it, a pointer that only
//! lies there does not, as GNOME's.
//!
//! The measures are estimated from Windows 11's and KWin's switchers.

use beize_ui::{
    AppIcon, Appearance, Color, DesktopWindow, DesktopWindowId, Family, Glass, Material, Rect,
    Scene, Style, Text,
};

use crate::apps::App;

const PADDING: f32 = 20.0;
const GAP: f32 = 10.0;
/// A tile's widths to try, largest first; its picture is 3:4 of that.
const WIDTHS: [f32; 4] = [240.0, 190.0, 150.0, 110.0];
const INSET: f32 = 8.0;
const LABEL: f32 = 32.0;
const ICON: u32 = 20;
const RADIUS: f32 = 20.0;
const TILE_RADIUS: f32 = 12.0;
const TEXT_SIZE: f32 = 13.0;

const INK_LIGHT: Color = Color::rgb(0x292929);
const INK_DARK: Color = Color::rgb(0xe8e8e8);
/// The selected tile, in the accent's blue, so a picture of any colour shows
/// against it.
const SELECTED: Color = Color {
    r: 0x00,
    g: 0x7a,
    b: 0xff,
    a: 110,
};

/// The grid for `count` tiles in at most `(w, h)` px: its tile's width and
/// its columns and rows.
fn grid(count: usize, (w, h): (u32, u32)) -> (f32, usize, usize) {
    let (w, h) = (w as f32, h as f32);
    let n = count.max(1);
    let fit = |tile: f32| {
        let columns = (((w - 2.0 * PADDING + GAP) / (tile + GAP)).floor() as usize).clamp(1, n);
        let rows = n.div_ceil(columns);
        let needed =
            2.0 * PADDING + rows as f32 * tile_height(tile) + rows.saturating_sub(1) as f32 * GAP;
        (needed <= h).then_some((tile, columns, rows))
    };
    WIDTHS
        .iter()
        .find_map(|&tile| fit(tile))
        .unwrap_or_else(|| {
            let tile = WIDTHS[WIDTHS.len() - 1];
            let columns = (((w - 2.0 * PADDING + GAP) / (tile + GAP)).floor() as usize).clamp(1, n);
            (tile, columns, n.div_ceil(columns))
        })
}

fn tile_height(tile: f32) -> f32 {
    ((tile - 2.0 * INSET) * 0.75).round() + 2.0 * INSET + LABEL
}

/// The switcher's windows as the compositor last said.
pub struct Switcher {
    entries: Vec<DesktopWindowId>,
    selected: Option<DesktopWindowId>,
    /// The room it may take, from the compositor.
    room: (u32, u32),
    pointer: Option<(f64, f64)>,
}

impl Switcher {
    pub fn new(room: (u32, u32)) -> Switcher {
        Switcher {
            entries: Vec::new(),
            selected: None,
            room,
            pointer: None,
        }
    }

    /// What the compositor says now; whether it looks different.
    pub fn update(
        &mut self,
        entries: Vec<DesktopWindowId>,
        selected: Option<DesktopWindowId>,
    ) -> bool {
        let changed = entries != self.entries || selected != self.selected;
        self.entries = entries;
        self.selected = selected;
        changed
    }

    /// Its size for the windows now.
    pub fn size(&self) -> (u32, u32) {
        let (tile, columns, rows) = grid(self.entries.len(), self.room);
        let w = 2.0 * PADDING + columns as f32 * tile + (columns - 1) as f32 * GAP;
        let h = 2.0 * PADDING + rows as f32 * tile_height(tile) + (rows - 1) as f32 * GAP;
        (w as u32, h as u32)
    }

    /// Where the `i`th tile lies.
    fn tile(&self, i: usize) -> Rect {
        let (tile, columns, _) = grid(self.entries.len(), self.room);
        let (column, row) = (i % columns, i / columns);
        Rect {
            x: PADDING + column as f32 * (tile + GAP),
            y: PADDING + row as f32 * (tile_height(tile) + GAP),
            w: tile,
            h: tile_height(tile),
        }
    }

    /// The place for a tile's picture.
    fn picture(tile: Rect) -> Rect {
        Rect {
            x: tile.x + INSET,
            y: tile.y + INSET,
            w: tile.w - 2.0 * INSET,
            h: tile.h - 2.0 * INSET - LABEL,
        }
    }

    /// Each window with the place of its picture.
    pub fn pictures(&self) -> Vec<(DesktopWindowId, Rect)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, Self::picture(self.tile(i))))
            .collect()
    }

    /// The pointer moved or left; the window of the tile under it.
    pub fn pointer(&mut self, at: Option<(f64, f64)>) -> Option<DesktopWindowId> {
        self.pointer = at;
        self.under()
    }

    /// The window of the tile under the pointer.
    pub fn under(&self) -> Option<DesktopWindowId> {
        let (x, y) = self.pointer?;
        let (x, y) = (x as f32, y as f32);
        self.entries.iter().enumerate().find_map(|(i, id)| {
            let t = self.tile(i);
            (x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h).then_some(*id)
        })
    }

    /// Its glass in `(w, h)`, as below a menu.
    pub fn glass((w, h): (u32, u32)) -> Glass {
        let all = Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        };
        Glass {
            shadow: true,
            ..Glass::one(all, RADIUS, true, Material::Menu)
        }
    }

    /// Its content: the selected tile's highlight, each tile's icon and
    /// title; the rest shows the glass, the pictures the compositor draws.
    pub fn draw(
        &self,
        text: &mut Text,
        (w, h): (u32, u32),
        appearance: Appearance,
        window: impl Fn(DesktopWindowId) -> Option<DesktopWindow>,
        names: &mut impl FnMut(&str) -> App,
    ) -> Scene {
        let mut scene = Scene::new();
        scene.clear(Rect {
            x: 0.0,
            y: 0.0,
            w: w as f32,
            h: h as f32,
        });
        let ink = match appearance {
            Appearance::Light => INK_LIGHT,
            Appearance::Dark => INK_DARK,
        };
        let style = Style {
            family: Family::Sans,
            size: TEXT_SIZE,
            weight: 500.0,
            color: ink,
        };
        for (i, id) in self.entries.iter().enumerate() {
            let tile = self.tile(i);
            if self.selected == Some(*id) {
                scene.rounded(tile, TILE_RADIUS, SELECTED);
            }
            let Some(window) = window(*id) else {
                continue;
            };
            let app = names(&window.app_id);
            let picture = Self::picture(tile);
            let label_y = picture.y + picture.h;
            let icon_x = tile.x + INSET;
            let icon_y = (label_y + (LABEL - ICON as f32) / 2.0).round();
            let icon = Rect {
                x: icon_x,
                y: icon_y,
                w: ICON as f32,
                h: ICON as f32,
            };
            if let Some(symbol) = AppIcon::of(&window.app_id) {
                symbol.draw(&mut scene, icon);
            } else if let Some(file) = &app.icon {
                scene.image(file, icon_x, icon_y, ICON);
            }
            let title = if window.title.is_empty() {
                app.name.as_str()
            } else {
                window.title.as_str()
            };
            let room = tile.w - 2.0 * INSET - ICON as f32 - 6.0;
            let shown = fitted(text, title, &style, room);
            let layout = text.layout(&shown, &style, None);
            scene.text(
                &layout,
                (icon_x + ICON as f32 + 6.0).round(),
                (label_y + (LABEL - layout.height()) / 2.0).round(),
            );
        }
        scene
    }
}

/// `title`, cut with an ellipsis to fit `room` px.
fn fitted(text: &mut Text, title: &str, style: &Style, room: f32) -> String {
    if text.layout(title, style, None).width() <= room {
        return title.to_owned();
    }
    let mut chars: Vec<char> = title.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cut: String = chars.iter().collect::<String>() + "…";
        if text.layout(&cut, style, None).width() <= room {
            return cut;
        }
    }
    "…".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_shrink_until_all_fit() {
        let (tile, columns, rows) = grid(2, (1280, 700));
        assert_eq!((tile, columns, rows), (240.0, 2, 1));
        let (tile, columns, rows) = grid(40, (1280, 700));
        assert!(tile < 240.0);
        assert!(columns * rows >= 40);
        let (tile, _, _) = grid(1000, (1280, 700));
        assert_eq!(tile, WIDTHS[WIDTHS.len() - 1]);
    }

    #[test]
    fn the_tile_under_the_pointer_is_its_windows() {
        let mut switcher = Switcher::new((1280, 700));
        let (a, b) = (DesktopWindowId(0), DesktopWindowId(1));
        assert!(switcher.update(vec![a, b], Some(b)));
        assert!(!switcher.update(vec![a, b], Some(b)));
        let t = switcher.tile(1);
        assert_eq!(
            switcher.pointer(Some((f64::from(t.x + 1.0), f64::from(t.y + 1.0)))),
            Some(b)
        );
        assert_eq!(switcher.pointer(Some((1.0, 1.0))), None);
        assert_eq!(switcher.pointer(None), None);
    }
}
