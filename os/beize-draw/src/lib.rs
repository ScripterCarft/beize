//! What Beize UI and the compositor draw alike (ADR 0028 §2): a scene of
//! quads for beize-vulkan's pipelines; text – the system's fonts, laid out
//! by Parley – and symbols, rasterized once into the glyph atlas; the
//! symbols of Beize's apps; the title bar, which Beize UI draws on its
//! windows and the compositor on those of other apps that ask it for their
//! frame; and the shell's measures. One crate, so both look the same to the
//! pixel. Neither Wayland nor the GPU is in here: each side uploads the
//! atlas and draws the quads itself.

pub mod draw;
pub mod glyphs;
pub mod icon;
pub mod measures;
pub mod text;
pub mod titlebar;

pub use draw::{Atlases, Color, Rect, Scene};
pub use glyphs::Glyphs;
pub use icon::AppIcon;
pub use text::{Cells, Family, Style, Text};
pub use titlebar::{Appearance, TitleBar};
