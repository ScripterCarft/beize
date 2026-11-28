//! Alt+Tab's window (ADR 0028 §7): the compositor keeps the keys and says
//! which windows and which is selected (`App::desktop_switcher`); the shell
//! lays them out as tiles on menu glass, each a place for the window's live
//! picture, which the compositor draws there (`App::show_thumbnails`), its
//! app's icon and its title. The tiles shrink until all fit. A click on a
//! tile chooses it; a pointer moved onto one selects it, a pointer that only
//! lies there does not, as GNOME's.
//!
//! The measures are estimated from Windows 11's and KWin's switchers; the
//! selected tile is the menus' selected row: the accent, white on it (the
//! owner's, on mock-ups, 31.10.).
//!
//! The selection's ring – that accent – springs from tile to tile as the
//! selection moves (ADR 0032 §5: macOS' ⌘⇥, SwiftUI's `interactiveSpring`),
//! drawn anew each time the switcher's surface is ready (its frame
//! callbacks); it comes at once with the switcher and with tiles that move.
//! The compositor keeps the selection: Alt let go chooses it, wherever the
//! ring is drawn (ADR 0032 §2). `BEIZE_ANIMATIONS_SLOWER` makes it slower,
//! as the compositor's springs, for the smoke test.

use std::time::Instant;

use beize_ui::menu::fitted;
use beize_ui::theme::{self, ACCENT, ON_ACCENT, SMALL_TEXT_SIZE};
use beize_ui::{
    AppIcon, Appearance, Density, DesktopWindow, DesktopWindowId, Family, Glass, Motion, Rect,
    Scene, Spring, Style, Text,
};
use tracing::info;

use crate::apps::App;

const PADDING: f32 = 18.0;
const GAP: f32 = 10.0;
/// A tile's widths to try, largest first; its picture is 3:4 of that.
const WIDTHS: [f32; 4] = [240.0, 190.0, 150.0, 110.0];
const INSET: f32 = 8.0;
const LABEL: f32 = 32.0;
const ICON: u32 = 20;
/// The panel's corners follow the tiles': their radius plus the padding.
const TILE_RADIUS: f32 = 14.0;
const RADIUS: f32 = TILE_RADIUS + PADDING;
/// The ring from tile to tile: 0.1 s, not past it, as the switcher fades in
/// (ADR 0032 §5; the owner on 0.5.132: quick, but not out of nowhere; it
/// swung 0.5 % past until 0.5.131).
const RING: Spring = Spring::new(0.1, 0.0);
/// An edge moves less than a quarter of a pixel: the ring rests.
const RING_UNSEEN: f64 = 0.25;

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
    /// The ring's edges as drawn – left, top, right, bottom –, on springs
    /// timed from `since`; whether it said it moves.
    ring: Option<[Motion; 4]>,
    since: Instant,
    ring_moves: bool,
    /// Every spring this many times longer (the smoke test's).
    slower: f64,
}

/// The four edges of `r`.
fn edges(r: Rect) -> [f64; 4] {
    [
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    ]
}

impl Switcher {
    pub fn new(room: (u32, u32)) -> Switcher {
        let slower = std::env::var("BEIZE_ANIMATIONS_SLOWER")
            .ok()
            .and_then(|factor| factor.trim().parse::<f64>().ok())
            .filter(|factor| factor.is_finite() && *factor >= 1.0)
            .unwrap_or(1.0);
        Switcher {
            entries: Vec::new(),
            selected: None,
            room,
            pointer: None,
            ring: None,
            since: Instant::now(),
            ring_moves: false,
            slower,
        }
    }

    /// What the compositor says now; whether it looks different.
    pub fn update(
        &mut self,
        entries: Vec<DesktopWindowId>,
        selected: Option<DesktopWindowId>,
    ) -> bool {
        let changed = entries != self.entries || selected != self.selected;
        // The tiles move: until it moves again, the pointer is on none, and
        // the ring is on its tile at once.
        let moved = entries != self.entries;
        if moved {
            self.pointer = None;
        }
        self.entries = entries;
        self.selected = selected;
        self.aim(moved);
        changed
    }

