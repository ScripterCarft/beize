//! What Beize UI and the compositor draw alike (ADR 0028 §2): a scene of
//! quads for beize-vulkan's pipelines; text – the system's fonts, laid out
//! by Parley – and symbols, rasterized once into the glyph atlas; the
//! symbols of Beize's apps; the title bar, which Beize UI draws on its
//! windows and the compositor on those of other apps that ask it for their
//! frame; the shell's measures; and the springs everything moves on. One crate, so both look the same to the
//! pixel. Neither Wayland nor the GPU is in here: each side uploads the
//! atlas and draws the quads itself.

pub mod draw;
pub mod glyphs;
#[cfg(feature = "icons")]
pub mod icon;
pub mod measures;
pub mod spring;
pub mod text;
pub mod titlebar;
pub mod tokens;

pub use draw::{Atlases, Cap, Color, Rect, Scene};
pub use glyphs::Glyphs;
#[cfg(feature = "icons")]
pub use icon::AppIcon;
pub use spring::{Motion, Spring};
pub use text::{Cells, Family, Style, Text};
pub use titlebar::{Appearance, TitleBar};
