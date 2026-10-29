//! The control center (ADR 0028 §5): tiles of clear glass under the menu
//! bar's right end, without a panel around them – each floats, what lies
//! behind shows blurred through it and, softer, around it (the smoke), as
//! macOS 26's. A popup of the menu bar (`App::popup`): a click elsewhere
//! closes it; the smoke takes no input.
//!
//! Its grid is macOS 26's, measured in Apple's press image of it (3840 px,
//! ~3.94 px per pt by the menu bar's cap height; CLAUDE.md §11): four
//! columns of 64 pt with 12 pt between – 292 pt across, which the image
//! confirms –, at Beize's scale (`measures::SCALE`), 6.5 % smaller (`SIZE`,
//! the owner's); its right edge under the clock's. A tile is one cell
//! (a circle), two across (a capsule), two by two (corners of 21 pt) or a
//! row of four (a slider, corners of 16.5 pt). The layout is the owner's
//! (28.10.): network top left, the media player beside it, Bluetooth below
//! the network, light/dark below that, then brightness and volume. Only
//! tiles whose service is there show (no empty tiles, §5): network and
//! light/dark now; Bluetooth, media and volume come with PipeWire and BlueZ
//! (#17), brightness with them. Rows left empty close up.

use beize_ui::measures::SCALE;
use beize_ui::{
    Appearance, Color, Family, Glass, GlassShape, Material, Rect, Scene, Smoke, Style, Text,
};

use crate::network::{Kind, Status};

/// macOS' measures in pt (`SCALE` and `SIZE` make them Beize's).
const CELL: f32 = 64.0;
const GAP: f32 = 12.0;
/// Below the menu bar, as between tiles.
const TOP: f32 = 12.0;
const BIG_RADIUS: f32 = 21.0;
/// A capsule's round button and where its centre and the text lie.
const BUTTON: f32 = 36.0;
const BUTTON_AT: f32 = 32.0;
const TEXT_AT: f32 = 56.0;
/// The owner's size (28.10.): 6.5 % below macOS' at Beize's scale – a cell
/// 75 px instead of 80; what lies in a tile shrinks with it.
const SIZE: f32 = 0.935;
/// A capsule's title and the line below, in px at `SCALE`, shrunk by
/// `SIZE`; the title 10 % lighter than semibold, and a little space
/// between the lines (both the owner's, 28.10.).
const TITLE: (f32, f32) = (15.0, 540.0);
const SUBTITLE: (f32, f32) = (13.0, 400.0);
const LINE_GAP: f32 = 1.5;

/// The text and symbols on the glass: white in both appearances, the line
/// below at 70 % (approved on mock-ups, 29.10.) – the clear glass holds its
/// light down for it.
const INK: Color = Color::rgb(0xffffff);
const SUB: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 178,
};
/// A switch that is on: a white disc, its symbol in the accent colour.
const ON: Color = Color::rgb(0xffffff);
const ACCENT: Color = Color::rgb(0x007aff);
/// A switch that is off: a faint disc on the glass.
const OFF: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 38,
};
/// The smoke around the tiles: what lies behind and between them blurred,
/// less and less over 130 px from their edges, by 4 px at most – a window
/// behind does not show sharp beside them (the owner, 29.10., on mock-ups;
/// macOS 26's press image blurs there too).
const SMOKE: Smoke = Smoke {
    falloff: 130.0,
    strength: 4.0,
};

fn px(pt: f32) -> f32 {
    (pt * SCALE * SIZE).round()
}

/// A symbol's square, given in px at `SCALE`.
fn symbol_size(size: f32) -> u32 {
    (size * SIZE).round() as u32
}

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
    /// Each tile there is, with its rectangle in the popup and its corners.
    tiles: Vec<(Tile, Rect, f32)>,
    size: (u32, u32),
}