    /// The ring goes to the selected tile: there at once, `moved` tiles
    /// too, else on its spring.
    fn aim(&mut self, moved: bool) {
        let to = self
            .selected
            .and_then(|id| self.entries.iter().position(|e| *e == id))
            .map(|i| edges(self.tile(i)));
        let Some(to) = to else {
            self.ring = None;
            return;
        };
        let now = self.since.elapsed();
        let spring = Spring::new(RING.duration * self.slower, RING.bounce);
        match &mut self.ring {
            Some(ring) if !moved => {
                if ring.iter().zip(to).any(|(edge, to)| edge.target() != to) {
                    for (edge, to) in ring.iter_mut().zip(to) {
                        edge.to(to, spring, now);
                    }
                    if !self.ring_moves {
                        info!("switcher: ring moves");
                    }
                    self.ring_moves = true;
                }
            }
            _ => self.ring = Some(to.map(|edge| Motion::new(edge, RING_UNSEEN))),
        }
    }

    /// Whether the ring moves: it is drawn again once the switcher is
    /// ready.
    pub fn moving(&self) -> bool {
        self.ring
            .as_ref()
            .is_some_and(|ring| ring.iter().any(Motion::moving))
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
            ..Glass::one(all, RADIUS, true, Density::Dense)
        }
    }

    /// Its content: the selected tile's highlight, each tile's icon and
    /// title; the rest shows the glass, the pictures the compositor draws.
    pub fn draw(
        &mut self,
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
        let now = self.since.elapsed();
        let ring = self.ring.as_mut().map(|ring| {
            let [left, top, right, bottom] = [0, 1, 2, 3].map(|i| ring[i].at(now) as f32);
            Rect {
                x: left,
                y: top,
                w: right - left,
                h: bottom - top,
            }
        });
        if self.ring_moves && !self.moving() {
            self.ring_moves = false;
            info!("switcher: ring rests");
        }
        if let Some(ring) = ring {
            scene.rounded(ring, TILE_RADIUS, ACCENT);
        }
        // White on the tile the ring's middle is over.
        let centre = ring.map(|r| (r.x + r.w / 2.0, r.y + r.h / 2.0));
        for (i, id) in self.entries.iter().enumerate() {
            let tile = self.tile(i);
            let selected = centre.is_some_and(|(x, y)| {
                x >= tile.x && x < tile.x + tile.w && y >= tile.y && y < tile.y + tile.h
            });
            let style = Style {
                family: Family::Sans,
                size: SMALL_TEXT_SIZE,
                weight: 500.0,
                color: if selected {
                    ON_ACCENT
                } else {
                    theme::ink(appearance)
                },
            };
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

    #[test]
    fn the_ring_springs_to_the_selection_and_rests() {
        let mut switcher = Switcher::new((1280, 700));
        switcher.slower = 1.0;
        let (a, b, c) = (DesktopWindowId(0), DesktopWindowId(1), DesktopWindowId(2));
        // Shown: on its tile at once.
        switcher.update(vec![a, b], Some(b));
        assert!(!switcher.moving());
        // Another selected: it moves, and rests on that tile.
        switcher.update(vec![a, b], Some(a));
        assert!(switcher.moving());
        let to = edges(switcher.tile(0));
        let ring = switcher.ring.as_mut().unwrap();
        let rest = (1..2000u64)
            .find(|&ms| {
                let now = std::time::Duration::from_millis(ms);
                ring.iter_mut().for_each(|e| {
                    e.at(now);
                });
                !ring.iter().any(Motion::moving)
            })
            .unwrap_or(0);
        assert!((100..400).contains(&rest), "{rest} ms");
        assert!(ring.iter().zip(to).all(|(e, to)| e.target() == to));
        // The tiles move (a window joins): at once on its tile.
        switcher.update(vec![a, b, c], Some(a));
        assert!(!switcher.moving());
    }
}
