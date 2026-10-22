//! Beize UI: the toolkit of Beize's own apps (ADR 0027 §9) – windows on
//! Wayland, drawn with Vulkan through beize-vulkan as the compositor draws,
//! and text: the system's fonts, laid out by Parley, rasterized once into
//! the glyph atlas. Input follows; widgets come with the shell (step 4).

pub mod draw;
mod glyphs;
pub mod gpu;
pub mod offscreen;
mod renderer;
pub mod text;
pub mod window;

pub use draw::{Color, Rect, Scene};
pub use offscreen::Offscreen;
pub use text::{Family, Style, Text};
pub use window::{Events, Window};
