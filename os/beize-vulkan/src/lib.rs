//! The Vulkan ground Beize draws on (ADR 0027 §2, §9): the device with the
//! features everything is built on, the quad pipelines and their shaders,
//! and images in the bindless texture array. Shared by the compositor and
//! Beize UI, so both draw the same way; neither Smithay nor Wayland is in
//! here – the compositor adds dmabufs, Beize UI its swapchain.

pub mod barrier;
pub mod buffer;
pub mod device;
pub mod error;
pub mod image;
pub mod pipeline;

pub use ash;
pub use error::{Error, Result};
