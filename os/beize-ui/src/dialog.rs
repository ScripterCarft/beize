//! A confirmation (VISION: one dialog style; ADR 0028 §3): a question, a line
//! below it, Cancel and the action, in a window of its own on menu glass, as
//! macOS' alerts. Return is the action, Escape cancels; a press and release
//! on a button is that button. Only a key's press counts, so the Return that
//! chose a menu's command, still held, does not answer.
//!
//! The measures are estimated from macOS 26's alerts at Beize's scale.

use beize_draw::measures::SCALE;
use beize_draw::{Color, Family, Rect, Scene, Style, Text};

use crate::Appearance;
use crate::desktop::{Glass, GlassShape, Material};
use crate::input::{Button, Input, Key, KeyState, Keysym, Pointer};

const WIDTH: f32 = 260.0;
const PADDING: f32 = 20.0;
const LINE_GAP: f32 = 6.0;
const BUTTONS_GAP: f32 = 16.0;
const BUTTON_HEIGHT: f32 = 28.0;
const BUTTON_SPACE: f32 = 8.0;
const RADIUS: f32 = 26.0;
const TITLE_SIZE: f32 = 15.0;
const LINE_SIZE: f32 = 13.0;

const INK_LIGHT: Color = Color::rgb(0x292929);
const INK_DARK: Color = Color::rgb(0xe8e8e8);
const ACCENT: Color = Color::rgb(0x007aff);
const ON_ACCENT: Color = Color::rgb(0xffffff);
/// Cancel's button: black or white, faintly over the glass.
const NEUTRAL_LIGHT: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 20,
};
const NEUTRAL_DARK: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 36,
};
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
    pointer: Option<(f64, f64)>,
    held: Option<Choice>,
}

impl Dialog {
    pub fn new(
        title: impl Into<String>,
        line: impl Into<String>,
        action: impl Into<String>,
    ) -> Dialog {
        Dialog {
            title: title.into(),
            line: line.into(),
            action: action.into(),
            pointer: None,
            held: None,
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

    /// Its width: the measure, wider for text that needs it.
    fn width(&self, text: &mut Text) -> f32 {
        let title = text
            .layout(&self.title, &Self::style(TITLE_SIZE, 700.0, INK_DARK), None)
            .width();
        let line = text
            .layout(&self.line, &Self::style(LINE_SIZE, 400.0, INK_DARK), None)
            .width();
        (title.max(line) + 2.0 * PADDING * SCALE)
            .max(WIDTH * SCALE)
            .ceil()
    }

    /// Its height: the same for any text of one line each.
    fn height(text: &mut Text) -> f32 {
        let title = text
            .layout("X", &Self::style(TITLE_SIZE, 700.0, INK_DARK), None)
            .height();
        let line = text
            .layout("X", &Self::style(LINE_SIZE, 400.0, INK_DARK), None)
            .height();
        (2.0 * PADDING * SCALE
            + title
            + LINE_GAP * SCALE
            + line
            + BUTTONS_GAP * SCALE
            + BUTTON_HEIGHT * SCALE)
            .ceil()
    }

    pub fn size(&self, text: &mut Text) -> (u32, u32) {
        (self.width(text) as u32, Self::height(text) as u32)
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

    /// Where the buttons lie in a dialog of `(w, h)`: Cancel, then the action
    /// on the right, as macOS has it.
    fn buttons((w, h): (f32, f32)) -> (Rect, Rect) {
        let pad = (PADDING * SCALE).round();
        let space = (BUTTON_SPACE * SCALE).round();
        let height = (BUTTON_HEIGHT * SCALE).round();
        let width = ((w - 2.0 * pad - space) / 2.0).floor();
        let y = h - pad - height;
        (
            Rect {
                x: pad,
                y,
                w: width,
                h: height,
            },
            Rect {
                x: w - pad - width,
                y,
                w: width,
                h: height,
            },
        )
    }

    fn choice_at(&self, size: (f32, f32)) -> Option<Choice> {
        let (x, y) = self.pointer?;
        let (x, y) = (x as f32, y as f32);
        let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        let (cancel, confirm) = Self::buttons(size);
        if inside(cancel) {
            Some(Choice::Cancel)
        } else if inside(confirm) {
            Some(Choice::Confirm)
        } else {
            None
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
                Keysym::Return | Keysym::KP_Enter => Outcome::Confirmed,
                Keysym::Escape => Outcome::Cancelled,
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
        let (ink, neutral) = match appearance {
            Appearance::Light => (INK_LIGHT, NEUTRAL_LIGHT),
            Appearance::Dark => (INK_DARK, NEUTRAL_DARK),
        };
        let centred = |width: f32| ((w - width) / 2.0).round();
        let mut y = (PADDING * SCALE).round();
        let title = text.layout(&self.title, &Self::style(TITLE_SIZE, 700.0, ink), None);
        scene.text(&title, centred(title.width()), y);
        y += title.height() + (LINE_GAP * SCALE).round();
        let line = text.layout(&self.line, &Self::style(LINE_SIZE, 400.0, ink), None);
        scene.text(&line, centred(line.width()), y);

        let (cancel, confirm) = Self::buttons((w, h));
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
            let layout = text.layout(label, &Self::style(TITLE_SIZE, 500.0, color), None);
            scene.text(
                &layout,
                (rect.x + (rect.w - layout.width()) / 2.0).round(),
                (rect.y + (rect.h - layout.height()) / 2.0).round(),
            );
        };
        button(cancel, shown(neutral, Choice::Cancel), "Cancel", ink, text);
        button(
            confirm,
            shown(ACCENT, Choice::Confirm),
            &self.action,
            ON_ACCENT,
            text,
        );
        scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (325, 150);

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
        let (cancel, confirm) = Dialog::buttons((SIZE.0 as f32, SIZE.1 as f32));
        let mut dialog = Dialog::new("t", "l", "a");
        at(&mut dialog, middle(cancel));
        assert_eq!(press(&mut dialog, true), Outcome::Changed);
        assert_eq!(press(&mut dialog, false), Outcome::Cancelled);
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        assert_eq!(press(&mut dialog, false), Outcome::Confirmed);
    }

    #[test]
    fn moved_off_a_button_a_release_chooses_nothing() {
        let (cancel, confirm) = Dialog::buttons((SIZE.0 as f32, SIZE.1 as f32));
        let mut dialog = Dialog::new("t", "l", "a");
        at(&mut dialog, middle(confirm));
        press(&mut dialog, true);
        at(&mut dialog, middle(cancel));
        assert_eq!(press(&mut dialog, false), Outcome::Changed);
        // A press beside the buttons chooses nothing either.
        at(&mut dialog, (2.0, 2.0));
        assert_eq!(press(&mut dialog, true), Outcome::None);
        assert_eq!(press(&mut dialog, false), Outcome::None);
    }
}
