//! A dialog (VISION: one dialog style; ADR 0028 §3): a question or a name, its
//! line below, the action over Cancel, in a window of its own on menu glass.
//! Return is the action, Escape cancels; a press and release on a button is
//! that button. Only a key's press counts, so the Return that chose a menu's
//! command, still held, does not answer. The arrows and Tab pick a button, which
//! shows an outline from then on: Return and Space then answer with it. The
//! first press leaves the action, so a key held by accident cannot end in it.
//!
//! The layout is macOS 26's alert, measured in the Human Interface Guidelines'
//! image of it (alert-macos@2x.png, 350 px across, in Beize's 325): text left
//! and all on one edge, the buttons stacked across the panel on another, the
//! default on top, the same space below them as beside them. The corners
//! (26 px) and the text's closer lines are the owner's, on mock-ups (31.10.).
//! The text is the menus' (`theme`).

use beize_draw::{Color, Family, Rect, Scene, Style, Text};

use crate::Appearance;
use crate::desktop::{Glass, GlassShape, Material};
use crate::input::{Button, Input, Key, KeyState, Keysym, Pointer};
use crate::theme::{self, ACCENT, ON_ACCENT, TEXT_SIZE};

/// The measures in Apple's image, at Beize's width of the panel.
const WIDTH: f32 = 325.0;
const TEXT_INSET: f32 = 28.0;
const BUTTON_INSET: f32 = 20.0;
const BUTTON_HEIGHT: f32 = 35.0;
const BUTTON_SPACE: f32 = 8.0;
/// Its top to the title, the title to its line, the text to the first button.
const TITLE_TOP: f32 = 26.0;
const TITLE_TO_LINE: f32 = 8.0;
const TEXT_TO_BUTTONS: f32 = 21.0;
const RADIUS: f32 = 26.0;
/// The outline of the button the keys picked, around it: the space between, its
/// width, in the accent at 90 % (the owner, on mock-ups, 31.10.).
const OUTLINE_SPACE: f32 = 2.0;
const OUTLINE_WIDTH: f32 = 2.0;
const OUTLINE: Color = theme::faded(ACCENT, 230);

/// Cancel's button: black 7.3 % on light (Apple's 215 on 232), white 8.3 % on
/// dark (45 on 26), both measured.
const NEUTRAL_LIGHT: Color = theme::faded(Color::rgb(0x000000), 19);
const NEUTRAL_DARK: Color = theme::faded(Color::rgb(0xffffff), 21);
/// How much of a held button shows.
const HELD: u8 = 200;

/// What an input did to the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// A button looks different: draw again.
    Changed,
    Confirmed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Cancel,
    Confirm,
}

pub struct Dialog {
    title: String,
    line: String,
    action: String,
    /// It offers a choice: a Cancel below the action. An alert that only tells
    /// has its one button (`Dialog::info`).
    cancel: bool,
    pointer: Option<(f64, f64)>,
    held: Option<Choice>,
    /// The button the keys picked; none has an outline before they did.
    picked: Option<Choice>,
}

impl Dialog {
    /// A question: `action` over Cancel.
    pub fn new(
        title: impl Into<String>,
        line: impl Into<String>,
        action: impl Into<String>,
    ) -> Dialog {
        Dialog {
            title: title.into(),
            line: line.into(),
            action: action.into(),
            cancel: true,
            pointer: None,
            held: None,
            picked: None,
        }
    }

    /// Something to read: only OK, as Apple's informational alerts.
    pub fn info(title: impl Into<String>, line: impl Into<String>) -> Dialog {
        Dialog {
            cancel: false,
            ..Dialog::new(title, line, "OK")
        }
    }

    fn style(weight: f32, color: Color) -> Style {
        Style {
            family: Family::Sans,
            size: TEXT_SIZE,
            weight,
            color,
        }
    }

    /// How wide its text may run.
    fn text_width() -> f32 {
        WIDTH - 2.0 * TEXT_INSET
    }

    /// Its height: as its text wraps, the buttons below.
    fn height(&self, text: &mut Text) -> f32 {
        let room = Some(Self::text_width());
        let title = text.layout(&self.title, &Self::style(700.0, theme::INK_DARK), room);
        let line = text.layout(&self.line, &Self::style(400.0, theme::INK_DARK), room);
        let buttons = if self.cancel {
            2.0 * BUTTON_HEIGHT + BUTTON_SPACE
        } else {
            BUTTON_HEIGHT
        };
        (TITLE_TOP
            + title.height()
            + TITLE_TO_LINE
            + line.height()
            + TEXT_TO_BUTTONS
            + buttons
            + BUTTON_INSET)
            .ceil()
    }

