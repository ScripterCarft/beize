//! A menu (ADR 0028 §3): a list of commands with separators, drawn on menu
//! glass – live, tinted thickly, with a shadow, which the compositor draws
//! (`Material::Menu`) – in a popup (`App::popup`), as macOS' menus: the
//! Beize menu, the app menus, context and dock menus.
//!
//! Its measures are macOS 26's, measured in the menus of Apple's Human
//! Interface Guidelines (images at exactly 2 px per pt), at Beize's scale
//! (`measures::SCALE`): rows of 24 pt, 5 pt above the first and below the
//! last, text 13 pt – the menu bar's, 15 px in Beize – set in 16 pt, a
//! separator of 1 pt across the text's width with 5.5 pt each side, corners
//! of about 13 pt (16 px, the windows' radius). The highlight is
//! estimated: the accent colour, inset 5 pt, its corners following the
//! menu's. A command that opens another menu has a caret at the right
//! (`Item::submenu`); a group may have its symbols (`Item::symbol`), as
//! Apple's HIG has them for a whole group or none.
//!
//! A command is chosen as macOS chooses it: released over it – after a
//! press on the button that opened the menu and a drag, or a click –, or
//! with Return after the arrow keys; Escape closes the menu. A menu that
//! opens under a still pointer chooses nothing by the release of the press
//! that opened it (`armed`).

use beize_draw::measures::SCALE;
use beize_draw::{Color, Family, Rect, Scene, Style, Text};

use crate::Appearance;
use crate::desktop::{Glass, GlassShape, Material};
use crate::input::{Button, Input, Key, KeyState, Keysym, Pointer};
use crate::theme::{self, ACCENT, ON_ACCENT, TEXT_SIZE};

/// macOS' measures in pt (`SCALE` makes them Beize's).
const ROW: f32 = 24.0;
const PADDING: f32 = 5.0;
const INSET: f32 = 16.0;
const SEPARATOR: f32 = 11.0;
const HIGHLIGHT_INSET: f32 = 5.0;
/// The narrowest menu: Apple's examples, 180 pt.
const MIN_WIDTH: f32 = 180.0;
/// The widest, in px: a window's title may be as long as it likes.
const MAX_WIDTH: f32 = 480.0;
/// The corners: the windows' radius, as macOS 27's menus match them.
pub const RADIUS: f32 = 16.0;
/// The highlight's corners: the menu's, less its inset of 6 px.
const HIGHLIGHT_RADIUS: f32 = 10.0;
/// The symbol before a title and the space after it, the caret at the right
/// and the least space before it, a mark in the inset (Phosphor's; the
/// owner's, on mock-ups, 31.10.).
const SYMBOL: u32 = 18;
const SYMBOL_GAP: f32 = 8.0;
const CARET: u32 = 14;
const CARET_GAP: f32 = 14.0;
const CHECK: u32 = 14;
const DIAMOND: u32 = 11;
/// A menu with a mark gives all its rows the column for it (Apple's image
/// menus-toggled-item: mark from 17 pt, 8.5 wide, titles at 36): its centre and
/// the titles, in px at `SCALE`, as the owner chose them (31.10.).
const MARK_CENTRE: f32 = 26.5;
const MARKED_TITLE: f32 = 45.0;
/// How much of the ink a disabled command and a shortcut show (estimated,
/// macOS' tertiary and secondary labels), and the least space between a title
/// and its shortcut.
const DISABLED: u8 = 77;
const SHORTCUT: u8 = 140;
const SHORTCUT_GAP: f32 = 24.0;

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

/// What stands before a title in the inset, as macOS' lists of windows have
/// it: the active window, a minimized one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    #[default]
    None,
    Check,
    Diamond,
}

/// A command of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    /// Greyed, not to be chosen.
    pub enabled: bool,
    /// Its keys, shown at the right: the app handles them itself.
    pub shortcut: Option<String>,
    pub mark: Mark,
    /// A symbol before the title, by its name (`Scene::symbol`).
    pub symbol: Option<&'static str>,
    /// It opens another menu instead of being chosen (`Menu::opening`).
    pub submenu: bool,
}

/// One entry of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(Item),
    /// A line between groups of commands.
    Separator,
}

impl Entry {
    /// A command by its title.
    pub fn item(title: impl Into<String>) -> Entry {
        Entry::Item(Item {
            title: title.into(),
            enabled: true,
            shortcut: None,
            mark: Mark::None,
            symbol: None,
            submenu: false,
        })
    }

