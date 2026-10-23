//! The title bar Beize UI draws on top of every window (ADR 0028 §2, as
//! AppKit draws it): the title centred, as on macOS and GNOME; the buttons
//! on the right, as on Windows 11 – 46 × 32 each, the close button red
//! under the pointer – with Phosphor's symbols (§8). Dragged, the bar moves
//! the window; a double click maximizes it and back.
//!
//! Minimizing comes with the dock, which brings a window back (ADR 0028 c):
//! until then there is no button for it, as nothing is shown that does
//! not work.

use std::time::{Duration, Instant};

use crate::draw::{Color, Rect, Scene};
use crate::text::{Family, Style, Text};

/// The bar's height, as Windows 11's caption buttons.
pub const HEIGHT: u32 = 32;
const BUTTON_WIDTH: f32 = 46.0;
/// Phosphor's 1/16 stroke is one pixel at this size.
const SYMBOL: u32 = 16;
const TITLE_SIZE: f32 = 13.0;
const TITLE_WEIGHT: f32 = 600.0;
/// Two presses at most this far apart are a double click (GNOME's
/// default), and at most this far moved.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_DISTANCE: f64 = 4.0;

/// Light or dark, as the app's content is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

struct Colors {
    bar: Color,
    ink: Color,
    /// Title and symbols of a window that is not active.
    faint: Color,
    hover: Color,
    pressed: Color,
}

impl Appearance {
    fn colors(self) -> Colors {
        let veil = |base: u32, a: u8| Color {
            a,
            ..Color::rgb(base)
        };
        match self {
            Appearance::Light => Colors {
                bar: Color::rgb(0xf3f3f3),
                ink: Color::rgb(0x1a1a1a),
                faint: Color::rgb(0x9a9a9a),
                hover: veil(0x000000, 0x10),
                pressed: veil(0x000000, 0x20),
            },
            Appearance::Dark => Colors {
                bar: Color::rgb(0x2b2b2b),
                ink: Color::rgb(0xffffff),
                faint: Color::rgb(0x8a8a8a),
                hover: veil(0xffffff, 0x10),
                pressed: veil(0xffffff, 0x20),
            },
        }
    }
}

/// Windows 11's close button under the pointer, and pressed.
const CLOSE_HOVER: Color = Color::rgb(0xc42b1c);
const CLOSE_PRESSED: Color = Color::rgb(0xc84031);
const CLOSE_INK: Color = Color::rgb(0xffffff);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Maximize,
    Close,
}

/// What a press on the bar asks for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Held on a button; what it does comes with the release over it.
    Button,
    /// Move the window with the pointer.
    Move,
    /// Maximize it, or back.
    ToggleMaximized,
}

#[derive(Default)]
pub(crate) struct TitleBar {
    pub title: String,
    pub appearance: Appearance,
    pub active: bool,
    pub maximized: bool,
    hover: Option<Button>,
    pressed: Option<Button>,
    last_press: Option<(Instant, f64, f64)>,
}

impl TitleBar {
    pub fn new(title: &str) -> TitleBar {
        TitleBar {
            title: title.to_owned(),
            ..TitleBar::default()
        }
    }

    /// The buttons and where they lie in a window `width` wide, right to
    /// left.
    fn buttons(width: f32) -> [(Button, Rect); 2] {
        let at = |n: f32| Rect {
            x: width - n * BUTTON_WIDTH,
            y: 0.0,
            w: BUTTON_WIDTH,
            h: HEIGHT as f32,
        };
        [(Button::Close, at(1.0)), (Button::Maximize, at(2.0))]
    }

    fn button_at(width: f32, x: f64, y: f64) -> Option<Button> {
        let (x, y) = (x as f32, y as f32);
        TitleBar::buttons(width)
            .into_iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
            .map(|(button, _)| button)
    }

