//! Beize UI: the toolkit of Beize's own apps (ADR 0027 §9) – windows on
//! Wayland, drawn with Vulkan through beize-vulkan as the compositor draws;
//! text, symbols and the title bar of every window (ADR 0028 §2) from
//! beize-draw, which the compositor shares; input from the keyboard and
//! the pointer. Widgets come with the shell (step 4).

mod app;
mod clipboard;
mod desktop;
pub mod gpu;
pub mod input;
pub mod log;
pub mod offscreen;
mod renderer;
mod window;

pub use app::{App, Dispatched, Edge, Events, Panel, WindowId};
pub use beize_draw::{Appearance, Cells, Color, Family, Rect, Scene, Style, Text, draw, text};
pub use desktop::{DesktopWindow, DesktopWindowId};
pub use input::{Input, Key, KeyState, Keysym, Modifiers};
pub use offscreen::Offscreen;