    /// `self` with its mark.
    pub fn mark(mut self, mark: Mark) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.mark = mark;
        }
        self
    }

    /// `self` with a symbol before its title.
    pub fn symbol(mut self, symbol: &'static str) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.symbol = Some(symbol);
        }
        self
    }

    /// `self`, opening another menu.
    pub fn submenu(mut self) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.submenu = true;
        }
        self
    }

    /// `self`, enabled or not.
    pub fn enabled(mut self, enabled: bool) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.enabled = enabled;
        }
        self
    }

    /// `self` with its keys shown.
    pub fn shortcut(mut self, keys: impl Into<String>) -> Entry {
        if let Entry::Item(item) = &mut self {
            item.shortcut = Some(keys.into());
        }
        self
    }

    fn chosen_able(&self) -> bool {
        matches!(self, Entry::Item(Item { enabled: true, .. }))
    }

    fn opens_menu(&self) -> bool {
        matches!(self, Entry::Item(Item { submenu: true, .. }))
    }
}

/// A menu of an app in the menu bar (`App::set_menus`): its title and
/// entries, each command with the id it comes back by (`Events::commands`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppMenu {
    pub title: String,
    pub entries: Vec<(Entry, u32)>,
}

/// What an input did to the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The left or right arrow: to the menu beside it in the bar (macOS).
    Neighbour(isize),
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

