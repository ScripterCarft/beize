//! Beize UI: the toolkit of Beize's own apps (ADR 0027 §9) – windows on
//! Wayland, drawn with Vulkan through beize-vulkan, as the compositor
//! draws. Text and input follow; widgets come with the shell (step 4).

pub mod draw;
pub mod gpu;
pub mod window;

pub use draw::{Color, Rect, Scene};
pub use window::{Events, Window};
