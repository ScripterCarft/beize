//! A menu (ADR 0028 §3): a list of commands with separators, drawn on menu
//! glass – live, tinted thickly, with a shadow, which the compositor draws
//! (`Material::Menu`) – in a popup (`App::popup`), as macOS' menus: the
//! Beize menu now, context and app menus later.
//!
//! Its measures are macOS 26's, measured in the menus of Apple's Human
//! Interface Guidelines (images at exactly 2 px per pt), at Beize's scale
//! (`measures::SCALE`): rows of 24 pt, 5 pt above the first and below the
//! last, text 13 pt – the menu bar's, 15 px in Beize – set in 16 pt, a
//! separator of 1 pt across the text's width with 5.5 pt each side, corners
//! of about 13 pt (16 px, the windows' radius). The highlight is
//! estimated: the accent colour, inset 5 pt, corners of 6 pt.
//!
//! A command is chosen as macOS chooses it: released over it – after a
//! press on the button that opened the menu and a drag, or a click –, or
//! with Return after the arrow keys; Escape closes the menu.

use beize_draw::measures::SCALE;
use beize_draw::{Color, Family, Rect, Scene, Style, Text};

use crate::Appearance;
use crate::desktop::{Glass, GlassShape, Material};
use crate::input::{Button, Input, Key, KeyState, Keysym, Pointer};

/// macOS' measures in pt (`SCALE` makes them Beize's).
const ROW: f32 = 24.0;
const PADDING: f32 = 5.0;
const INSET: f32 = 16.0;
const SEPARATOR: f32 = 11.0;
const HIGHLIGHT_INSET: f32 = 5.0;
const HIGHLIGHT_RADIUS: f32 = 6.0;
/// The narrowest menu: Apple's examples, 180 pt.
const MIN_WIDTH: f32 = 180.0;
/// The corners: the windows' radius, as macOS 27's menus match them.
pub const RADIUS: f32 = 16.0;
/// The text's size: the menu bar's.
const TEXT_SIZE: f32 = 15.0;

/// The text in Apple's menus (#292929, measured) and on dark ones; the
/// highlight's (macOS' accent blue) and the text on it.
const INK_LIGHT: Color = Color::rgb(0x292929);
const INK_DARK: Color = Color::rgb(0xe8e8e8);
const ACCENT: Color = Color::rgb(0x007aff);
const ON_ACCENT: Color = Color::rgb(0xffffff);
/// The separator: black 7.5 % on light (measured), white 10 % on dark.
const LINE_LIGHT: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 19,
};
const LINE_DARK: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 26,
};

/// One entry of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A command, by its title.
    Item(String),
    /// A line between groups of commands.
    Separator,
}

/// What an input did to the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to show.
    None,
    /// The highlight moved: draw again.
    Changed,
    /// The command at this index of the entries was chosen: close the menu
    /// and act.
    Chosen(usize),
    /// Escape: close it.
    Dismissed,
}

pub struct Menu {
    entries: Vec<Entry>,
    /// The highlighted command, by its index in `entries`.
    highlight: Option<usize>,
}

impl Menu {
    pub fn new(entries: Vec<Entry>) -> Menu {
        Menu {
            entries,
            highlight: None,
        }
    }

    fn style(appearance: Appearance, color: Option<Color>) -> Style {
        Style {
            family: Family::Sans,
            size: TEXT_SIZE,
            weight: 400.0,
            color: color.unwrap_or(match appearance {
                Appearance::Light => INK_LIGHT,
                Appearance::Dark => INK_DARK,
            }),
        }
    }

    /// The height of an entry.
    fn height(entry: &Entry) -> f32 {
        match entry {
            Entry::Item(_) => (ROW * SCALE).round(),
            Entry::Separator => (SEPARATOR * SCALE).round(),
        }
    }

