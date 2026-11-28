//! Beize's design values (ADR 0033 §9): the grid, the radii, the type, the
//! colours by role, the glass's tint and densities, the two heights. No
//! other file names a number for one of these roles; what the building
//! blocks keep are derivations ("a capsule is an element", "text starts at
//! the capsule's edge plus its padding").
//!
//! Two sets of values while the owner chooses between them (STATUS, plan
//! B2): `without_references` is ADR 0033 as written, `with_references` is
//! the same system tuned by what his reference pictures share – warmer,
//! lighter glass, rounder, more air. One `pub use` picks the set; the one
//! not chosen goes when he has chosen.
//!
//! The contrast test below holds every set to WCAG over the hardest case of
//! each appearance – light glass over black (a terminal behind it), dark
//! glass over white (a light window behind it) –, mixed in linear light as
//! the compositor mixes.

use crate::Color;

pub use without_references::*;

/// One appearance's colours by role (ADR 0033 §5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Text and symbols.
    pub ink: Color,
    /// Secondary text, shortcuts, second lines: a colour of its own, not
    /// the ink faded – faded ink, mixed in linear light, keeps too little
    /// contrast over the hardest case (the test).
    pub ink_secondary: Color,
    /// Disabled entries; exempt from the contrast test.
    pub ink_disabled: Color,
    /// A neutral tile, button or field on glass.
    pub fill: Color,
    /// The same, chosen: an open button of the bar, a chosen tile.
    pub fill_chosen: Color,
    /// Choice and the main action; white on it.
    pub accent: Color,
    /// Error text, a destructive action.
    pub danger: Color,
    /// A separator in menus and surfaces.
    pub line: Color,
}

/// Beize glass's tint for one appearance (ADR 0033 §6): one neutral colour
/// over the blurred, saturated backdrop, at one of three densities. The
/// rim along its edge is the same at every density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub color: Color,
    /// Menus, dialogs, Alt+Tab, the control center, notifications.
    pub dense: f32,
    /// The dock.
    pub medium: f32,
    /// The menu bar with a background, the snap preview's companions.
    pub thin: f32,
    /// The rim's alpha (premultiplied white).
    pub rim: f32,
}

/// A shadow: how far down, how soft (a Gaussian's sigma) and how dark
/// (black's alpha) (ADR 0033 §7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elevation {
    pub offset: i32,
    pub sigma: f32,
    pub alpha: f32,
}

const fn rgba(hex: u32, a: u8) -> Color {
    Color {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
        a,
    }
}

/// The ADR's values. Sizes in px at Beize's scale 1.
pub mod without_references {
    use super::{Elevation, Palette, Tint, rgba};

    /// Every space and size is a multiple of this (§2).
    pub const GRID: f32 = 4.0;
    /// Large floating surfaces: dialog, Alt+Tab, control center,
    /// notifications, dock (§3).
    pub const RADIUS_SURFACE: f32 = 28.0;
    /// Windows, menus, capsules and tiles in a surface: the surface's less
    /// the space between them.
    pub const RADIUS_ELEMENT: f32 = 16.0;
    /// A menu row's highlight, a field, thumbnails: the element's less 4.
    pub const RADIUS_DETAIL: f32 = 12.0;
    /// Between a surface's edge and what lies in it: outer = inner + space.
    pub const INSET: f32 = RADIUS_SURFACE - RADIUS_ELEMENT;
    /// Buttons are capsules this high (radius = element).
    pub const CAPSULE: f32 = 32.0;
    /// A capsule's padding: free text starts at `INSET + CAPSULE_PADDING`,
    /// in line with a button's label.
    pub const CAPSULE_PADDING: f32 = 8.0;
    /// From the dock's icons to its rim, all round: its radius less the
    /// icons' own (about 14 px at 56).
    pub const DOCK_INSET: f32 = 14.0;

    /// Two sizes, three weights (§4).
    pub const TEXT: f32 = 15.0;
    pub const TEXT_SMALL: f32 = 13.0;
    pub const REGULAR: f32 = 400.0;
    pub const MEDIUM: f32 = 500.0;
    pub const SEMIBOLD: f32 = 600.0;

