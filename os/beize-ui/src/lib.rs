//! Beize UI: the toolkit of Beize's own apps (ADR 0027 §9) – windows on
//! Wayland, drawn with Vulkan through beize-vulkan as the compositor draws,
//! and text: the system's fonts, laid out by Parley, rasterized once into
//! the glyph atlas; input from the keyboard and the pointer; the title bar
//! of every window (ADR 0028 §2). Widgets come with the shell (step 4).

mod clipboard;
pub mod draw;
mod glyphs;
pub mod gpu;
pub mod input;
pub mod log;
pub mod offscreen;
mod renderer;
pub mod text;
mod titlebar;
pub mod window;

pub use draw::{Color, Rect, Scene};
pub use input::{Input, Key, KeyState, Keysym, Modifiers};
pub use offscreen::Offscreen;
pub use text::{Cells, Family, Style, Text};
pub use titlebar::Appearance;
pub use window::{Edge, Events, Window};
