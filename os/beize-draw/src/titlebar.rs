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
//! The buttons' circles and marks are shapes of their own, drawn for their
//! size (`Scene::rounded`, `stroke`, `triangle`), as AppKit's and Windows'
//! are: a set of symbols draws a square larger than an X, which looks
//! wrong at this size. The GPU computes them per pixel, sharp at any size.
//!
//! Minimizing takes the window off the screen; the dock brings it back
//! (ADR 0028 §4).

use std::time::{Duration, Instant};

use crate::draw::{Cap, Color, Rect, Scene};
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
    /// The bar's height: macOS' title bar (28 for its 12 px circles) grown
    /// with Beize's larger circles, the same space above and below them;
    /// Windows 11's caption buttons.
    pub const fn height(self) -> u32 {
        match self {
            Look::Mac => CIRCLE + 2 * CIRCLE_TOP,
            Look::Windows => 32,
        }
    }
}

/// The look of every bar, until the settings offer the choice.
pub const LOOK: Look = Look::Mac;
/// The bar's height in `LOOK`.
pub const HEIGHT: u32 = LOOK.height();

/// macOS' 13 with its 12 px circles, grown with Beize's.
const TITLE_SIZE: f32 = 15.0;
const TITLE_WEIGHT: f32 = 600.0;
/// Two presses at most this far apart are a double click (GNOME's
/// default), and at most this far moved.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_DISTANCE: f64 = 4.0;

/// macOS' circles: their diameter and the step from one to the next, as
/// measured in macOS 26 (13.3 and 23 pt in the owner's photo of Finder; the
/// owner chose 14, 25.10.: AppKit's standard 12/20 looked too small on his
/// screen); the space above, below and left of them, AppKit's 8 grown with
/// them.
const CIRCLE: u32 = 14;
const CIRCLE_STEP: f32 = 23.0;
const CIRCLE_TOP: u32 = 9;
const CIRCLE_LEFT: f32 = CIRCLE_TOP as f32;
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
    /// macOS' circle of a window that is not active, and its rim.
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

/// macOS' circles as AppKit draws them: the fill, its rim, both held down,
/// and the mark. Held down the whole circle darkens: the rim by the fill's
/// own factor per channel (about 0.75), so it stays darker than the fill.
struct Circle {
    fill: u32,
    rim: u32,
    pressed: u32,
    pressed_rim: u32,
    mark: u32,
}

impl Circle {
    /// The fill and the rim, held down or not.
    fn colours(&self, held: bool) -> (u32, u32) {
        if held {
            (self.pressed, self.pressed_rim)
        } else {
            (self.fill, self.rim)
        }
    }
}

const RED: Circle = Circle {
    fill: 0xff5f57,
    rim: 0xe0443e,
    pressed: 0xbf4942,
    pressed_rim: 0xa8342f,
    mark: 0x4d0000,
};
const YELLOW: Circle = Circle {
    fill: 0xfebc2e,
    rim: 0xdea123,
    pressed: 0xbf8e22,
    pressed_rim: 0xa77a1a,
    mark: 0x995700,
};
const GREEN: Circle = Circle {
    fill: 0x28c840,
    rim: 0x1aab29,
    pressed: 0x1f9a31,
    pressed_rim: 0x14841f,
    mark: 0x006500,
};

