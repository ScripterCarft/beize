//! What Beize's system surfaces have in common: menus, dialogs and Alt+Tab take
//! their ink, accent and text from here, so they look like one system (the
//! owner, 31.10.).

use beize_draw::Color;

use crate::Appearance;

/// The text in Apple's menus (#292929, measured) and on dark ones.
pub const INK_LIGHT: Color = Color::rgb(0x292929);
pub const INK_DARK: Color = Color::rgb(0xe8e8e8);
/// macOS' accent blue: a selected row, a default button, Alt+Tab's selected
/// tile, each with `ON_ACCENT` on it.
pub const ACCENT: Color = Color::rgb(0x007aff);
pub const ON_ACCENT: Color = Color::rgb(0xffffff);
/// The text of menus and dialogs: the menu bar's size (the owner, 25.10.).
pub const TEXT_SIZE: f32 = 15.0;
/// The text below a window's picture in Alt+Tab.
pub const SMALL_TEXT_SIZE: f32 = 13.0;

/// The ink on glass of `appearance`.
pub const fn ink(appearance: Appearance) -> Color {
    match appearance {
        Appearance::Light => INK_LIGHT,
        Appearance::Dark => INK_DARK,
    }
}

/// `color` showing `alpha` of itself.
pub const fn faded(color: Color, alpha: u8) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: alpha,
    }
}
