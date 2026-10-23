//! The title bar on top of every window (ADR 0028 §2): Beize UI draws it
//! on its own, as AppKit does, the compositor on other apps' windows that
//! ask it for their frame (xdg-decoration). What it does is one: dragged it
//! moves the window, a double click maximizes it and back, a button acts
//! when released over it. How it looks is a `Look`: macOS' (Beize's) or
//! Windows 11's, the same code for both sides, so a later setting switches
//! every window at once.
//!
//! macOS: the three circles at the left – close red, minimize yellow, zoom
//! green – grey in a window that is not active; with the pointer over them
//! all three show their marks. Windows 11: flat buttons 46 px wide at the
//! right, close red under the pointer. The title is centred in both.
//!
//! The buttons' marks are shapes of their own, drawn for their size on the
//! pixel grid (`Scene::shape`), as AppKit's and Windows' are: a set of
//! symbols draws a square larger than an X, which looks wrong at this size.
//!
//! Minimizing comes with the dock, which brings a window back (ADR 0028 c):
//! until then macOS' yellow circle is grey, as macOS shows a button that
//! is not available, and Windows' look has none.

use std::time::{Duration, Instant};

use crate::draw::{Color, Rect, Scene};
use crate::text::{Family, Style, Text};

/// How the bar looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// The circles at the left (Beize's own).
    #[default]
    Mac,
    /// Flat buttons at the right.
    Windows,
}

impl Look {
    /// The bar's height: macOS' title bar, Windows 11's caption buttons.
    pub const fn height(self) -> u32 {
        match self {
            Look::Mac => 28,
            Look::Windows => 32,
        }
    }
}

/// The look of every bar, until the settings offer the choice.
pub const LOOK: Look = Look::Mac;
/// The bar's height in `LOOK`.
pub const HEIGHT: u32 = LOOK.height();

const TITLE_SIZE: f32 = 13.0;
const TITLE_WEIGHT: f32 = 600.0;
/// Two presses at most this far apart are a double click (GNOME's
/// default), and at most this far moved.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_DISTANCE: f64 = 4.0;

/// macOS' circles: their diameter, the first one's left edge and the step
/// from one to the next (AppKit's standard window buttons).
const CIRCLE: u32 = 12;
const CIRCLE_LEFT: f32 = 8.0;
const CIRCLE_STEP: f32 = 20.0;
/// Windows 11's buttons and the box their marks fill.
const BUTTON_WIDTH: f32 = 46.0;
const MARK: u32 = 16;

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
    /// Title and marks of a window that is not active.
    faint: Color,
    hover: Color,
    pressed: Color,
    /// macOS' circle of a window that is not active, or of a button that
    /// is not available, and its rim.
    idle: Color,
    idle_rim: Color,
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
                idle: Color::rgb(0xd6d6d6),
                idle_rim: Color::rgb(0xc4c4c4),
            },
            Appearance::Dark => Colors {
                bar: Color::rgb(0x2b2b2b),
                ink: Color::rgb(0xffffff),
                faint: Color::rgb(0x8a8a8a),
                hover: veil(0xffffff, 0x10),
                pressed: veil(0xffffff, 0x20),
                idle: Color::rgb(0x4a4a4a),
                idle_rim: Color::rgb(0x404040),
            },
        }
    }
}

/// Windows 11's close button under the pointer, and pressed.
const CLOSE_HOVER: Color = Color::rgb(0xc42b1c);
const CLOSE_PRESSED: Color = Color::rgb(0xc84031);
const CLOSE_INK: Color = Color::rgb(0xffffff);

/// macOS' circles as AppKit draws them: the fill, its rim, the fill held
/// down, and the mark.
struct Circle {
    fill: u32,
    rim: u32,
    pressed: u32,
    mark: u32,
}

const RED: Circle = Circle {
    fill: 0xff5f57,
    rim: 0xe0443e,
    pressed: 0xbf4942,
    mark: 0x4d0000,
};
const YELLOW: Circle = Circle {
    fill: 0xfebc2e,
    rim: 0xdea123,
    pressed: 0xbf8e22,
    mark: 0x995700,
};
const GREEN: Circle = Circle {
    fill: 0x28c840,
    rim: 0x1aab29,
    pressed: 0x1f9a31,
    mark: 0x006500,
};

/// The marks, each on its own grid: macOS' in the 12 px circle, Windows'
/// in a 16 px box with a 10 px mark, one pixel wide, on the pixel grid.
const DISC: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="6"/></svg>"#;
const RIM: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><circle cx="6" cy="6" r="5.75" fill="none" stroke="#000" stroke-width="0.5"/></svg>"##;
const MAC_CLOSE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M3.5 3.5l5 5M8.5 3.5l-5 5" stroke="#000" stroke-width="1.2" stroke-linecap="round"/></svg>"##;
const MAC_MINIMIZE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M3 6h6" stroke="#000" stroke-width="1.2" stroke-linecap="round"/></svg>"##;
const MAC_ZOOM: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M3.5 3.5h4.2l-4.2 4.2zM8.5 8.5H4.3l4.2-4.2z"/></svg>"#;
const WIN_CLOSE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3 3l10 10M13 3L3 13" stroke="#000" stroke-width="1"/></svg>"##;
const WIN_MAXIMIZE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="3.5" y="3.5" width="9" height="9" fill="none" stroke="#000" stroke-width="1"/></svg>"##;
const WIN_RESTORE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3.5 5.5h7v7h-7zM5.5 5.5v-2h7v7h-2" fill="none" stroke="#000" stroke-width="1"/></svg>"##;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Close,
    Minimize,
    Maximize,
}

