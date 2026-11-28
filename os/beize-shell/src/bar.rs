//! The menu bar (ADR 0028 §3): its measures, what lies behind it, and
//! drawing it – Beize's mark, the active app's name and menus, the
//! status and the clock – with where its buttons are.

use super::*;

pub(super) const HEIGHT: u32 = measures::MENU_BAR;
/// The bar's text on a light background and on a dark one.
pub(super) const INK_LIGHT: Color = Color::rgb(0x1d1d1d);
pub(super) const INK_DARK: Color = Color::rgb(0xffffff);
/// Its size, macOS' 13 a little larger (the owner chose between 15 and
/// 16.25, macOS' at `SCALE`), and the space at the bar's ends: macOS' 12
/// at `SCALE` was 15, the owner wanted the ends further in (28.10.: 20).
/// The control center's right edge lies on it too, under the clock's.
pub(super) const TEXT_SIZE: f32 = 15.0;
pub(super) const MARGIN: f32 = 20.0;
/// The bar's symbols: Beize's mark and Phosphor's, as the mock-ups the
/// owner approved (28.10.); the space between the mark and the app's name.
/// The globe larger, its circle about 16 px (the owner's, 28.10.).
pub(super) const SYMBOL: u32 = 18;
pub(super) const SYMBOL_GAP: f32 = 22.0;
pub(super) const GLOBE: u32 = 20;
/// The pill below an open menu's button, round at the ends, flat, no
/// glass: white at 12 % below white ink, subtle as macOS' (the owner, 31.10.,
/// on mock-ups); below the control center's button light grey at 34 % –
/// lighter than the bar, as Apple's (its open control-center button: the
/// backdrop about 1.45 times as light, measured), greyer and more opaque than
/// white at 24 % so it shows, the owner's on hardware, 31.10. –; black at 10 %
/// below dark ink, where the bar is light. 36 pt wide under the Beize menu (measured in
/// the HIG's menu-bar image); under the control center's the owner's
/// 54 × 28 px (28.10.; Apple's 46.5 × 24 pt at `SCALE`, 58 × 30, filled
/// the bar). The control center's button is its pill: the clock and the
/// globe keep Apple's space from it, about 6 pt to their ink in Apple's
/// image of it (2 px per pt), at `SCALE`.
pub(super) const PILL_HEIGHT: f32 = 28.0;
pub(super) const MENU_PILL: f32 = 36.0 * measures::SCALE;
pub(super) const CONTROL_CENTER_PILL: f32 = 54.0;
pub(super) const BUTTON_SPACE: f32 = 8.0;
/// The space either side of an app's menu title, which its open pill fills:
/// 21 to 22 pt between titles on a Mac (measured on the owner's image at 2 px
/// per pt, 31.10.), at `SCALE`.
pub(super) const MENU_PADDING: f32 = 13.0;

/// What lies behind the menu bar (ADR 0028 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarLook {
    /// No background, as macOS' since 26.
    Transparent,
    /// Base glass, as macOS' with "Show menu bar background".
    Glass,
}

impl BarLook {
    /// Transparent, as macOS starts. Until Beize's settings hold the
    /// choice, `BEIZE_MENU_BAR=glass` in the desktop's environment shows
    /// the background (like `BEIZE_APPEARANCE`).
    pub(super) fn from_environment() -> BarLook {
        match std::env::var("BEIZE_MENU_BAR").as_deref() {
            Ok("glass") => BarLook::Glass,
            _ => BarLook::Transparent,
        }
    }
}

/// The bar's glass (`BarLook::Glass`): all of it, square, without a rim –
/// it runs along the screen's edge –, base glass: no window lies behind it.
pub(super) fn bar_glass((w, h): (u32, u32)) -> Glass {
    let all = Rect {
        x: 0.0,
        y: 0.0,
        w: w as f32,
        h: h as f32,
    };
    Glass::one(all, 0.0, false, Material::Base)
}

