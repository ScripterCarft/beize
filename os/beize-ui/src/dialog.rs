//! A dialog (VISION: one dialog style; ADR 0028 §3, ADR 0031): a question or a
//! name, its lines below, Cancel and the action side by side, in a window of
//! its own on menu glass. Return is the action, Escape cancels; a press and
//! release on a button is that button. Only a key's press counts, so the
//! Return that chose a menu's command, still held, does not answer. The
//! arrows and Tab pick a button, which shows an outline from then on: Return
//! and Space then answer with it. The first press leaves the action, so a
//! key held by accident cannot end in it.
//!
//! A dialog that asks for a password (ADR 0031) has a symbol above its title
//! – an image with the app's icon as a badge, macOS 26's – and a field
//! (`field`) between its lines and the buttons, which has the keyboard
//! first: typing goes there, Space too. Tab goes from the field to Cancel, to
//! the action and back to the field (macOS' keyboard navigation), the arrows
//! from button to button. A note below the field says what went wrong.
//!
//! The layout is macOS 26's alert, measured in the Human Interface Guidelines'
//! image of it (alert-macos@2x.png, 350 px across, in Beize's 325): text left
//! and all on one edge, the same space below the buttons as beside them. The
//! corners (26 px) and the text's closer lines are the owner's, on mock-ups
//! (31.10.); the buttons side by side, Cancel on the left, and the password
//! dialog's measures his too (15.11.). The text is the menus' (`theme`).

use std::cell::Cell;
use std::path::PathBuf;

use beize_draw::{AppIcon, Color, Family, Rect, Scene, Style, Text};
use zeroize::Zeroizing;

use crate::Appearance;
use crate::desktop::{Glass, GlassShape, Material};
use crate::field::{self, Edit, Field};
use crate::input::{Button, Input, Key, KeyState, Keysym, Pointer};
use crate::theme::{self, ACCENT, ON_ACCENT, SMALL_TEXT_SIZE, TEXT_SIZE};

/// The measures in Apple's image, at Beize's width of the panel.
const WIDTH: f32 = 325.0;
const TEXT_INSET: f32 = 28.0;
const BUTTON_INSET: f32 = 20.0;
const BUTTON_HEIGHT: f32 = 35.0;
const BUTTON_SPACE: f32 = 8.0;
/// Its top to the title, the title to its line and one line to the next, the
/// text to the buttons.
const TITLE_TOP: f32 = 26.0;
const TITLE_TO_LINE: f32 = 8.0;
const TEXT_TO_BUTTONS: f32 = 21.0;
const RADIUS: f32 = 26.0;
/// The symbol: the image 56 px, the badge 30 on its lower right, 10 beyond
/// it (about 0.55 of it, as macOS 26's), 12 above the title.
const IMAGE: u32 = 56;
const BADGE: u32 = 30;
const BADGE_BEYOND: f32 = 10.0;
const SYMBOL_TO_TITLE: f32 = 12.0;
/// The text to the field, the field to its note, the note's inset.
const TEXT_TO_FIELD: f32 = 16.0;
const FIELD_TO_NOTE: f32 = 8.0;
const NOTE_INSET: f32 = 4.0;
/// A note that says something went wrong: Apple's system red, light and dark.
const RED_LIGHT: Color = Color::rgb(0xff3b30);
const RED_DARK: Color = Color::rgb(0xff453a);
/// The outline of the button the keys picked, around it: the space between, its
/// width, in the accent at 90 % (the owner, on mock-ups, 31.10.).
const OUTLINE_SPACE: f32 = 2.0;
const OUTLINE_WIDTH: f32 = 2.0;
const OUTLINE: Color = theme::faded(ACCENT, 230);

/// Cancel's button: black 7.3 % on light (Apple's 215 on 232), white 8.3 % on
/// dark (45 on 26), both measured.
const NEUTRAL_LIGHT: Color = theme::faded(Color::rgb(0x000000), 19);
const NEUTRAL_DARK: Color = theme::faded(Color::rgb(0xffffff), 21);
/// How much of a held button shows; of one that waits (`set_busy`).
const HELD: u8 = 200;
const WAITING: u8 = 128;