impl Button {
    /// Whether it does something yet: minimizing waits for the dock.
    fn available(self) -> bool {
        self != Button::Minimize
    }
}

/// What a press on the bar asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Press {
    /// Held on a button; what it does comes with the release over it.
    Button,
    /// Move the window with the pointer.
    Move,
    /// Maximize it, or back.
    ToggleMaximized,
}

#[derive(Default)]
pub struct TitleBar {
    pub title: String,
    pub appearance: Appearance,
    pub active: bool,
    pub maximized: bool,
    pub look: Look,
    hover: Option<Button>,
    /// The pointer is over the buttons (macOS shows their marks then).
    over_buttons: bool,
    pressed: Option<Button>,
    last_press: Option<(Instant, f64, f64)>,
}

impl TitleBar {
    pub fn new(title: &str) -> TitleBar {
        TitleBar {
            title: title.to_owned(),
            look: LOOK,
            ..TitleBar::default()
        }
    }

    pub fn height(&self) -> u32 {
        self.look.height()
    }

    /// The buttons and where they lie in a window `width` wide: what takes
    /// a press for each.
    pub fn buttons(&self, width: f32) -> Vec<(Button, Rect)> {
        let height = self.height() as f32;
        match self.look {
            Look::Mac => [Button::Close, Button::Minimize, Button::Maximize]
                .into_iter()
                .enumerate()
                .map(|(i, button)| {
                    // From halfway to the neighbours, the bar's height.
                    let centre = CIRCLE_LEFT + CIRCLE as f32 / 2.0 + i as f32 * CIRCLE_STEP;
                    let rect = Rect {
                        x: centre - CIRCLE_STEP / 2.0,
                        y: 0.0,
                        w: CIRCLE_STEP,
                        h: height,
                    };
                    (button, rect)
                })
                .collect(),
            Look::Windows => [(Button::Close, 1.0), (Button::Maximize, 2.0)]
                .into_iter()
                .map(|(button, n)| {
                    let rect = Rect {
                        x: width - n * BUTTON_WIDTH,
                        y: 0.0,
                        w: BUTTON_WIDTH,
                        h: height,
                    };
                    (button, rect)
                })
                .collect(),
        }
    }

    fn button_at(&self, width: f32, x: f64, y: f64) -> Option<Button> {
        let (x, y) = (x as f32, y as f32);
        self.buttons(width)
            .into_iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
            .map(|(button, _)| button)
    }

    /// The pointer moved to `x`, `y` on the bar, or left it (None); whether
    /// the bar looks different now.
    pub fn hover(&mut self, width: f32, at: Option<(f64, f64)>) -> bool {
        let hover = at.and_then(|(x, y)| self.button_at(width, x, y));
        let over = hover.is_some();
        let changed = hover != self.hover || over != self.over_buttons;
        self.hover = hover;
        self.over_buttons = over;
        changed
    }