/// What the bar shows.
pub(super) struct BarContent<'a> {
    pub(super) app: Option<&'a str>,
    /// The titles of the menus the active app describes.
    pub(super) menus: &'a [String],
    pub(super) time: Option<String>,
    pub(super) network: network::Status,
    pub(super) open: Option<Open>,
}

/// The bar's buttons as drawn: where each lies (its pill), for a press.
#[derive(Default)]
pub(super) struct Buttons {
    pub(super) beize_menu: Option<Rect>,
    pub(super) control_center: Option<Rect>,
    pub(super) width: f32,
    /// The menus along the bar's left side, in order, and how each opens.
    pub(super) along: Vec<(Open, Rect, Align)>,
}

impl Buttons {
    /// Each button's centre, in whole pixels.
    pub(super) fn centres(&self) -> [Option<(i32, i32)>; 2] {
        let centre = |r: Option<Rect>| {
            r.map(|r| {
                (
                    (r.x + r.w / 2.0).round() as i32,
                    (r.y + r.h / 2.0).round() as i32,
                )
            })
        };
        [centre(self.beize_menu), centre(self.control_center)]
    }

    /// The menus along the bar's left side, in order.
    pub(super) fn along_bar(&self) -> Vec<(Open, Rect, Align)> {
        self.along.clone()
    }

    /// The button at `(x, y)` on the bar, what its menu opens below and
    /// how it lines up: the Beize menu from its button's left edge, the
    /// control center a gap from the screen's right edge.
    pub(super) fn at(&self, (x, y): (f64, f64)) -> Option<(Open, Rect, Align)> {
        let (x, y) = (x as f32, y as f32);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= 0.0 && y < HEIGHT as f32;
        if let Some(found) = self.along.iter().find(|(_, r, _)| inside(r)) {
            return Some(*found);
        }
        if self.control_center.is_some_and(|r| inside(&r)) {
            let right = self.width - MARGIN;
            let anchor = Rect {
                x: right - 1.0,
                y: 0.0,
                w: 1.0,
                h: HEIGHT as f32,
            };
            return Some((Open::ControlCenter, anchor, Align::Right));
        }
        None
    }
}