    pub fn size(&self, text: &mut Text) -> (u32, u32) {
        (WIDTH as u32, self.height(text) as u32)
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

    /// Where the buttons lie in a dialog of `(w, h)`: Cancel, if it has one, at
    /// the bottom, the action above it.
    fn buttons(&self, (w, h): (f32, f32)) -> (Option<Rect>, Rect) {
        let button = |y: f32| Rect {
            x: BUTTON_INSET,
            y,
            w: w - 2.0 * BUTTON_INSET,
            h: BUTTON_HEIGHT,
        };
        let last = h - BUTTON_INSET - BUTTON_HEIGHT;
        if self.cancel {
            (
                Some(button(last)),
                button(last - BUTTON_SPACE - BUTTON_HEIGHT),
            )
        } else {
            (None, button(last))
        }
    }

    fn choice_at(&self, size: (f32, f32)) -> Option<Choice> {
        let (x, y) = self.pointer?;
        let (x, y) = (x as f32, y as f32);
        let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        let (cancel, confirm) = self.buttons(size);
        if cancel.is_some_and(inside) {
            Some(Choice::Cancel)
        } else if inside(confirm) {
            Some(Choice::Confirm)
        } else {
            None
        }
    }

    /// The button an arrow or Tab goes to: the other, or the only one.
    fn next(&self) -> Choice {
        match (self.cancel, self.picked) {
            (false, _) | (true, Some(Choice::Cancel)) => Choice::Confirm,
            (true, _) => Choice::Cancel,
        }
    }

    fn answer(choice: Choice) -> Outcome {
        match choice {
            Choice::Cancel => Outcome::Cancelled,
            Choice::Confirm => Outcome::Confirmed,
        }
    }

    /// What `input`, to the dialog of `size`, does.
    pub fn input(&mut self, input: &Input, (w, h): (u32, u32)) -> Outcome {
        let size = (w as f32, h as f32);
        match input {
            Input::Key(Key {
                sym,
                state: KeyState::Pressed,
                ..
            }) => match *sym {
                Keysym::Return | Keysym::KP_Enter => {
                    Self::answer(self.picked.unwrap_or(Choice::Confirm))
                }
                Keysym::space => self.picked.map_or(Outcome::None, Self::answer),
                Keysym::Escape => Outcome::Cancelled,
                Keysym::Up
                | Keysym::Down
                | Keysym::Left
                | Keysym::Right
                | Keysym::Tab
                | Keysym::ISO_Left_Tab => {
                    self.picked = Some(self.next());
                    Outcome::Changed
                }
                _ => Outcome::None,
            },
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
                    (Some(Choice::Cancel), Some(Choice::Cancel)) => Outcome::Cancelled,
                    (Some(Choice::Confirm), Some(Choice::Confirm)) => Outcome::Confirmed,
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
        let neutral = match appearance {
            Appearance::Light => NEUTRAL_LIGHT,
            Appearance::Dark => NEUTRAL_DARK,
        };
        let room = Some(Self::text_width());
        let title = text.layout(&self.title, &Self::style(700.0, ink), room);
        scene.text(&title, TEXT_INSET, TITLE_TOP);
        let line = text.layout(&self.line, &Self::style(400.0, ink), room);
        let line_top = (TITLE_TOP + title.height() + TITLE_TO_LINE).round();
        scene.text(&line, TEXT_INSET, line_top);

        let (cancel, confirm) = self.buttons((w, h));
        let shown = |color: Color, choice: Choice| {
            if self.held == Some(choice) {
                Color {
                    a: (u32::from(color.a) * u32::from(HELD) / 255) as u8,
                    ..color
                }
            } else {
                color
            }
        };
        let mut button = |rect: Rect, fill: Color, label: &str, color: Color, text: &mut Text| {
            scene.rounded(rect, rect.h / 2.0, fill);
            let layout = text.layout(label, &Self::style(500.0, color), None);
            scene.text(
                &layout,
                (rect.x + (rect.w - layout.width()) / 2.0).round(),
                (rect.y + (rect.h - layout.height()) / 2.0).round(),
            );
        };
        button(
            confirm,
            shown(ACCENT, Choice::Confirm),
            &self.action,
            ON_ACCENT,
            text,
        );
        if let Some(cancel) = cancel {
            button(cancel, shown(neutral, Choice::Cancel), "Cancel", ink, text);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (325, 197);

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
    fn the_buttons_are_stacked_on_one_edge_with_the_action_on_top() {
        let dialog = Dialog::new("t", "l", "a");
        let (cancel, confirm) = dialog.buttons(size());
        let cancel = cancel.unwrap();
        assert_eq!((confirm.x, confirm.w), (cancel.x, cancel.w));
        assert_eq!(confirm.y + confirm.h + BUTTON_SPACE, cancel.y);
        // The same space below them as beside them.
        assert_eq!(size().1 - (cancel.y + cancel.h), cancel.x);
        assert_eq!(size().0 - (cancel.x + cancel.w), cancel.x);
    }

    fn key(dialog: &mut Dialog, sym: Keysym) -> Outcome {
        dialog.input(
            &Input::Key(Key {
                sym,
                text: None,
                mods: crate::input::Modifiers::default(),
                state: KeyState::Pressed,
            }),
            SIZE,
        )
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
        key(&mut dialog, Keysym::Up);
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
    fn an_info_dialog_has_its_one_button() {
        let mut dialog = Dialog::info("t", "l");
        let (cancel, confirm) = dialog.buttons(size());
        assert!(cancel.is_none());
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        assert_eq!(press(&mut dialog, false), Outcome::Confirmed);
    }
}