    /// The left button went down at `x`, `y` on the bar.
    pub fn press(&mut self, width: f32, x: f64, y: f64, now: Instant) -> Press {
        if let Some(button) = self.button_at(width, x, y) {
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
    /// under the pointer and does something.
    pub fn release(&mut self) -> Option<Button> {
        let pressed = self.pressed.take();
        pressed.filter(|p| Some(*p) == self.hover && p.available())
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
        let height = self.height() as f32;
        let mut scene = Scene::new();
        scene.fill(
            Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: height,
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
        // Centred in the window; a title too long for that starts after
        // the circles (macOS) or at the left and runs under the buttons
        // (Windows).
        let (start, buttons) = match self.look {
            Look::Mac => (
                CIRCLE_LEFT + 3.0 * CIRCLE_STEP + 8.0,
                3.0 * CIRCLE_STEP + 16.0,
            ),
            Look::Windows => (12.0, 2.0 * BUTTON_WIDTH),
        };
        let room = width - 2.0 * buttons;
        let x = if layout.width() <= room {
            (width - layout.width()) / 2.0
        } else {
            start
        };
        let y = (height - layout.height()) / 2.0;
        scene.text(&layout, x.round(), y.round());
        match self.look {
            Look::Mac => self.draw_circles(&mut scene, &colors),
            Look::Windows => self.draw_buttons(&mut scene, &colors, ink, width),
        }
        scene
    }

    fn draw_circles(&self, scene: &mut Scene, colors: &Colors) {
        let y = ((self.height() - CIRCLE) / 2) as f32;
        for (i, button) in [Button::Close, Button::Minimize, Button::Maximize]
            .into_iter()
            .enumerate()
        {
            let x = CIRCLE_LEFT + i as f32 * CIRCLE_STEP;
            let circle = match button {
                Button::Close => &RED,
                Button::Minimize => &YELLOW,
                Button::Maximize => &GREEN,
            };
            // A window that is not active shows grey circles, but where
            // the pointer is over them: macOS shows them in colour then.
            let lit = (self.active || self.over_buttons) && button.available();
            let held = self.pressed == Some(button) && self.hover == Some(button);
            let (fill, rim) = if !lit {
                (colors.idle, colors.idle_rim)
            } else if held {
                (Color::rgb(circle.pressed), Color::rgb(circle.rim))
            } else {
                (Color::rgb(circle.fill), Color::rgb(circle.rim))
            };
            scene.shape("disc", DISC, x, y, CIRCLE, fill);
            scene.shape("rim", RIM, x, y, CIRCLE, rim);
            if self.over_buttons && button.available() {
                let (name, svg) = match button {
                    Button::Close => ("mac-close", MAC_CLOSE),
                    Button::Minimize => ("mac-minimize", MAC_MINIMIZE),
                    Button::Maximize => ("mac-zoom", MAC_ZOOM),
                };
                scene.shape(name, svg, x, y, CIRCLE, Color::rgb(circle.mark));
            }
        }
    }

    fn draw_buttons(&self, scene: &mut Scene, colors: &Colors, ink: Color, width: f32) {
        for (button, rect) in self.buttons(width) {
            let held = self.pressed == Some(button) && self.hover == Some(button);
            let hovered = self.hover == Some(button);
            let mut mark_ink = ink;
            if button == Button::Close && (held || hovered) {
                scene.fill(rect, if held { CLOSE_PRESSED } else { CLOSE_HOVER });
                mark_ink = CLOSE_INK;
            } else if held {
                scene.fill(rect, colors.pressed);
            } else if hovered {
                scene.fill(rect, colors.hover);
            }
            let (name, svg) = match button {
                Button::Close => ("win-close", WIN_CLOSE),
                Button::Maximize if self.maximized => ("win-restore", WIN_RESTORE),
                Button::Maximize | Button::Minimize => ("win-maximize", WIN_MAXIMIZE),
            };
            let size = MARK as f32;
            scene.shape(
                name,
                svg,
                (rect.x + (rect.w - size) / 2.0).round(),
                (rect.y + (rect.h - size) / 2.0).round(),
                MARK,
                mark_ink,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(look: Look) -> TitleBar {
        TitleBar {
            look,
            ..TitleBar::new("t")
        }
    }

    #[test]
    fn macs_circles_lie_at_the_left() {
        let bar = bar(Look::Mac);
        assert_eq!(bar.button_at(400.0, 14.0, 14.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 34.0, 14.0), Some(Button::Minimize));
        assert_eq!(bar.button_at(400.0, 54.0, 14.0), Some(Button::Maximize));
        assert_eq!(bar.button_at(400.0, 64.0, 14.0), None);
        assert_eq!(bar.button_at(400.0, 380.0, 14.0), None);
        assert_eq!(bar.button_at(400.0, 14.0, 28.0), None);
    }

    #[test]
    fn windows_buttons_lie_at_the_right() {
        let bar = bar(Look::Windows);
        assert_eq!(bar.button_at(400.0, 399.0, 0.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 354.0, 31.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 353.0, 10.0), Some(Button::Maximize));
        assert_eq!(bar.button_at(400.0, 307.0, 10.0), None);
        assert_eq!(bar.button_at(400.0, 380.0, 32.0), None);
    }

    #[test]
    fn a_button_acts_only_when_released_over_it() {
        let mut bar = bar(Look::Mac);
        let now = Instant::now();
        bar.hover(400.0, Some((14.0, 10.0)));
        assert_eq!(bar.press(400.0, 14.0, 10.0, now), Press::Button);
        assert_eq!(bar.release(), Some(Button::Close));
        // Pressed on close, released over zoom: nothing.
        bar.press(400.0, 14.0, 10.0, now);
        bar.hover(400.0, Some((54.0, 10.0)));
        assert_eq!(bar.release(), None);
        assert!(!bar.holding());
        // Minimize does nothing until the dock.
        bar.hover(400.0, Some((34.0, 10.0)));
        assert_eq!(bar.press(400.0, 34.0, 10.0, now), Press::Button);
        assert_eq!(bar.release(), None);
    }

    #[test]
    fn the_marks_show_with_the_pointer_over_the_circles() {
        let mut bar = bar(Look::Mac);
        assert!(bar.hover(400.0, Some((34.0, 10.0))));
        assert!(bar.over_buttons);
        // From one circle to the next: still over them.
        assert!(bar.hover(400.0, Some((54.0, 10.0))));
        assert!(bar.over_buttons);
        assert!(bar.hover(400.0, Some((200.0, 10.0))));
        assert!(!bar.over_buttons);
        assert!(!bar.hover(400.0, None));
    }

    #[test]
    fn a_double_click_maximizes_a_drag_moves() {
        let mut bar = bar(Look::Mac);
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