    /// Its size: as wide as its longest title needs, at least Apple's
    /// narrowest.
    pub fn size(&self, text: &mut Text) -> (u32, u32) {
        let style = Self::style(Appearance::Light, None);
        let widest = self
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(title) => Some(text.layout(title, &style, None).width()),
                Entry::Separator => None,
            })
            .fold(0.0, f32::max);
        let w = (widest + 2.0 * INSET * SCALE).max(MIN_WIDTH * SCALE).ceil();
        let h =
            2.0 * (PADDING * SCALE).round() + self.entries.iter().map(Self::height).sum::<f32>();
        (w as u32, h as u32)
    }

    /// The glass below a menu of `size`: all of it, menu glass with a rim
    /// and a shadow.
    pub fn glass((w, h): (u32, u32)) -> Glass {
        Glass {
            shapes: vec![GlassShape {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: w as f32,
                    h: h as f32,
                },
                radius: RADIUS,
                rim: true,
            }],
            material: Material::Menu,
            shadow: true,
            smoke: None,
        }
    }

    /// Where each entry lies, top to bottom, in a menu `w` wide.
    fn rows(&self, w: f32) -> impl Iterator<Item = (usize, &Entry, Rect)> {
        let mut y = (PADDING * SCALE).round();
        self.entries.iter().enumerate().map(move |(i, entry)| {
            let h = Self::height(entry);
            let rect = Rect { x: 0.0, y, w, h };
            y += h;
            (i, entry, rect)
        })
    }

    /// The command under the point `(x, y)` of a menu `w` wide.
    fn command_at(&self, w: f32, (x, y): (f64, f64)) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        if x < 0.0 || x >= w {
            return None;
        }
        self.rows(w)
            .find(|(_, entry, r)| matches!(entry, Entry::Item(_)) && y >= r.y && y < r.y + r.h)
            .map(|(i, _, _)| i)
    }

    /// The next command from the highlight up (`-1`) or down (`1`), round.
    fn step(&self, by: isize) -> Option<usize> {
        let n = self.entries.len() as isize;
        let commands = |i: &isize| matches!(self.entries[*i as usize], Entry::Item(_));
        let start = self
            .highlight
            .map_or(if by > 0 { -1 } else { n }, |h| h as isize);
        (1..=n)
            .map(|k| (start + by * k).rem_euclid(n))
            .find(commands)
            .map(|i| i as usize)
    }

    /// What `input` – to the menu's popup, `width` wide – does.
    pub fn input(&mut self, input: &Input, width: u32) -> Outcome {
        let w = width as f32;
        let highlight = match input {
            Input::Pointer(Pointer::Enter(x, y) | Pointer::Motion(x, y)) => {
                self.command_at(w, (*x, *y))
            }
            Input::Pointer(Pointer::Leave) => None,
            Input::Pointer(Pointer::Button {
                button: Button::Left,
                pressed: false,
            }) => {
                return self.highlight.map_or(Outcome::None, Outcome::Chosen);
            }
            Input::Key(Key {
                sym,
                state: KeyState::Pressed | KeyState::Repeated,
                ..
            }) => match *sym {
                Keysym::Down => self.step(1),
                Keysym::Up => self.step(-1),
                Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                    return self.highlight.map_or(Outcome::None, Outcome::Chosen);
                }
                Keysym::Escape => return Outcome::Dismissed,
                _ => return Outcome::None,
            },
            _ => return Outcome::None,
        };
        if highlight == self.highlight {
            return Outcome::None;
        }
        self.highlight = highlight;
        Outcome::Changed
    }

    /// The menu's content in `(w, h)` for `appearance`: the rest is
    /// transparent, the glass below shows.
    pub fn draw(&self, text: &mut Text, (w, h): (u32, u32), appearance: Appearance) -> Scene {
        let (w, h) = (w as f32, h as f32);
        let mut scene = Scene::new();
        scene.clear(Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        let inset = (INSET * SCALE).round();
        for (i, entry, row) in self.rows(w) {
            match entry {
                Entry::Separator => {
                    let line = match appearance {
                        Appearance::Light => LINE_LIGHT,
                        Appearance::Dark => LINE_DARK,
                    };
                    let y = (row.y + row.h / 2.0).floor();
                    scene.fill(
                        Rect {
                            x: inset,
                            y,
                            w: w - 2.0 * inset,
                            h: 1.0,
                        },
                        line,
                    );
                }
                Entry::Item(title) => {
                    let lit = self.highlight == Some(i);
                    if lit {
                        let s = (HIGHLIGHT_INSET * SCALE).round();
                        scene.rounded(
                            Rect {
                                x: s,
                                y: row.y,
                                w: w - 2.0 * s,
                                h: row.h,
                            },
                            (HIGHLIGHT_RADIUS * SCALE).round(),
                            ACCENT,
                        );
                    }
                    let style = Self::style(appearance, lit.then_some(ON_ACCENT));
                    let layout = text.layout(title, &style, None);
                    let y = (row.y + (row.h - layout.height()) / 2.0).round();
                    scene.text(&layout, inset, y);
                }
            }
        }
        scene
    }
}