/// `title`, cut with an ellipsis to fit `room` px.
pub fn fitted(text: &mut Text, title: &str, style: &Style, room: f32) -> String {
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

pub struct Menu {
    entries: Vec<Entry>,
    /// The highlighted command, by its index in `entries`.
    highlight: Option<usize>,
    /// The pointer moved or a button went down over the menu since it
    /// opened: a release chooses now.
    armed: bool,
    /// The command whose menu should show (`Menu::opening`).
    opening: Option<usize>,
    /// The command whose menu shows: it stays highlighted while the pointer
    /// is over that menu (`Menu::hold`).
    held: Option<usize>,
}

impl Menu {
    pub fn new(entries: Vec<Entry>) -> Menu {
        Menu {
            entries,
            highlight: None,
            armed: false,
            opening: None,
            held: None,
        }
    }

    /// The command whose menu should show now: the pointer on it, or Right,
    /// Return or a click there. Its owner opens that menu in a popup of this
    /// one (`Align::Beside`), or closes it when this is none.
    pub fn opening(&self) -> Option<usize> {
        self.opening
    }

    /// Where the entry `i` lies in a menu `width` wide, for its menu's anchor.
    pub fn row(&self, i: usize, width: u32) -> Option<Rect> {
        self.rows(width as f32)
            .find(|(j, ..)| *j == i)
            .map(|(.., rect)| rect)
    }

    /// The menu of command `i` shows, or none does: `i` stays highlighted
    /// meanwhile.
    pub fn hold(&mut self, i: Option<usize>) {
        self.held = i;
        if i.is_none() {
            self.opening = None;
        }
    }

    /// Whether the entry `i` opens another menu.
    fn opens_menu(&self, i: usize) -> bool {
        self.entries.get(i).is_some_and(Entry::opens_menu)
    }

    fn style(appearance: Appearance, color: Option<Color>) -> Style {
        Style {
            family: Family::Sans,
            size: TEXT_SIZE,
            weight: 400.0,
            color: color.unwrap_or(theme::ink(appearance)),
        }
    }

    /// Where the titles start: further in when a row has a mark.
    fn title_inset(&self) -> f32 {
        let marked = |e: &Entry| matches!(e, Entry::Item(item) if item.mark != Mark::None);
        if self.entries.iter().any(marked) {
            MARKED_TITLE
        } else {
            INSET * SCALE
        }
    }

    /// The height of an entry.
    fn height(entry: &Entry) -> f32 {
        match entry {
            Entry::Item(_) => (ROW * SCALE).round(),
            Entry::Separator => (SEPARATOR * SCALE).round(),
        }
    }

    /// Its size: as wide as its longest row needs, between Apple's narrowest
    /// and `MAX_WIDTH`; a longer title is cut (`fitted`).
    pub fn size(&self, text: &mut Text) -> (u32, u32) {
        let style = Self::style(Appearance::Light, None);
        let widest = self
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item(item) => {
                    let keys = item.shortcut.as_ref().map_or(0.0, |keys| {
                        SHORTCUT_GAP * SCALE + text.layout(keys, &style, None).width()
                    });
                    let symbol = item.symbol.map_or(0.0, |_| SYMBOL as f32 + SYMBOL_GAP);
                    let caret = if item.submenu {
                        CARET_GAP + CARET as f32
                    } else {
                        0.0
                    };
                    Some(symbol + text.layout(&item.title, &style, None).width() + keys + caret)
                }
                Entry::Separator => None,
            })
            .fold(0.0, f32::max);
        let w = (widest + self.title_inset() + INSET * SCALE)
            .clamp(MIN_WIDTH * SCALE, MAX_WIDTH)
            .ceil();
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
            .find(|(_, entry, r)| entry.chosen_able() && y >= r.y && y < r.y + r.h)
            .map(|(i, _, _)| i)
    }

    /// The next command from the highlight up (`-1`) or down (`1`), round.
    fn step(&self, by: isize) -> Option<usize> {
        let n = self.entries.len() as isize;
        let commands = |i: &isize| self.entries[*i as usize].chosen_able();
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
        let mut by_pointer = true;
        let highlight = match input {
            Input::Pointer(Pointer::Enter(x, y)) => self.command_at(w, (*x, *y)),
            Input::Pointer(Pointer::Motion(x, y)) => {
                self.armed = true;
                self.command_at(w, (*x, *y))
            }
            Input::Pointer(Pointer::Leave) => None,
            Input::Pointer(Pointer::Button {
                button: Button::Left | Button::Right,
                pressed: true,
            }) => {
                self.armed = true;
                return Outcome::None;
            }
            Input::Pointer(Pointer::Button {
                button: Button::Left | Button::Right,
                pressed: false,
            }) => {
                if !self.armed {
                    return Outcome::None;
                }
                return self.choose();
            }
            Input::Key(Key {
                sym,
                state: KeyState::Pressed | KeyState::Repeated,
                ..
            }) => {
                by_pointer = false;
                match *sym {
                    Keysym::Down => self.step(1),
                    Keysym::Up => self.step(-1),
                    Keysym::Left => return Outcome::Neighbour(-1),
                    Keysym::Right => {
                        return match self.highlight.filter(|i| self.opens_menu(*i)) {
                            Some(_) => self.choose(),
                            None => Outcome::Neighbour(1),
                        };
                    }
                    Keysym::Return | Keysym::KP_Enter | Keysym::space => return self.choose(),
                    Keysym::Escape => return Outcome::Dismissed,
                    _ => return Outcome::None,
                }
            }
            _ => return Outcome::None,
        };
        // Off its rows, the command whose menu shows stays lit: the pointer
        // is on the way there, or on it.
        let highlight = highlight.or(self.held);
        if by_pointer {
            self.opening = highlight.filter(|i| self.opens_menu(*i));
        }
        if highlight == self.highlight {
            return Outcome::None;
        }
        self.highlight = highlight;
        Outcome::Changed
    }

    /// The highlighted command is chosen, or its menu opens.
    fn choose(&mut self) -> Outcome {
        match self.highlight {
            Some(i) if self.opens_menu(i) => {
                self.opening = Some(i);
                Outcome::Changed
            }
            Some(i) => Outcome::Chosen(i),
            None => Outcome::None,
        }
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
        let title_inset = self.title_inset();
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
                Entry::Item(item) => {
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
                            HIGHLIGHT_RADIUS,
                            ACCENT,
                        );
                    }
                    let ink = Self::style(appearance, lit.then_some(ON_ACCENT)).color;
                    let shown = Color {
                        a: if item.enabled { ink.a } else { DISABLED },
                        ..ink
                    };
                    let style = Self::style(appearance, Some(shown));
                    let keys_width = item.shortcut.as_ref().map_or(0.0, |keys| {
                        SHORTCUT_GAP * SCALE + text.layout(keys, &style, None).width()
                    });
                    let caret_width = if item.submenu {
                        CARET_GAP + CARET as f32
                    } else {
                        0.0
                    };
                    let symbol_width = item.symbol.map_or(0.0, |_| SYMBOL as f32 + SYMBOL_GAP);
                    let room = w - title_inset - inset - symbol_width - keys_width - caret_width;
                    let title = fitted(text, &item.title, &style, room);
                    let layout = text.layout(&title, &style, None);
                    let y = (row.y + (row.h - layout.height()) / 2.0).round();
                    let centred = |size: u32| (row.y + (row.h - size as f32) / 2.0).round();
                    match item.mark {
                        Mark::None => {}
                        Mark::Check => {
                            let x = (MARK_CENTRE - CHECK as f32 / 2.0).round();
                            scene.symbol("check-bold", x, centred(CHECK), CHECK, shown);
                        }
                        Mark::Diamond => {
                            let x = (MARK_CENTRE - DIAMOND as f32 / 2.0).round();
                            scene.symbol("diamond-fill", x, centred(DIAMOND), DIAMOND, shown);
                        }
                    }
                    let title_x = match item.symbol {
                        Some(symbol) => {
                            scene.symbol(symbol, title_inset, centred(SYMBOL), SYMBOL, shown);
                            title_inset + SYMBOL as f32 + SYMBOL_GAP
                        }
                        None => title_inset,
                    };
                    scene.text(&layout, title_x, y);
                    if item.submenu {
                        let x = w - inset - CARET as f32;
                        scene.symbol("caret-right-bold", x, centred(CARET), CARET, shown);
                    }
                    if let Some(keys) = &item.shortcut {
                        let keys_ink = Color {
                            a: if item.enabled { SHORTCUT } else { DISABLED },
                            ..ink
                        };
                        let style = Self::style(appearance, Some(keys_ink));
                        let layout = text.layout(keys, &style, None);
                        let x = (w - inset - caret_width - layout.width()).round();
                        scene.text(&layout, x, y);
                    }
                }
            }
        }
        scene
    }
}