    /// The pointer moved to `x`, `y` on the bar, or left it (None); whether
    /// the bar looks different now.
    pub fn hover(&mut self, width: f32, at: Option<(f64, f64)>) -> bool {
        let hover = at.and_then(|(x, y)| TitleBar::button_at(width, x, y));
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// The left button went down at `x`, `y` on the bar.
    pub fn press(&mut self, width: f32, x: f64, y: f64, now: Instant) -> Press {
        if let Some(button) = TitleBar::button_at(width, x, y) {
            self.pressed = Some(button);
            self.last_press = None;
            return Press::Button;
        }
        let double = self.last_press.is_some_and(|(at, px, py)| {
            now.duration_since(at) <= DOUBLE_CLICK
                && (x - px).hypot(y - py) <= DOUBLE_CLICK_DISTANCE
        });
        if double {
            self.last_press = None;
            Press::ToggleMaximized
        } else {
            self.last_press = Some((now, x, y));
            Press::Move
        }
    }

    /// The left button came up: the button it went down on, if it is still
    /// under the pointer.
    pub fn release(&mut self) -> Option<Button> {
        let pressed = self.pressed.take();
        pressed.filter(|p| Some(*p) == self.hover)
    }

    /// Whether a button is held.
    pub fn holding(&self) -> bool {
        self.pressed.is_some()
    }

    /// The bar across a window `width` wide.
    pub fn draw(&self, text: &mut Text, width: f32) -> Scene {
        let colors = self.appearance.colors();
        let ink = if self.active {
            colors.ink
        } else {
            colors.faint
        };
        let mut scene = Scene::new();
        scene.fill(
            Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: HEIGHT as f32,
            },
            colors.bar,
        );
        let style = Style {
            family: Family::Sans,
            size: TITLE_SIZE,
            weight: TITLE_WEIGHT,
            color: ink,
        };
        let layout = text.layout(&self.title, &style, None);
        // Centred in the window; a title too long for that starts at the
        // left and runs under the buttons, which cover it.
        let room = width - 2.0 * 2.0 * BUTTON_WIDTH;
        let x = if layout.width() <= room {
            (width - layout.width()) / 2.0
        } else {
            12.0
        };
        let y = (HEIGHT as f32 - layout.height()) / 2.0;
        scene.text(&layout, x.round(), y.round());
        for (button, rect) in TitleBar::buttons(width) {
            scene.fill(rect, colors.bar);
            let held = self.pressed == Some(button) && self.hover == Some(button);
            let hovered = self.hover == Some(button);
            let mut symbol_ink = ink;
            if button == Button::Close && (held || hovered) {
                scene.fill(rect, if held { CLOSE_PRESSED } else { CLOSE_HOVER });
                symbol_ink = CLOSE_INK;
            } else if held {
                scene.fill(rect, colors.pressed);
            } else if hovered {
                scene.fill(rect, colors.hover);
            }
            let name = match button {
                Button::Close => "x",
                Button::Maximize if self.maximized => "copy-simple",
                Button::Maximize => "square",
            };
            let size = SYMBOL as f32;
            scene.symbol(
                name,
                rect.x + (rect.w - size) / 2.0,
                rect.y + (rect.h - size) / 2.0,
                SYMBOL,
                symbol_ink,
            );
        }
        scene
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_lie_at_the_right_end() {
        assert_eq!(TitleBar::button_at(400.0, 399.0, 0.0), Some(Button::Close));
        assert_eq!(TitleBar::button_at(400.0, 354.0, 31.0), Some(Button::Close));
        assert_eq!(
            TitleBar::button_at(400.0, 353.0, 10.0),
            Some(Button::Maximize)
        );
        assert_eq!(
            TitleBar::button_at(400.0, 308.0, 10.0),
            Some(Button::Maximize)
        );
        assert_eq!(TitleBar::button_at(400.0, 307.0, 10.0), None);
        assert_eq!(TitleBar::button_at(400.0, 380.0, 32.0), None);
    }

    #[test]
    fn a_button_acts_only_when_released_over_it() {
        let mut bar = TitleBar::new("t");
        let now = Instant::now();
        bar.hover(400.0, Some((380.0, 10.0)));
        assert_eq!(bar.press(400.0, 380.0, 10.0, now), Press::Button);
        assert_eq!(bar.release(), Some(Button::Close));
        // Pressed on close, released over maximize: nothing.
        bar.press(400.0, 380.0, 10.0, now);
        bar.hover(400.0, Some((330.0, 10.0)));
        assert_eq!(bar.release(), None);
        assert!(!bar.holding());
    }

    #[test]
    fn a_double_click_maximizes_a_drag_moves() {
        let mut bar = TitleBar::new("t");
        let now = Instant::now();
        assert_eq!(bar.press(400.0, 100.0, 10.0, now), Press::Move);
        assert_eq!(
            bar.press(400.0, 101.0, 11.0, now + Duration::from_millis(300)),
            Press::ToggleMaximized
        );
        // A third press starts again.
        assert_eq!(
            bar.press(400.0, 101.0, 11.0, now + Duration::from_millis(400)),
            Press::Move
        );
        // Too late, or too far.
        assert_eq!(
            bar.press(400.0, 101.0, 11.0, now + Duration::from_millis(900)),
            Press::Move
        );
        assert_eq!(
            bar.press(400.0, 140.0, 11.0, now + Duration::from_millis(1000)),
            Press::Move
        );
    }
}
