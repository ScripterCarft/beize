//! The control center (ADR 0028 §5, ADR 0033 §6): one surface of dense
//! Beize glass under the menu bar's right end, its tiles on it – neutral
//! fills, not glass of their own –, as the control centers of tvOS and of
//! the cars Beize takes its manner from: one calm plane, the tiles in it.
//! A popup of the menu bar (`App::popup`): a click beside it closes it.
//!
//! Its values are the design tokens' (`tokens`): the surface's radius, the
//! tiles' (the element's: surface less the inset, so they sit concentric in
//! it), the inset all round and between tiles. A tile is one cell (a
//! toggle), two across (a button with a title and a line below), two by two
//! or a row of four. The layout is the owner's (28.10.): network top left,
//! the media player beside it, Bluetooth below the network, light/dark
//! below that, then brightness and volume. Only tiles whose service is
//! there show (no empty tiles, §5): network and light/dark now; Bluetooth,
//! media and volume come with PipeWire and BlueZ (#17), brightness with
//! them. Rows left empty close up.

use beize_ui::tokens::{self, Palette};
use beize_ui::{Appearance, Color, Density, Family, Glass, GlassShape, Rect, Scene, Style, Text};

use crate::network::{Kind, Status};

/// A cell of the grid: about macOS 26's 64 pt at Beize's scale and the
/// owner's size (28.10.: 75 px), on the grid.
const CELL: f32 = 19.0 * tokens::GRID;
/// Below the menu bar, as between tiles.
const TOP: f32 = tokens::INSET;
/// A wide tile's round button: its margin to the tile's edge the same all
/// round, and its symbol.
const BUTTON: f32 = 11.0 * tokens::GRID;
const SYMBOL: u32 = 5 * tokens::GRID as u32;
/// A toggle's symbol, larger: it fills its tile alone.
const TOGGLE_SYMBOL: u32 = 7 * tokens::GRID as u32;
/// Between a title and the line below it.
const LINE_GAP: f32 = 2.0;

/// The tiles there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tile {
    Network,
    Appearance,
}

/// The layout's rows.
const ROWS: u32 = 5;

/// Where each tile lies in the owner's layout: column, row, width, height
/// in cells. Those not built yet are listed for their place. The control
/// center is as wide as the columns used, right under its button.
const LAYOUT: [(Option<Tile>, u32, u32, u32, u32); 6] = [
    (Some(Tile::Network), 0, 0, 2, 1),
    (None, 2, 0, 2, 2), // the media player
    (None, 0, 1, 2, 1), // Bluetooth
    (Some(Tile::Appearance), 0, 2, 1, 1),
    (None, 0, 3, 4, 1), // brightness
    (None, 0, 4, 4, 1), // volume
];

/// What a click in the control center does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Switch to the other appearance.
    Appearance(Appearance),
}

pub struct ControlCenter {
    /// Each tile there is, with its rectangle in the popup.
    tiles: Vec<(Tile, Rect)>,
    /// The glass surface the tiles lie on.
    surface: Rect,
    size: (u32, u32),
}

impl ControlCenter {
    pub fn new() -> ControlCenter {
        let (cell, gap, inset) = (CELL, tokens::INSET, tokens::INSET);
        let shown: Vec<_> = LAYOUT
            .iter()
            .filter_map(|&(tile, col, row, w, h)| Some((tile?, col, row, w, h)))
            .collect();
        // Rows without a tile close up.
        let used = |row: u32| shown.iter().any(|t| t.2 == row);
        let row_of = |row: u32| (0..row).filter(|&r| used(r)).count() as f32;
        let rows = (0..ROWS).filter(|&r| used(r)).count() as f32;
        // As wide as the columns used, so it lies under its button.
        let columns = shown.iter().map(|t| t.1 + t.3).max().unwrap_or(1) as f32;
        let tiles = shown
            .iter()
            .map(|&(tile, col, row, w, h)| {
                let rect = Rect {
                    x: inset + col as f32 * (cell + gap),
                    y: TOP + inset + row_of(row) * (cell + gap),
                    w: w as f32 * cell + (w - 1) as f32 * gap,
                    h: h as f32 * cell + (h - 1) as f32 * gap,
                };
                (tile, rect)
            })
            .collect();
        let surface = Rect {
            x: 0.0,
            y: TOP,
            w: 2.0 * inset + columns * (cell + gap) - gap,
            h: 2.0 * inset + rows * (cell + gap) - gap,
        };
        ControlCenter {
            tiles,
            surface,
            size: (surface.w as u32, (surface.y + surface.h) as u32),
        }
    }

