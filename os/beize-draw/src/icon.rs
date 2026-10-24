//! The symbols of Beize's own apps (ADR 0028 §8): one form for all, as
//! macOS' icons share theirs – the square of Apple's icon grid with the
//! continuous corners of its mask – filled with a gradient in the app's
//! colours, its motif a symbol of Phosphor's on top. Beize draws its own:
//! Apple's icons and "macOS look" sets stay out (VISION: concepts, not
//! looks). Other apps bring theirs (hicolor).

use crate::draw::{Color, Rect, Scene};

/// The corners' radius, of the side: the mask of Apple's App Icon Template
/// (CLAUDE.md §11).
pub const RADIUS: f32 = 0.2449;
/// The motif's square, of the side: Beize's own choice, so that its ink
/// (the terminal's prompt spans 78 % of Phosphor's square) covers a little
/// over half of the icon.
const MOTIF: f32 = 0.7;

/// How an app's symbol looks.
#[derive(Debug, PartialEq, Eq)]
pub struct AppIcon {
    /// The gradient, top to bottom.
    pub top: Color,
    pub bottom: Color,
    /// Phosphor's symbol (`Scene::symbol`) and its colour.
    pub motif: &'static str,
    pub ink: Color,
}

/// The terminal: a prompt on graphite, as a terminal's window shows it.
const TERMINAL: AppIcon = AppIcon {
    top: Color::rgb(0x4a4a50),
    bottom: Color::rgb(0x1c1c1f),
    motif: "terminal-bold",
    ink: Color::rgb(0xffffff),
};

impl AppIcon {
    /// The symbol of the app `app_id`, if it is one of Beize's.
    pub fn of(app_id: &str) -> Option<&'static AppIcon> {
        match app_id {
            "beize-terminal" => Some(&TERMINAL),
            _ => None,
        }
    }

    /// The symbol in the square `rect`; its motif on whole pixels.
    pub fn draw(&self, scene: &mut Scene, rect: Rect) {
        scene.gradient(rect, rect.w * RADIUS, self.top, self.bottom);
        let size = (rect.w * MOTIF).round();
        scene.symbol(
            self.motif,
            (rect.x + (rect.w - size) / 2.0).round(),
            (rect.y + (rect.h - size) / 2.0).round(),
            size as u32,
            self.ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_beizes_apps_have_one() {
        assert_eq!(
            AppIcon::of("beize-terminal").map(|i| i.motif),
            Some("terminal-bold")
        );
        assert_eq!(AppIcon::of("org.example.App"), None);
    }

    #[test]
    fn the_form_then_the_motif() {
        let mut scene = Scene::new();
        TERMINAL.draw(
            &mut scene,
            Rect {
                x: 14.6875,
                y: 14.6875,
                w: 38.625,
                h: 38.625,
            },
        );
        assert_eq!(scene.len(), 2);
        assert!(scene.has_glyphs());
    }
}