impl ControlCenter {
    pub fn new() -> ControlCenter {
        let (cell, gap) = (px(CELL), px(GAP));
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
        let top = px(TOP);
        let tiles = shown
            .iter()
            .map(|&(tile, col, row, w, h)| {
                let rect = Rect {
                    x: col as f32 * (cell + gap),
                    y: top + row_of(row) * (cell + gap),
                    w: w as f32 * cell + (w - 1) as f32 * gap,
                    h: h as f32 * cell + (h - 1) as f32 * gap,
                };
                let radius = if h == 1 { rect.h / 2.0 } else { px(BIG_RADIUS) };
                (tile, rect, radius)
            })
            .collect();
        let width = columns * (cell + gap) - gap;
        let height = top + rows * (cell + gap) - gap;
        ControlCenter {
            tiles,
            size: (width as u32, height as u32),
        }
    }

    /// The popup's size: the grid, and the space above it below the bar.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Where it takes the pointer: its tiles. Beside them a click goes to
    /// what lies below, and closes it.
    pub fn input(&self) -> Vec<Rect> {
        self.tiles.iter().map(|&(_, rect, _)| rect).collect()
    }

    /// The tiles' glass: clear, with a rim and a shadow each, the smoke
    /// around them.
    pub fn glass(&self) -> Glass {
        Glass {
            shapes: self
                .tiles
                .iter()
                .map(|&(_, rect, radius)| GlassShape {
                    rect,
                    radius,
                    rim: true,
                })
                .collect(),
            material: Material::Clear,
            shadow: true,
            smoke: Some(SMOKE),
        }
    }

    /// What a click at `(x, y)` does, on a desktop in `appearance`.
    pub fn click(&self, (x, y): (f64, f64), appearance: Appearance) -> Option<Action> {
        let (x, y) = (x as f32, y as f32);
        let (tile, _, _) = self
            .tiles
            .iter()
            .find(|(_, r, _)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)?;
        match tile {
            Tile::Appearance => Some(Action::Appearance(match appearance {
                Appearance::Light => Appearance::Dark,
                Appearance::Dark => Appearance::Light,
            })),
            // Its details come with Wi-Fi (#17).
            Tile::Network => None,
        }
    }

    /// The tiles' content for the `network` and `appearance`; the rest is
    /// transparent, the glass below shows.
    pub fn draw(&self, text: &mut Text, network: Status, appearance: Appearance) -> Scene {
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let mut scene = Scene::new();
        scene.clear(Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        for &(tile, r, _) in &self.tiles {
            match tile {
                Tile::Network => {
                    let centre = [r.x + px(BUTTON_AT), r.y + r.h / 2.0];
                    let (disc, symbol, color) = if network.connected {
                        (ON, "globe-simple", ACCENT)
                    } else {
                        (OFF, "globe-simple-x", INK)
                    };
                    scene.circle(centre, px(BUTTON) / 2.0, disc);
                    let size = symbol_size(22.0);
                    let half = size as f32 / 2.0;
                    scene.symbol(symbol, centre[0] - half, centre[1] - half, size, color);
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
                    let x = r.x + px(TEXT_AT);
                    let title = text.layout(title, &style(TITLE, INK), None);
                    let state = text.layout(state, &style(SUBTITLE, SUB), None);
                    let gap = px(LINE_GAP);
                    let block = title.height() + gap + state.height();
                    let y = (r.y + (r.h - block) / 2.0).round();
                    scene.text(&title, x, y);
                    scene.text(&state, x, (y + title.height() + gap).round());
                }
                Tile::Appearance => {
                    let centre = [r.x + r.w / 2.0, r.y + r.h / 2.0];
                    // On – a white disc – when dark is on, as macOS' switch.
                    let color = if appearance == Appearance::Dark {
                        scene.circle(centre, r.w / 2.0, ON);
                        ACCENT
                    } else {
                        INK
                    };
                    // A pixel larger each way than at `SIZE` (owner, 28.10.).
                    let size = symbol_size(30.0) + 2;
                    let half = size as f32 / 2.0;
                    scene.symbol(
                        "circle-half-fill",
                        centre[0] - half,
                        centre[1] - half,
                        size,
                        color,
                    );
                }
            }
        }
        scene
    }
}

fn style((size, weight): (f32, f32), color: Color) -> Style {
    Style {
        family: Family::Sans,
        size: size * SIZE,
        weight,
        color,
    }
}
