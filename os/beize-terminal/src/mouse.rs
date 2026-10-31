//! The pointer in the terminal: selecting (a click and drag; double click
//! a word, triple click a line), the wheel through the scrollback – or,
//! when a program asks for the mouse (htop, vim), reports to it in xterm's
//! encodings. Shift keeps the pointer the terminal's, as in xterm and VTE.

use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use beize_ui::input::{Button, Pointer};
use beize_ui::{Cells, Modifiers};

use crate::view;

/// Clicks this close in time on the same cell count as one double click
/// (GNOME's default).
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Lines per wheel notch, as VTE and Alacritty.
const LINES_PER_NOTCH: f64 = 3.0;

#[derive(Default)]
pub struct Mouse {
    x: f64,
    y: f64,
    /// Buttons held, for drags and motion reports.
    left: bool,
    held: Option<u8>,
    clicks: u8,
    last_click: Option<(Instant, Point)>,
    /// Touchpad scrolling below a line, carried over.
    scrolled: f64,
}

impl Mouse {
    /// Where the pointer is on the content.
    pub fn at(&self) -> (f32, f32) {
        (self.x as f32, self.y as f32)
    }

    /// Handles one pointer event; bytes for the program go to `out`.
    /// True if what the window shows changed.
    pub fn event<T: EventListener>(
        &mut self,
        event: &Pointer,
        mods: Modifiers,
        term: &mut Term<T>,
        cells: &Cells,
        out: &mut Vec<u8>,
    ) -> bool {
        let mode = *term.mode();
        let reporting = mode.intersects(TermMode::MOUSE_MODE) && !mods.shift;
        match *event {
            Pointer::Enter(x, y) | Pointer::Motion(x, y) => {
                let before = self.cell(term, cells);
                (self.x, self.y) = (x, y);
                let now = self.cell(term, cells);
                if reporting {
                    let motion = match self.held {
                        Some(_) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
                        None => mode.contains(TermMode::MOUSE_MOTION),
                    };
                    if motion && now != before {
                        // 32: motion; 3 is "no button".
                        let button = self.held.unwrap_or(3) + 32;
                        report(
                            out,
                            mode,
                            mods,
                            button,
                            true,
                            now,
                            term.grid().display_offset(),
                        );
                    }
                    return false;
                }
                if self.left && now != before {
                    let side = self.side(cells);
                    if let Some(selection) = &mut term.selection {
                        selection.update(now, side);
                        return true;
                    }
                }
                false
            }
            Pointer::Leave => false,
            Pointer::Button { button, pressed } => {
                let code = match button {
                    Button::Left => 0,
                    Button::Middle => 1,
                    Button::Right => 2,
                    Button::Other(_) => return false,
                };
                let point = self.cell(term, cells);
                if reporting {
                    self.held = pressed.then_some(code);
                    report(
                        out,
                        mode,
                        mods,
                        code,
                        pressed,
                        point,
                        term.grid().display_offset(),
                    );
                    return false;
                }
                if button != Button::Left {
                    return false;
                }
                self.left = pressed;
                if !pressed {
                    return false;
                }
                let now = Instant::now();
                self.clicks = match self.last_click {
                    Some((at, p)) if p == point && now - at < MULTI_CLICK => self.clicks % 3 + 1,
                    _ => 1,
                };
                self.last_click = Some((now, point));
                let kind = match self.clicks {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                let side = self.side(cells);
                let mut selection = Selection::new(kind, point, side);
                if kind != SelectionType::Simple {
                    // A word or line is selected at once, without a drag.
                    selection.include_all();
                }
                term.selection = Some(selection);
                true
            }
            Pointer::Scroll { y, steps_y, .. } => {
                let lines = if steps_y != 0.0 {
                    steps_y * LINES_PER_NOTCH
                } else {
                    // A touchpad: pixels, one line per cell height.
                    y / f64::from(cells.height)
                };
                self.scrolled += lines;
                let whole = self.scrolled.trunc();
                self.scrolled -= whole;
                let whole = whole as i32;
                if whole == 0 {
                    return false;
                }
                if reporting {
                    // 64: wheel up, 65: down.
                    let button = if whole < 0 { 64 } else { 65 };
                    let point = self.cell(term, cells);
                    for _ in 0..whole.unsigned_abs() {
                        report(
                            out,
                            mode,
                            mods,
                            button,
                            true,
                            point,
                            term.grid().display_offset(),
                        );
                    }
                    return false;
                }
                if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                    // less, man: the wheel as cursor keys.
                    let key = match (whole < 0, mode.contains(TermMode::APP_CURSOR)) {
                        (true, true) => "\x1bOA",
                        (true, false) => "\x1b[A",
                        (false, true) => "\x1bOB",
                        (false, false) => "\x1b[B",
                    };
                    for _ in 0..whole.unsigned_abs() {
                        out.extend_from_slice(key.as_bytes());
                    }
                    return false;
                }
                // Up (negative) shows older lines.
                term.scroll_display(Scroll::Delta(-whole));
                true
            }
        }
    }

    fn cell<T: EventListener>(&self, term: &Term<T>, cells: &Cells) -> Point {
        let grid = term.grid();
        view::point(
            self.x,
            self.y,
            cells,
            grid.columns(),
            grid.screen_lines(),
            grid.display_offset(),
        )
    }

    /// Which half of its cell the pointer is on.
    fn side(&self, cells: &Cells) -> Side {
        let within = (self.x as f32 - view::PADDING).rem_euclid(cells.width);
        if within < cells.width / 2.0 {
            Side::Left
        } else {
            Side::Right
        }
    }
}

/// A mouse report: SGR (`CSI < b ; x ; y M`, `m` on release) when the
/// program asked for it, else xterm's original bytes, which end at
/// column 223.
fn report(
    out: &mut Vec<u8>,
    mode: TermMode,
    mods: Modifiers,
    button: u8,
    pressed: bool,
    point: Point,
    offset: usize,
) {
    let modifiers = 4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.ctrl);
    let column = point.column.0 + 1;
    let line = (point.line.0 + offset as i32 + 1).max(1) as usize;
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        let b = button + modifiers;
        out.extend_from_slice(format!("\x1b[<{b};{column};{line}{end}").as_bytes());
        return;
    }
    // The original encoding knows no release of a particular button.
    let b = if pressed { button } else { 3 } + modifiers;
    if column > 223 || line > 223 {
        return;
    }
    out.extend_from_slice(&[0x1b, b'[', b'M', 32 + b, 32 + column as u8, 32 + line as u8]);
}