/// macOS' circles and marks on a 12 grid scaled to the circle: the rim
/// half a unit wide inside its edge; close and minimize strokes 1.2 wide
/// with round ends, zoom two triangles pointing apart at 45 degrees.
const GRID: f32 = 12.0;
const RIM: f32 = 0.5;
const MAC_STROKE: f32 = 1.2;
const MAC_CLOSE: [[[f32; 2]; 2]; 2] = [[[3.5, 3.5], [8.5, 8.5]], [[8.5, 3.5], [3.5, 8.5]]];
const MAC_MINIMIZE: [[f32; 2]; 2] = [[3.0, 6.0], [9.0, 6.0]];
const MAC_ZOOM: [[[f32; 2]; 3]; 2] = [
    [[3.5, 3.5], [7.7, 3.5], [3.5, 7.7]],
    [[8.5, 8.5], [4.3, 8.5], [8.5, 4.3]],
];
/// Windows' marks in their 16 px box, 10 px, one pixel wide on the pixel
/// grid; the boxes' sides run half a pixel past their corners, so they
/// meet without a gap.
const WIN_CLOSE: [[[f32; 2]; 2]; 2] = [[[3.0, 3.0], [13.0, 13.0]], [[13.0, 3.0], [3.0, 13.0]]];
const WIN_MINIMIZE: [[f32; 2]; 2] = [[3.0, 8.5], [13.0, 8.5]];
const WIN_MAXIMIZE: [[f32; 2]; 5] = [
    [3.5, 3.5],
    [12.5, 3.5],
    [12.5, 12.5],
    [3.5, 12.5],
    [3.5, 3.5],
];
/// The window in front, then the one behind it, as far as it shows.
const WIN_RESTORE: [&[[f32; 2]]; 2] = [
    &[
        [3.5, 5.5],
        [10.5, 5.5],
        [10.5, 12.5],
        [3.5, 12.5],
        [3.5, 5.5],
    ],
    &[
        [5.5, 5.5],
        [5.5, 3.5],
        [12.5, 3.5],
        [12.5, 10.5],
        [10.5, 10.5],
    ],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Close,
    Minimize,
    Maximize,
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
            Look::Windows => [
                (Button::Close, 1.0),
                (Button::Maximize, 2.0),
                (Button::Minimize, 3.0),
            ]
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
            let lit = self.active || self.over_buttons;
            let held = self.pressed == Some(button) && self.hover == Some(button);
            let (fill, rim) = if lit {
                let (fill, rim) = circle.colours(held);
                (Color::rgb(fill), Color::rgb(rim))
            } else {
                (colors.idle, colors.idle_rim)
            };
            // The rim, and the fill inside it.
            let unit = CIRCLE as f32 / GRID;
            let radius = CIRCLE as f32 / 2.0;
            let centre = [x + radius, y + radius];
            scene.circle(centre, radius, rim);
            scene.circle(centre, radius - RIM * unit, fill);
            if self.over_buttons {
                let at = |[px, py]: [f32; 2]| [x + px * unit, y + py * unit];
                let ink = Color::rgb(circle.mark);
                let width = MAC_STROKE * unit;
                match button {
                    Button::Close => {
                        for [a, b] in MAC_CLOSE {
                            scene.stroke(at(a), at(b), width, Cap::Round, ink);
                        }
                    }
                    Button::Minimize => {
                        let [a, b] = MAC_MINIMIZE;
                        scene.stroke(at(a), at(b), width, Cap::Round, ink);
                    }
                    Button::Maximize => {
                        for corners in MAC_ZOOM {
                            scene.triangle(corners.map(at), ink);
                        }
                    }
                }
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
            let size = MARK as f32;
            let x = (rect.x + (rect.w - size) / 2.0).round();
            let y = (rect.y + (rect.h - size) / 2.0).round();
            let at = |[px, py]: [f32; 2]| [x + px, y + py];
            let mut line = |a, b, cap| scene.stroke(at(a), at(b), 1.0, cap, mark_ink);
            match button {
                Button::Close => {
                    for [a, b] in WIN_CLOSE {
                        line(a, b, Cap::Butt);
                    }
                }
                Button::Minimize => {
                    let [a, b] = WIN_MINIMIZE;
                    line(a, b, Cap::Butt);
                }
                Button::Maximize => {
                    let paths: &[&[[f32; 2]]] = if self.maximized {
                        &WIN_RESTORE
                    } else {
                        &[&WIN_MAXIMIZE]
                    };
                    for path in paths {
                        for pair in path.windows(2) {
                            line(pair[0], pair[1], Cap::Square);
                        }
                    }
                }
            }
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
        assert_eq!(bar.button_at(400.0, 16.0, 16.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 39.0, 16.0), Some(Button::Minimize));
        assert_eq!(bar.button_at(400.0, 62.0, 16.0), Some(Button::Maximize));
        assert_eq!(bar.button_at(400.0, 74.0, 16.0), None);
        assert_eq!(bar.button_at(400.0, 380.0, 16.0), None);
        assert_eq!(bar.button_at(400.0, 16.0, 32.0), None);
    }

    #[test]
    fn windows_buttons_lie_at_the_right() {
        let bar = bar(Look::Windows);
        assert_eq!(bar.button_at(400.0, 399.0, 0.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 354.0, 31.0), Some(Button::Close));
        assert_eq!(bar.button_at(400.0, 353.0, 10.0), Some(Button::Maximize));
        assert_eq!(bar.button_at(400.0, 307.0, 10.0), Some(Button::Minimize));
        assert_eq!(bar.button_at(400.0, 261.0, 10.0), None);
        assert_eq!(bar.button_at(400.0, 380.0, 32.0), None);
    }

    #[test]
    fn a_button_acts_only_when_released_over_it() {
        let mut bar = bar(Look::Mac);
        let now = Instant::now();
        bar.hover(400.0, Some((16.0, 10.0)));
        assert_eq!(bar.press(400.0, 16.0, 10.0, now), Press::Button);
        assert_eq!(bar.release(), Some(Button::Close));
        // Pressed on close, released over zoom: nothing.
        bar.press(400.0, 16.0, 10.0, now);
        bar.hover(400.0, Some((62.0, 10.0)));
        assert_eq!(bar.release(), None);
        assert!(!bar.holding());
        bar.hover(400.0, Some((39.0, 10.0)));
        assert_eq!(bar.press(400.0, 39.0, 10.0, now), Press::Button);
        assert_eq!(bar.release(), Some(Button::Minimize));
    }

    #[test]
    fn a_circles_rim_stays_darker_than_its_fill_when_held() {
        let channels = |c: u32| [c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff];
        for circle in [&RED, &YELLOW, &GREEN] {
            for held in [false, true] {
                let (fill, rim) = circle.colours(held);
                let (fill, rim) = (channels(fill), channels(rim));
                assert!(
                    fill.iter().zip(rim).all(|(f, r)| r <= *f),
                    "rim {rim:?} lighter than fill {fill:?}, held {held}"
                );
            }
        }
    }

    #[test]
    fn the_marks_show_with_the_pointer_over_the_circles() {
        let mut bar = bar(Look::Mac);
        assert!(bar.hover(400.0, Some((39.0, 10.0))));
        assert!(bar.over_buttons);
        // From one circle to the next: still over them.
        assert!(bar.hover(400.0, Some((62.0, 10.0))));
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