    /// The popup's size: the surface, and the space above it below the bar.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Where it takes the pointer: its surface. Beside it a click goes to
    /// what lies below, and closes it.
    pub fn input(&self) -> Vec<Rect> {
        vec![self.surface]
    }

    /// Its glass: one dense surface with the surface's radius, a rim and the
    /// floating shadow.
    pub fn glass(&self) -> Glass {
        Glass {
            shapes: vec![GlassShape {
                rect: self.surface,
                radius: tokens::RADIUS_SURFACE,
                rim: true,
            }],
            density: Density::Dense,
            shadow: true,
        }
    }

    /// What a click at `(x, y)` does, on a desktop in `appearance`.
    pub fn click(&self, (x, y): (f64, f64), appearance: Appearance) -> Option<Action> {
        let (x, y) = (x as f32, y as f32);
        let (tile, _) = self
            .tiles
            .iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)?;
        match tile {
            Tile::Appearance => Some(Action::Appearance(match appearance {
                Appearance::Light => Appearance::Dark,
                Appearance::Dark => Appearance::Light,
            })),
            // Its details come with Wi-Fi (#17).
            Tile::Network => None,
        }
    }

    /// The tiles for the `network` and `appearance` on the glass; the rest
    /// is transparent, the glass below shows.
    pub fn draw(&self, text: &mut Text, network: Status, appearance: Appearance) -> Scene {
        let p = palette(appearance);
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let mut scene = Scene::new();
        scene.clear(Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        for &(tile, r) in &self.tiles {
            match tile {
                Tile::Network => {
                    scene.rounded(r, tokens::RADIUS_ELEMENT, p.fill);
                    // On: the accent with a white symbol, as every switch.
                    let margin = ((r.h - BUTTON) / 2.0).round();
                    let centre = [r.x + margin + BUTTON / 2.0, r.y + r.h / 2.0];
                    let (disc, symbol, color) = if network.connected {
                        (p.accent, "globe-simple", WHITE)
                    } else {
                        (p.fill_chosen, "globe-simple-x", p.ink)
                    };
                    scene.circle(centre, BUTTON / 2.0, disc);
                    let half = SYMBOL as f32 / 2.0;
                    scene.symbol(symbol, centre[0] - half, centre[1] - half, SYMBOL, color);
                    let title = match network.kind {
                        Some(Kind::Ethernet) => "Ethernet",
                        Some(Kind::WiFi) => "Wi-Fi",
                        None => "Network",
                    };
                    let state = if network.connected {
                        "Connected"
                    } else {
                        "Not Connected"
                    };
                    let x = r.x + margin + BUTTON + tokens::INSET;
                    let title =
                        text.layout(title, &style(tokens::TEXT, tokens::SEMIBOLD, p.ink), None);
                    let state = text.layout(
                        state,
                        &style(tokens::TEXT_SMALL, tokens::REGULAR, p.ink_secondary),
                        None,
                    );
                    let block = title.height() + LINE_GAP + state.height();
                    let y = (r.y + (r.h - block) / 2.0).round();
                    scene.text(&title, x, y);
                    scene.text(&state, x, (y + title.height() + LINE_GAP).round());
                }
                Tile::Appearance => {
                    // On – the accent – when dark is on, as macOS' switch.
                    let (fill, color) = if appearance == Appearance::Dark {
                        (p.accent, WHITE)
                    } else {
                        (p.fill, p.ink)
                    };
                    scene.rounded(r, tokens::RADIUS_ELEMENT, fill);
                    let half = TOGGLE_SYMBOL as f32 / 2.0;
                    scene.symbol(
                        "circle-half-fill",
                        (r.x + r.w / 2.0 - half).round(),
                        (r.y + r.h / 2.0 - half).round(),
                        TOGGLE_SYMBOL,
                        color,
                    );
                }
            }
        }
        scene
    }
}

/// White on the accent (`tokens`: 5.6 : 1).
const WHITE: Color = Color::rgb(0xffffff);

fn palette(appearance: Appearance) -> Palette {
    match appearance {
        Appearance::Light => tokens::LIGHT,
        Appearance::Dark => tokens::DARK,
    }
}

fn style(size: f32, weight: f32, color: Color) -> Style {
    Style {
        family: Family::Sans,
        size,
        weight,
        color,
    }
}
