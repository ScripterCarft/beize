//! Beize's measures for the shell (ADR 0028 §3, §4): the shell draws its
//! menu bar and dock in them, the compositor lays the scrim and glass below
//! them in the same.
//!
//! macOS' at scale 1, times `SCALE`. The owner (25.10., on his 27"/1440p
//! after 0.5.104): macOS' menu bar looks larger on a Mac, Beize's must be
//! larger at its 100 % too. Physically macOS' 1× is no larger (109 ppi is
//! Apple's 1× and Beize's screen alike), but a desktop screen is further
//! from the eye than a laptop's; `SCALE` is chosen, not measured, judged on
//! screenshots by the owner. The dock grows with the bar, so both stay in
//! proportion. Title bars do not: they follow their buttons (`titlebar`).

/// Beize's shell against macOS at scale 1.
pub const SCALE: f32 = 1.25;

/// A length of macOS at scale 1 in Beize's shell, on whole pixels.
pub const fn scaled(macos: f32) -> u32 {
    (macos * SCALE + 0.5) as u32
}

/// The menu bar's height: as high as a title bar, more than macOS' 24 at
/// `SCALE` – without a background only the space above its text shows, and
/// that looked tight (the owner, 25.10.).
pub const MENU_BAR: u32 = 32;