/// What an input did to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// It looks different: draw again.
    Changed,
    Confirmed,
    Cancelled,
    /// Ctrl+V in its field: the app asks for the clipboard and hands the
    /// text to `paste`.
    Paste,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Cancel,
    Confirm,
}

/// The symbol above a dialog's title: an image, with a badge on its lower
/// right – the app's icon – if there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub image: PathBuf,
    pub badge: Option<Badge>,
}

/// An app's icon as the dock shows it: Beize's own apps' drawn
/// (`AppIcon`), others' from their file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Badge {
    Own(&'static AppIcon),
    File(PathBuf),
}

/// A line below the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    /// It says something went wrong: red.
    pub error: bool,
}

pub struct Dialog {
    symbol: Option<Symbol>,
    title: String,
    lines: Vec<String>,
    field: Option<Field>,
    note: Option<Note>,
    /// The note is about the answer before the one being checked: hidden
    /// while it is, so the verdict on this one shows anew (GNOME sets its
    /// message on each failure).
    note_stale: bool,
    action: String,
    /// It offers a choice: Cancel beside the action. An alert that only tells
    /// has its one button (`Dialog::info`).
    cancel: bool,
    /// It waits for its answer to be checked: the field takes nothing, the
    /// action does nothing.
    busy: bool,
    /// The window has the keyboard: the field shows its outline and caret.
    focused: bool,
    caps_lock: bool,
    pointer: Option<(f64, f64)>,
    held: Option<Choice>,
    /// The button the keys picked; none has an outline before they did, and
    /// the field has the keys while none is.
    picked: Option<Choice>,
    /// Where the field was drawn last, for a click on it.
    field_rect: Cell<Option<Rect>>,
}

impl Dialog {
    /// A question: Cancel and `action`.
    pub fn new(
        title: impl Into<String>,
        line: impl Into<String>,
        action: impl Into<String>,
    ) -> Dialog {
        Dialog {
            symbol: None,
            title: title.into(),
            lines: vec![line.into()],
            field: None,
            note: None,
            note_stale: false,
            action: action.into(),
            cancel: true,
            busy: false,
            focused: true,
            caps_lock: false,
            pointer: None,
            held: None,
            picked: None,
            field_rect: Cell::new(None),
        }
    }

    /// Something to read: only OK, as Apple's informational alerts.
    pub fn info(title: impl Into<String>, line: impl Into<String>) -> Dialog {
        Dialog {
            cancel: false,
            ..Dialog::new(title, line, "OK")
        }
    }

    /// With `symbol` above the title.
    pub fn with_symbol(mut self, symbol: Symbol) -> Dialog {
        self.symbol = Some(symbol);
        self
    }

    /// With another line below the ones it has.
    pub fn with_line(mut self, line: impl Into<String>) -> Dialog {
        self.lines.push(line.into());
        self
    }

    /// With `field` below its lines.
    pub fn with_field(mut self, field: Field) -> Dialog {
        self.field = Some(field);
        self
    }

    /// The note below the field; a new one changes its height.
    pub fn set_note(&mut self, note: Option<Note>) {
        self.note = note;
        self.note_stale = false;
    }