/// The bar in `size`, on a `backdrop` light or dark: Beize's mark and the
/// active app's name on the left, the network, the control center and the
/// time on the right, all centred in the height; the rest transparent. And
/// where its buttons lie.
pub(super) fn menu_bar(
    text: &mut Text,
    (w, h): (u32, u32),
    content: &BarContent<'_>,
    backdrop: Appearance,
) -> (Scene, Buttons) {
    let (w, h) = (w as f32, h as f32);
    let mut scene = Scene::new();
    scene.clear(Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    });
    let ink = match backdrop {
        Appearance::Light => INK_LIGHT,
        Appearance::Dark => INK_DARK,
    };
    let pill_color = match backdrop {
        Appearance::Light => Color {
            a: 26,
            ..Color::rgb(0)
        },
        Appearance::Dark => Color {
            a: 87,
            ..Color::rgb(0xe1_e1_e1)
        },
    };
    let menu_pill = match backdrop {
        Appearance::Light => pill_color,
        Appearance::Dark => Color {
            a: 31,
            ..Color::rgb(0xff_ff_ff)
        },
    };
    let symbol = SYMBOL as f32;
    let symbol_y = ((h - symbol) / 2.0).round();
    // A pill `width` wide centred on a symbol at `x`, for its button.
    let pill = |x: f32, width: f32| Rect {
        x: (x + symbol / 2.0 - width / 2.0).round(),
        y: ((h - PILL_HEIGHT) / 2.0).round(),
        w: width.round(),
        h: PILL_HEIGHT.round(),
    };
    let style = |weight| Style {
        family: Family::Sans,
        size: TEXT_SIZE,
        weight,
        color: ink,
    };
    let centred = |height: f32| ((h - height) / 2.0).round();

    let logo_x = (MARGIN + 4.0).round();
    let beize_button = pill(logo_x, MENU_PILL);
    if content.open == Some(Open::BeizeMenu) {
        scene.rounded(beize_button, beize_button.h / 2.0, menu_pill);
    }
    logo(&mut scene, logo_x, symbol_y, symbol, ink);
    let bar_anchor = |r: Rect| Rect {
        x: r.x,
        y: 0.0,
        w: r.w,
        h,
    };
    let mut along = vec![(Open::BeizeMenu, bar_anchor(beize_button), Align::Left)];
    if let Some(app) = content.app {
        // Bold, as macOS names the active app; then its menus, each its text
        // on a pill wider by `MENU_PADDING` a side.
        let mut x = logo_x + symbol + SYMBOL_GAP;
        let mut item =
            |title: &str, weight: f32, which: Open, text: &mut Text, scene: &mut Scene| {
                let layout = text.layout(title, &style(weight), None);
                let button = Rect {
                    x: (x - MENU_PADDING).round(),
                    y: ((h - PILL_HEIGHT) / 2.0).round(),
                    w: (layout.width() + 2.0 * MENU_PADDING).round(),
                    h: PILL_HEIGHT,
                };
                if content.open == Some(which) {
                    scene.rounded(button, button.h / 2.0, menu_pill);
                }
                scene.text(&layout, x.round(), centred(layout.height()));
                along.push((which, bar_anchor(button), Align::Left));
                x += layout.width() + 2.0 * MENU_PADDING;
            };
        item(app, 700.0, Open::AppName, text, &mut scene);
        for (i, title) in content.menus.iter().enumerate() {
            item(title, 500.0, Open::AppMenu(i), text, &mut scene);
        }
        item("Window", 500.0, Open::WindowMenu, text, &mut scene);
    }

    let mut right = w - MARGIN;
    if let Some(time) = &content.time {
        let layout = text.layout(time, &style(500.0), None);
        right -= layout.width();
        scene.text(&layout, right.round(), centred(layout.height()));
        right -= BUTTON_SPACE;
    }
    let cc_button = Rect {
        x: (right - CONTROL_CENTER_PILL).round(),
        y: ((h - PILL_HEIGHT) / 2.0).round(),
        w: CONTROL_CENTER_PILL,
        h: PILL_HEIGHT,
    };
    if content.open == Some(Open::ControlCenter) {
        scene.rounded(cc_button, cc_button.h / 2.0, pill_color);
    }
    let cc_x = (cc_button.x + (cc_button.w - symbol) / 2.0).round();
    scene.symbol("toggle-left-fill", cc_x, symbol_y, SYMBOL, ink);
    let globe = GLOBE as f32;
    let network_x = (cc_button.x - BUTTON_SPACE - globe).round();
    let network_symbol = if content.network.connected {
        "globe-simple"
    } else {
        "globe-simple-x"
    };
    scene.symbol(
        network_symbol,
        network_x,
        ((h - globe) / 2.0).round(),
        GLOBE,
        ink,
    );
    let buttons = Buttons {
        beize_menu: Some(beize_button),
        control_center: Some(cc_button),
        width: w,
        along,
    };
    (scene, buttons)
}

/// Beize's mark (the owner, 28.10.: two bars with round ends, slanting up,
/// one above the other like a B) in a square of `size` at `(x, y)`: two
/// strokes with round caps, sharp at any size. Drawn on a 16-unit grid.
pub(super) fn logo(scene: &mut Scene, x: f32, y: f32, size: f32, color: Color) {
    let k = size / 16.0;
    let at = |u: f32, v: f32| [x + u * k, y + v * k];
    let width = 3.8 * k;
    scene.stroke(at(4.0, 6.6), at(11.0, 3.8), width, Cap::Round, color);
    scene.stroke(at(4.0, 12.4), at(12.6, 9.0), width, Cap::Round, color);
}