    pub const LIGHT: Palette = Palette {
        ink: rgba(0x1d1d1d, 255),
        ink_secondary: rgba(0x5e5e5e, 255),
        ink_disabled: rgba(0x1d1d1d, 77),
        fill: rgba(0x000000, 18),
        fill_chosen: rgba(0x000000, 31),
        accent: rgba(0x0064d2, 255),
        danger: rgba(0xc4001a, 255),
        line: rgba(0x000000, 20),
    };
    pub const DARK: Palette = Palette {
        ink: rgba(0xf2f2f2, 255),
        ink_secondary: rgba(0xa3a3a3, 255),
        ink_disabled: rgba(0xf2f2f2, 89),
        fill: rgba(0xffffff, 20),
        fill_chosen: rgba(0xffffff, 41),
        accent: rgba(0x0064d2, 255),
        danger: rgba(0xffb3ae, 255),
        line: rgba(0xffffff, 26),
    };

    /// The menu glass the owner loves (#F2F2F2 / #2D2D2D), dense: light
    /// 80 % as approved, dark as dense as the test asks.
    pub const TINT_LIGHT: Tint = Tint {
        color: rgba(0xf2f2f2, 255),
        dense: 0.80,
        medium: 0.60,
        thin: 0.30,
        rim: 0.30,
    };
    pub const TINT_DARK: Tint = Tint {
        color: rgba(0x2d2d2d, 255),
        dense: 0.94,
        medium: 0.60,
        thin: 0.30,
        rim: 0.12,
    };

    pub const WINDOW_ACTIVE: Elevation = Elevation {
        offset: 10,
        sigma: 14.0,
        alpha: 0.35,
    };
    pub const WINDOW_INACTIVE: Elevation = Elevation {
        offset: 4,
        sigma: 8.0,
        alpha: 0.20,
    };
    /// Everything of Beize glass.
    pub const FLOATING: Elevation = Elevation {
        offset: 8,
        sigma: 24.0,
        alpha: 0.18,
    };
}

/// The same system, tuned by what the owner's reference pictures share
/// (visionOS, smart, Li Auto, tvOS's control center): a warm neutral glass
/// that lets the background's colour through more in the light, one step
/// rounder, more air around the dock, and the choice shown as a surface.
/// Dark stays as dense as the test asks: over a white window anything
/// thinner loses the ink.
pub mod with_references {
    use super::{Palette, Tint, rgba};

    pub use super::without_references::{
        CAPSULE, CAPSULE_PADDING, FLOATING, GRID, MEDIUM, REGULAR, SEMIBOLD, TEXT, TEXT_SMALL,
        WINDOW_ACTIVE, WINDOW_INACTIVE,
    };

    pub const RADIUS_SURFACE: f32 = 32.0;
    pub const RADIUS_ELEMENT: f32 = 20.0;
    pub const RADIUS_DETAIL: f32 = 12.0;
    pub const INSET: f32 = RADIUS_SURFACE - RADIUS_ELEMENT;
    pub const DOCK_INSET: f32 = 18.0;

    pub const LIGHT: Palette = Palette {
        ink: rgba(0x1d1d1b, 255),
        ink_secondary: rgba(0x5e5d5b, 255),
        ink_disabled: rgba(0x1d1d1b, 77),
        fill: rgba(0x000000, 15),
        fill_chosen: rgba(0x000000, 26),
        accent: rgba(0x0064d2, 255),
        danger: rgba(0xb00018, 255),
        line: rgba(0x000000, 18),
    };
    pub const DARK: Palette = Palette {
        ink: rgba(0xf2f1ef, 255),
        ink_secondary: rgba(0xa3a29f, 255),
        ink_disabled: rgba(0xf2f1ef, 89),
        fill: rgba(0xffffff, 18),
        fill_chosen: rgba(0xffffff, 36),
        accent: rgba(0x0064d2, 255),
        danger: rgba(0xffb3ae, 255),
        line: rgba(0xffffff, 23),
    };

