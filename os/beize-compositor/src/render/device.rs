//! The renderer's device: beize-vulkan's on Smithay's physical device, with
//! what a compositor needs on top – dmabufs in and out, and their fences as
//! sync files.

use smithay::backend::vulkan::PhysicalDevice;
use smithay::reexports::ash::{self, vk};

pub use beize_vulkan::device::{Garbage, Shared};

use crate::error::Result;

/// What the device offers beyond the required.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// `VK_EXT_image_drm_format_modifier`: dmabufs with any modifier; without
    /// it (lavapipe without udmabuf) linear ones with Vulkan's row pitch.
    pub modifiers: bool,
    /// Nanoseconds per timestamp tick, if the queue has timestamps.
    pub timestamp_period: Option<f32>,
}

/// The device's extension functions the renderer calls.
pub struct Ext {
    pub memory_fd: ash::khr::external_memory_fd::Device,
    pub fence_fd: ash::khr::external_fence_fd::Device,
    pub semaphore_fd: ash::khr::external_semaphore_fd::Device,
    /// `VK_EXT_calibrated_timestamps`: when the GPU worked on a frame, on
    /// the clock of KMS and the fences (`VkRenderer::gpu_time`).
    pub calibrated: Option<ash::ext::calibrated_timestamps::Device>,
}

pub struct Created {
    pub shared: std::sync::Arc<Shared>,
    pub queue: vk::Queue,
    pub family: u32,
    pub caps: Caps,
    pub ext: Ext,
}

const REQUIRED: [&std::ffi::CStr; 5] = [
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
    ash::khr::external_fence_fd::NAME,
    ash::khr::external_semaphore_fd::NAME,
    ash::ext::queue_family_foreign::NAME,
];

pub fn create(phd: &PhysicalDevice) -> Result<Created> {
    let instance = phd.instance().handle();
    let extensions = beize_vulkan::device::Extensions {
        required: &REQUIRED,
        optional: &[
            ash::ext::image_drm_format_modifier::NAME,
            ash::ext::calibrated_timestamps::NAME,
        ],
    };
    // SAFETY: the physical device belongs to Smithay's instance, and the
    // device keeps a clone of it (reference-counted).
    let created = unsafe {
        beize_vulkan::device::create(
            instance,
            phd.handle(),
            Box::new(phd.instance().clone()),
            &extensions,
        )
    }?;
    let device = &created.shared.device;
    let ext = Ext {
        memory_fd: ash::khr::external_memory_fd::Device::new(instance, device),
        fence_fd: ash::khr::external_fence_fd::Device::new(instance, device),
        semaphore_fd: ash::khr::external_semaphore_fd::Device::new(instance, device),
        calibrated: (created.optional[1] && created.timestamp_period.is_some())
            .then(|| ash::ext::calibrated_timestamps::Device::new(instance, device)),
    };
    Ok(Created {
        caps: Caps {
            modifiers: created.optional[0],
            timestamp_period: created.timestamp_period,
        },
        shared: created.shared,
        queue: created.queue,
        family: created.family,
        ext,
    })
}