    /// Whether it waits for its answer to be checked. Waiting, the answer's
    /// dots stay dimmed in the field and the note before hides, in its
    /// place; once the verdict is in, the field is empty (macOS, GNOME, UAC
    /// empty it with the verdict, not before).
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        if busy {
            self.note_stale = self.note.is_some();
        } else if let Some(field) = &mut self.field {
            field.release();
        }
    }

    /// Caps Lock is on or off: true if that changes what it shows.
    pub fn set_caps_lock(&mut self, on: bool) -> bool {
        let changed = self.field.is_some() && self.caps_lock != on;
        self.caps_lock = on;
        changed
    }

    /// What PAM asks for, in its field (`Field::set_prompt`).
    pub fn set_prompt(&mut self, prompt: &str, echo: bool) {
        if let Some(field) = &mut self.field {
            field.set_prompt(prompt, echo);
        }
    }

    /// The field has the keys again, no button picked.
    pub fn focus_field(&mut self) {
        if self.field.is_some() {
            self.picked = None;
        }
    }

    /// What its field holds, which it no longer does.
    pub fn take_answer(&mut self) -> Option<Zeroizing<String>> {
        self.field.as_mut().map(Field::take)
    }

    /// The clipboard's text into its field.
    pub fn paste(&mut self, text: &str) -> Outcome {
        if self.busy {
            return Outcome::None;
        }
        match self.field.as_mut().map(|field| field.paste(text)) {
            Some(Edit::Changed) => Outcome::Changed,
            _ => Outcome::None,
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

    /// How wide its text may run.
    fn text_width() -> f32 {
        WIDTH - 2.0 * TEXT_INSET
    }

    fn field_width() -> f32 {
        WIDTH - 2.0 * BUTTON_INSET
    }

    /// Where its parts lie from the top, as its text wraps.
    fn layout(&self, text: &mut Text, ink: Color, red: Color) -> Layout {
        let room = Some(Self::text_width());
        let mut y = TITLE_TOP;
        let symbol = self.symbol.as_ref().map(|_| {
            let at = y;
            y += IMAGE as f32 + BADGE_BEYOND + SYMBOL_TO_TITLE;
            at
        });
        let title = text.layout(&self.title, &Self::style(TEXT_SIZE, 700.0, ink), room);
        let title_y = y;
        y += title.height();
        let mut lines = Vec::new();
        for line in &self.lines {
            y += TITLE_TO_LINE;
            let layout = text.layout(line, &Self::style(TEXT_SIZE, 400.0, ink), room);
            lines.push((layout, y.round()));
            y += lines.last().map_or(0.0, |(l, _)| l.height());
        }
        let field = self.field.as_ref().map(|_| {
            y += TEXT_TO_FIELD;
            let rect = Rect {
                x: BUTTON_INSET,
                y: y.round(),
                w: Self::field_width(),
                h: field::HEIGHT,
            };
            y += field::HEIGHT;
            rect
        });
        let note = match (&self.note, field) {
            (Some(note), Some(_)) => {
                y += FIELD_TO_NOTE;
                let color = if note.error { red } else { ink };
                let room = Some(Self::field_width() - 2.0 * NOTE_INSET);
                let layout = text.layout(
                    &note.text,
                    &Self::style(SMALL_TEXT_SIZE, 400.0, color),
                    room,
                );
                let at = y.round();
                y += layout.height();
                Some((layout, at))
            }
            _ => None,
        };
        y += TEXT_TO_BUTTONS + BUTTON_HEIGHT + BUTTON_INSET;
        Layout {
            symbol,
            title: (title, title_y.round()),
            lines,
            field,
            note,
            height: y.ceil(),
        }
    }

    pub fn size(&self, text: &mut Text) -> (u32, u32) {
        let layout = self.layout(text, theme::INK_LIGHT, RED_LIGHT);
        (WIDTH as u32, layout.height as u32)
    }

    /// The glass below a dialog of `size`, as below a menu.
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

    /// Where the buttons lie in a dialog of `(w, h)`, along its bottom: Cancel,
    /// if it has one, on the left, the action on the right, each half the
    /// width on whole pixels; an alert's one button across it.
    fn buttons(&self, (w, h): (f32, f32)) -> (Option<Rect>, Rect) {
        let y = h - BUTTON_INSET - BUTTON_HEIGHT;
        let across = w - 2.0 * BUTTON_INSET;
        if !self.cancel {
            let button = Rect {
                x: BUTTON_INSET,
                y,
                w: across,
                h: BUTTON_HEIGHT,
            };
            return (None, button);
        }
        let half = ((across - BUTTON_SPACE) / 2.0).floor();
        let cancel = Rect {
            x: BUTTON_INSET,
            y,
            w: half,
            h: BUTTON_HEIGHT,
        };
        let x = BUTTON_INSET + half + BUTTON_SPACE;
        let confirm = Rect {
            x,
            y,
            w: w - BUTTON_INSET - x,
            h: BUTTON_HEIGHT,
        };
        (Some(cancel), confirm)
    }

    fn pointer_in(&self, r: Rect) -> bool {
        self.pointer.is_some_and(|(x, y)| {
            let (x, y) = (x as f32, y as f32);
            x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
        })
    }

    fn choice_at(&self, size: (f32, f32)) -> Option<Choice> {
        let (cancel, confirm) = self.buttons(size);
        if cancel.is_some_and(|r| self.pointer_in(r)) {
            Some(Choice::Cancel)
        } else if self.pointer_in(confirm) {
            Some(Choice::Confirm)
        } else {
            None
        }
    }

    /// The button an arrow goes to: the other, or the only one.
    fn next(&self) -> Choice {
        match (self.cancel, self.picked) {
            (false, _) | (true, Some(Choice::Cancel)) => Choice::Confirm,
            (true, _) => Choice::Cancel,
        }
    }

    /// Where Tab goes: with a field from it to Cancel, the action and back,
    /// backwards with Shift; without one as an arrow.
    fn tab(&self, back: bool) -> Option<Choice> {
        if self.field.is_none() {
            return Some(self.next());
        }
        let order = if self.cancel {
            &[None, Some(Choice::Cancel), Some(Choice::Confirm)][..]
        } else {
            &[None, Some(Choice::Confirm)][..]
        };
        let at = order.iter().position(|c| *c == self.picked).unwrap_or(0);
        let n = order.len();
        order[if back { (at + n - 1) % n } else { (at + 1) % n }]
    }

    fn answer(&self, choice: Choice) -> Outcome {
        match choice {
            Choice::Cancel => Outcome::Cancelled,
            Choice::Confirm if self.busy => Outcome::None,
            Choice::Confirm => Outcome::Confirmed,
        }
    }

    /// The field has the keys: it has one and no button is picked.
    fn typing(&self) -> bool {
        self.field.is_some() && self.picked.is_none()
    }

    fn key(&mut self, key: &Key) -> Outcome {
        let pressed = key.state == KeyState::Pressed;
        match key.sym {
            Keysym::Escape if pressed => Outcome::Cancelled,
            Keysym::Return | Keysym::KP_Enter if pressed => {
                self.answer(self.picked.unwrap_or(Choice::Confirm))
            }
            Keysym::Tab | Keysym::ISO_Left_Tab if pressed => {
                let back = key.sym == Keysym::ISO_Left_Tab || key.mods.shift;
                self.picked = self.tab(back);
                Outcome::Changed
            }
            Keysym::Up | Keysym::Down | Keysym::Left | Keysym::Right
                if pressed && !self.typing() =>
            {
                self.picked = Some(self.next());
                Outcome::Changed
            }
            Keysym::space if pressed && !self.typing() => {
                self.picked.map_or(Outcome::None, |c| self.answer(c))
            }
            _ if self.typing() && !self.busy => match self.field.as_mut().map(|f| f.key(key)) {
                Some(Edit::Changed) => Outcome::Changed,
                Some(Edit::Paste) => Outcome::Paste,
                _ => Outcome::None,
            },
            _ => Outcome::None,
        }
    }

    /// What `input`, to the dialog of `size`, does.
    pub fn input(&mut self, input: &Input, (w, h): (u32, u32)) -> Outcome {
        let size = (w as f32, h as f32);
        match input {
            Input::Key(key) if key.state != KeyState::Released => self.key(key),
            Input::Focus(focused) => {
                self.focused = *focused;
                if self.field.is_some() {
                    Outcome::Changed
                } else {
                    Outcome::None
                }
            }
            Input::Pointer(Pointer::Enter(x, y) | Pointer::Motion(x, y)) => {
                self.pointer = Some((*x, *y));
                Outcome::None
            }
            Input::Pointer(Pointer::Leave) => {
                self.pointer = None;
                if self.held.take().is_some() {
                    Outcome::Changed
                } else {
                    Outcome::None
                }
            }
            Input::Pointer(Pointer::Button {
                button: Button::Left,
                pressed: true,
            }) => {
                self.held = self.choice_at(size);
                if self.held.is_some() {
                    return Outcome::Changed;
                }
                // A click on the field gives it the keys back.
                let on_field = self.field_rect.get().is_some_and(|r| self.pointer_in(r));
                if on_field && self.picked.take().is_some() {
                    Outcome::Changed
                } else {
                    Outcome::None
                }
            }
            Input::Pointer(Pointer::Button {
                button: Button::Left,
                pressed: false,
            }) => {
                let held = self.held.take();
                match (held, self.choice_at(size)) {
                    (Some(a), Some(b)) if a == b => match self.answer(a) {
                        Outcome::None => Outcome::Changed,
                        answer => answer,
                    },
                    (Some(_), _) => Outcome::Changed,
                    (None, _) => Outcome::None,
                }
            }
            _ => Outcome::None,
        }
    }

    /// Its content in `(w, h)` for `appearance`; the rest shows the glass.
    pub fn draw(&self, text: &mut Text, (w, h): (u32, u32), appearance: Appearance) -> Scene {
        let (w, h) = (w as f32, h as f32);
        let mut scene = Scene::new();
        scene.clear(Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        let ink = theme::ink(appearance);
        let (neutral, red) = match appearance {
            Appearance::Light => (NEUTRAL_LIGHT, RED_LIGHT),
            Appearance::Dark => (NEUTRAL_DARK, RED_DARK),
        };
        let layout = self.layout(text, ink, red);
        if let (Some(symbol), Some(y)) = (&self.symbol, layout.symbol) {
            scene.image(&symbol.image, TEXT_INSET, y, IMAGE);
            let at = IMAGE as f32 + BADGE_BEYOND - BADGE as f32;
            match &symbol.badge {
                Some(Badge::Own(icon)) => icon.draw(
                    &mut scene,
                    Rect {
                        x: TEXT_INSET + at,
                        y: y + at,
                        w: BADGE as f32,
                        h: BADGE as f32,
                    },
                ),
                Some(Badge::File(file)) => scene.image(file, TEXT_INSET + at, y + at, BADGE),
                None => {}
            }
        }
        let (title, title_y) = &layout.title;
        scene.text(title, TEXT_INSET, *title_y);
        for (line, y) in &layout.lines {
            scene.text(line, TEXT_INSET, *y);
        }
        self.field_rect.set(layout.field);
        if let (Some(field), Some(rect)) = (&self.field, layout.field) {
            let focused = self.focused && self.picked.is_none();
            let waiting = self.busy.then_some(WAITING);
            field.draw(
                &mut scene,
                text,
                rect,
                appearance,
                focused,
                self.caps_lock,
                waiting,
            );
        }
        if let (Some((note, y)), Some(rect)) = (&layout.note, layout.field)
            && !self.note_stale
        {
            scene.text(note, rect.x + NOTE_INSET, *y);
        }

        let (cancel, confirm) = self.buttons((w, h));
        let shown = |color: Color, choice: Choice| {
            let fade = |by: u8| Color {
                a: (u32::from(color.a) * u32::from(by) / 255) as u8,
                ..color
            };
            if self.busy && choice == Choice::Confirm {
                fade(WAITING)
            } else if self.held == Some(choice) {
                fade(HELD)
            } else {
                color
            }
        };
        let mut button = |rect: Rect, fill: Color, label: &str, color: Color, text: &mut Text| {
            scene.rounded(rect, rect.h / 2.0, fill);
            let layout = text.layout(label, &Self::style(TEXT_SIZE, 500.0, color), None);
            scene.text(
                &layout,
                (rect.x + (rect.w - layout.width()) / 2.0).round(),
                (rect.y + (rect.h - layout.height()) / 2.0).round(),
            );
        };
        if let Some(cancel) = cancel {
            button(cancel, shown(neutral, Choice::Cancel), "Cancel", ink, text);
        }
        button(
            confirm,
            shown(ACCENT, Choice::Confirm),
            &self.action,
            ON_ACCENT,
            text,
        );
        let picked = match self.picked {
            Some(Choice::Confirm) => Some(confirm),
            Some(Choice::Cancel) => cancel,
            None => None,
        };
        if let Some(rect) = picked {
            let out = OUTLINE_SPACE + OUTLINE_WIDTH;
            let around = Rect {
                x: rect.x - out,
                y: rect.y - out,
                w: rect.w + 2.0 * out,
                h: rect.h + 2.0 * out,
            };
            scene.ring(around, rect.h / 2.0 + out, OUTLINE_WIDTH, OUTLINE);
        }
        scene
    }
}

/// Where a dialog's parts lie: the symbol's top, the title and each line
/// with theirs, the field, the note with its top, the whole height.
struct Layout {
    symbol: Option<f32>,
    title: (beize_draw::text::Layout<Color>, f32),
    lines: Vec<(beize_draw::text::Layout<Color>, f32)>,
    field: Option<Rect>,
    note: Option<(beize_draw::text::Layout<Color>, f32)>,
    height: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;

    const SIZE: (u32, u32) = (325, 148);

    fn size() -> (f32, f32) {
        (SIZE.0 as f32, SIZE.1 as f32)
    }

    fn at(dialog: &mut Dialog, (x, y): (f32, f32)) {
        dialog.input(
            &Input::Pointer(Pointer::Motion(f64::from(x), f64::from(y))),
            SIZE,
        );
    }

    fn press(dialog: &mut Dialog, pressed: bool) -> Outcome {
        dialog.input(
            &Input::Pointer(Pointer::Button {
                button: Button::Left,
                pressed,
            }),
            SIZE,
        )
    }

    fn middle(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn a_press_and_release_on_a_button_is_that_button() {
        let mut dialog = Dialog::new("t", "l", "a");
        let (cancel, confirm) = dialog.buttons(size());
        at(&mut dialog, middle(cancel.unwrap()));
        assert_eq!(press(&mut dialog, true), Outcome::Changed);
        assert_eq!(press(&mut dialog, false), Outcome::Cancelled);
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        assert_eq!(press(&mut dialog, false), Outcome::Confirmed);
    }

    #[test]
    fn moved_off_a_button_a_release_chooses_nothing() {
        let mut dialog = Dialog::new("t", "l", "a");
        let (cancel, confirm) = dialog.buttons(size());
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        at(&mut dialog, middle(cancel.unwrap()));
        assert_eq!(press(&mut dialog, false), Outcome::Changed);
        // A press beside the buttons chooses nothing either.
        at(&mut dialog, (2.0, 2.0));
        assert_eq!(press(&mut dialog, true), Outcome::None);
        assert_eq!(press(&mut dialog, false), Outcome::None);
    }

    #[test]
    fn the_buttons_lie_side_by_side_with_cancel_on_the_left() {
        let dialog = Dialog::new("t", "l", "a");
        let (cancel, confirm) = dialog.buttons(size());
        let cancel = cancel.unwrap();
        assert_eq!(cancel.y, confirm.y);
        assert_eq!(cancel.x + cancel.w + BUTTON_SPACE, confirm.x);
        // Half each, on whole pixels; the same space below them as beside them.
        assert!((cancel.w - confirm.w).abs() <= 1.0);
        assert_eq!(cancel.w.fract(), 0.0);
        assert_eq!(size().1 - (cancel.y + cancel.h), cancel.x);
        assert_eq!(size().0 - (confirm.x + confirm.w), cancel.x);
    }

    fn key_with(dialog: &mut Dialog, sym: Keysym, text: Option<&str>, shift: bool) -> Outcome {
        dialog.input(
            &Input::Key(Key {
                sym,
                text: text.map(str::to_owned),
                mods: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                state: KeyState::Pressed,
            }),
            SIZE,
        )
    }

    fn key(dialog: &mut Dialog, sym: Keysym) -> Outcome {
        key_with(dialog, sym, None, false)
    }

    #[test]
    fn return_is_the_action_until_the_keys_pick_another() {
        let mut dialog = Dialog::new("t", "l", "a");
        assert_eq!(key(&mut dialog, Keysym::space), Outcome::None);
        assert_eq!(key(&mut dialog, Keysym::Down), Outcome::Changed);
        assert_eq!(key(&mut dialog, Keysym::Return), Outcome::Cancelled);
        let mut dialog = Dialog::new("t", "l", "a");
        assert_eq!(key(&mut dialog, Keysym::Return), Outcome::Confirmed);
    }

    #[test]
    fn the_arrows_go_from_one_button_to_the_other() {
        let mut dialog = Dialog::new("t", "l", "a");
        key(&mut dialog, Keysym::Left);
        assert_eq!(dialog.picked, Some(Choice::Cancel));
        key(&mut dialog, Keysym::Tab);
        assert_eq!(dialog.picked, Some(Choice::Confirm));
        assert_eq!(key(&mut dialog, Keysym::space), Outcome::Confirmed);
        // Escape stays Cancel, whatever is picked.
        assert_eq!(key(&mut dialog, Keysym::Escape), Outcome::Cancelled);
    }

    #[test]
    fn an_info_dialogs_one_button_gets_the_outline() {
        let mut dialog = Dialog::info("t", "l");
        assert_eq!(key(&mut dialog, Keysym::Down), Outcome::Changed);
        assert_eq!(dialog.picked, Some(Choice::Confirm));
        assert_eq!(key(&mut dialog, Keysym::Return), Outcome::Confirmed);
    }

    #[test]
    fn an_info_dialog_has_its_one_button_across() {
        let mut dialog = Dialog::info("t", "l");
        let (cancel, confirm) = dialog.buttons(size());
        assert!(cancel.is_none());
        assert_eq!(confirm.w, size().0 - 2.0 * BUTTON_INSET);
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        assert_eq!(press(&mut dialog, false), Outcome::Confirmed);
    }

    fn password() -> Dialog {
        Dialog::new("t", "l", "Allow").with_field(Field::password())
    }

    #[test]
    fn the_field_has_the_keys_space_and_arrows_too() {
        let mut dialog = password();
        assert_eq!(
            key_with(&mut dialog, Keysym::a, Some("a"), false),
            Outcome::Changed
        );
        assert_eq!(
            key_with(&mut dialog, Keysym::space, Some(" "), false),
            Outcome::Changed
        );
        assert_eq!(key(&mut dialog, Keysym::Left), Outcome::None);
        assert_eq!(dialog.picked, None);
        assert_eq!(key(&mut dialog, Keysym::Return), Outcome::Confirmed);
        assert_eq!(dialog.take_answer().unwrap().as_str(), "a ");
    }

    #[test]
    fn tab_goes_from_the_field_to_the_buttons_and_back() {
        let mut dialog = password();
        key(&mut dialog, Keysym::Tab);
        assert_eq!(dialog.picked, Some(Choice::Cancel));
        // A button picked: typing goes nowhere, the arrows go along.
        assert_eq!(
            key_with(&mut dialog, Keysym::a, Some("a"), false),
            Outcome::None
        );
        key(&mut dialog, Keysym::Right);
        assert_eq!(dialog.picked, Some(Choice::Confirm));
        key(&mut dialog, Keysym::Tab);
        assert_eq!(dialog.picked, None);
        key_with(&mut dialog, Keysym::ISO_Left_Tab, None, true);
        assert_eq!(dialog.picked, Some(Choice::Confirm));
        assert!(dialog.take_answer().unwrap().is_empty());
    }

    #[test]
    fn while_busy_the_field_takes_nothing_and_allow_waits() {
        let mut dialog = password();
        dialog.set_busy(true);
        assert_eq!(
            key_with(&mut dialog, Keysym::a, Some("a"), false),
            Outcome::None
        );
        assert_eq!(dialog.paste("x"), Outcome::None);
        assert_eq!(key(&mut dialog, Keysym::Return), Outcome::None);
        assert_eq!(key(&mut dialog, Keysym::Escape), Outcome::Cancelled);
        dialog.set_busy(false);
        assert_eq!(dialog.paste("x"), Outcome::Changed);
    }

    #[test]
    fn the_last_note_hides_while_the_next_answer_is_checked() {
        let mut dialog = password();
        let wrong = || {
            Some(Note {
                text: "wrong".into(),
                error: true,
            })
        };
        dialog.set_note(wrong());
        dialog.set_busy(true);
        assert!(dialog.note_stale);
        // The verdict: the same words, shown anew.
        dialog.set_note(wrong());
        dialog.set_busy(false);
        assert!(!dialog.note_stale && dialog.note.is_some());
    }

    #[test]
    fn ctrl_v_in_the_field_asks_for_the_clipboard() {
        let mut dialog = password();
        let outcome = dialog.input(
            &Input::Key(Key {
                sym: Keysym::v,
                text: Some("\u{16}".into()),
                mods: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
                state: KeyState::Pressed,
            }),
            SIZE,
        );
        assert_eq!(outcome, Outcome::Paste);
    }

    #[test]
    fn caps_lock_matters_only_with_a_field() {
        let mut plain = Dialog::new("t", "l", "a");
        assert!(!plain.set_caps_lock(true));
        let mut dialog = password();
        assert!(dialog.set_caps_lock(true));
        assert!(!dialog.set_caps_lock(true));
    }
}