    pub const TINT_LIGHT: Tint = Tint {
        color: rgba(0xf2f1ef, 255),
        dense: 0.72,
        medium: 0.50,
        thin: 0.25,
        rim: 0.30,
    };
    pub const TINT_DARK: Tint = Tint {
        color: rgba(0x2e2d2b, 255),
        dense: 0.94,
        medium: 0.55,
        thin: 0.30,
        rim: 0.12,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// A colour in linear light, its alpha over `under` (linear).
    fn over(c: Color, under: [f64; 3]) -> [f64; 3] {
        let a = f64::from(c.a) / 255.0;
        let rgb = [linear(c.r), linear(c.g), linear(c.b)];
        std::array::from_fn(|i| rgb[i] * a + under[i] * (1.0 - a))
    }

    fn luminance(c: [f64; 3]) -> f64 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Dense glass over `behind`, as the backdrop's last pass mixes it
    /// (saturation keeps the luminance; grey stays grey).
    fn dense(tint: &Tint, behind: [f64; 3]) -> [f64; 3] {
        let t = Color {
            a: (tint.dense * 255.0).round() as u8,
            ..tint.color
        };
        over(t, behind)
    }

    /// The background's darkest colour in the light, lightest in the dark
    /// (`backdrop.frag`, its points and bands).
    const WALLPAPER_LIGHT: Color = rgba(0x5d8fe6, 255);
    const WALLPAPER_DARK: Color = rgba(0x8a3f95, 255);

    #[allow(clippy::too_many_arguments)]
    fn check(name: &str, p: &Palette, tint: &Tint, behind: [f64; 3], wallpaper: Color) {
        let glass = dense(tint, behind);
        let fill = over(p.fill, glass);
        let chosen = over(p.fill_chosen, glass);
        let ink = over(p.ink, glass);
        let on_wallpaper = dense(tint, over(wallpaper, [0.0; 3]));
        let pairs = [
            ("ink on glass", contrast(ink, glass), 4.5),
            ("ink on a fill", contrast(over(p.ink, fill), fill), 4.5),
            // A chosen fill carries controls' labels; WCAG's 3:1 for
            // components in the hardest case, far more in any other.
            (
                "ink on a chosen fill",
                contrast(over(p.ink, chosen), chosen),
                3.0,
            ),
            (
                "secondary ink on glass",
                contrast(over(p.ink_secondary, glass), glass),
                3.0,
            ),
            (
                "secondary ink on glass over the background",
                contrast(over(p.ink_secondary, on_wallpaper), on_wallpaper),
                4.5,
            ),
            (
                "danger on glass",
                contrast(over(p.danger, glass), glass),
                4.5,
            ),
            (
                "white on the accent",
                contrast([1.0; 3], over(p.accent, glass)),
                4.5,
            ),
        ];
        for (pair, ratio, least) in pairs {
            assert!(
                ratio >= least,
                "{name}: {pair} {ratio:.2} : 1, below {least} : 1"
            );
        }
    }

    #[test]
    fn every_set_holds_its_contrast_over_the_hardest_case() {
        use super::with_references as w;
        use super::without_references as wo;
        check(
            "ADR light",
            &wo::LIGHT,
            &wo::TINT_LIGHT,
            [0.0; 3],
            WALLPAPER_LIGHT,
        );
        check(
            "ADR dark",
            &wo::DARK,
            &wo::TINT_DARK,
            [1.0; 3],
            WALLPAPER_DARK,
        );
        check(
            "references light",
            &w::LIGHT,
            &w::TINT_LIGHT,
            [0.0; 3],
            WALLPAPER_LIGHT,
        );
        check(
            "references dark",
            &w::DARK,
            &w::TINT_DARK,
            [1.0; 3],
            WALLPAPER_DARK,
        );
    }

    #[test]
    fn radii_follow_outer_is_inner_plus_space() {
        use super::with_references as w;
        use super::without_references as wo;
        for (surface, element, detail) in [
            (wo::RADIUS_SURFACE, wo::RADIUS_ELEMENT, wo::RADIUS_DETAIL),
            (w::RADIUS_SURFACE, w::RADIUS_ELEMENT, w::RADIUS_DETAIL),
        ] {
            assert!(surface > element && element > detail);
            for v in [
                surface,
                element,
                detail,
                surface - element,
                element - detail,
            ] {
                assert_eq!(v % wo::GRID, 0.0, "{v} is off the grid");
            }
        }
        assert_eq!(wo::CAPSULE, 2.0 * wo::RADIUS_ELEMENT);
    }
}
