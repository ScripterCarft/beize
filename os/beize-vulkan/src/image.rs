//! Images: made with memory of their own, and – for the shaders to sample –
//! given a place in the bindless array for as long as they live, destroyed
//! only after the last submission that used them (`device::Garbage`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ash::vk;

use crate::device::{Garbage, Shared};
use crate::error::{Context, Result, error};

/// An image the shaders sample, with its view and its place in the array;
/// the caller writes that place (`Pipelines::write`).
pub struct Sampled {
    shared: Arc<Shared>,
    pub image: vk::Image,
    pub view: vk::ImageView,
    memory: vk::DeviceMemory,
    pub slot: u32,
    /// The submission that last used it.
    last_used: AtomicU64,
}

impl Sampled {
    /// Takes over `image` and `memory`, which the caller made; on failure
    /// they are destroyed. Without alpha the view reads it as 1, so the
    /// image is drawn opaque.
    pub fn new(
        shared: &Arc<Shared>,
        image: vk::Image,
        memory: vk::DeviceMemory,
        format: vk::Format,
        has_alpha: bool,
    ) -> Result<Sampled> {
        let d = &shared.device;
        let destroy = || {
            // SAFETY: never used.
            unsafe {
                d.destroy_image(image, None);
                d.free_memory(memory, None);
            }
        };
        let view = match view(d, image, format, has_alpha) {
            Ok(v) => v,
            Err(e) => {
                destroy();
                return Err(e);
            }
        };
        let slot = match shared.slot() {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: never used.
                unsafe { d.destroy_image_view(view, None) };
                destroy();
                return Err(e);
            }
        };
        Ok(Sampled {
            shared: shared.clone(),
            image,
            view,
            memory,
            slot,
            last_used: AtomicU64::new(0),
        })
    }

    /// Submission `submission` uses it.
    pub fn used(&self, submission: u64) {
        self.last_used.fetch_max(submission, Ordering::Relaxed);
    }
}

impl Drop for Sampled {
    fn drop(&mut self) {
        let after = self.last_used.load(Ordering::Relaxed);
        self.shared.defer(
            after,
            Garbage::Image {
                image: self.image,
                view: self.view,
                memory: self.memory,
            },
        );
        self.shared.defer(after, Garbage::Slot(self.slot));
    }
}

pub fn view(
    d: &ash::Device,
    image: vk::Image,
    format: vk::Format,
    has_alpha: bool,
) -> Result<vk::ImageView> {
    let alpha = if has_alpha {
        vk::ComponentSwizzle::IDENTITY
    } else {
        vk::ComponentSwizzle::ONE
    };
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping {
            r: vk::ComponentSwizzle::IDENTITY,
            g: vk::ComponentSwizzle::IDENTITY,
            b: vk::ComponentSwizzle::IDENTITY,
            a: alpha,
        })
        .subresource_range(COLOR);
    // SAFETY: the image belongs to this device; the info lives until the
    // call returns.
    unsafe { d.create_image_view(&info, None) }.context("image view")
}

pub const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

/// An image with memory of its own, bound.
pub fn own_image(
    shared: &Shared,
    format: vk::Format,
    size: (u32, u32),
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::DeviceMemory)> {
    let d = &shared.device;
    let info = image_info(format, size, usage).tiling(vk::ImageTiling::OPTIMAL);
    // SAFETY: the create info lives until the call returns; on failure the
    // image is destroyed again.
    unsafe {
        let image = d.create_image(&info, None).context("image")?;
        let requirements = d.get_image_memory_requirements(image);
        let memory = shared
            .memory_type(
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .and_then(|kind| {
                d.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirements.size)
                        .memory_type_index(kind),
                    None,
                )
                .context("image memory")
            });
        let memory = match memory {
            Ok(m) => m,
            Err(e) => {
                d.destroy_image(image, None);
                return Err(e);
            }
        };
        if let Err(e) = d.bind_image_memory(image, memory, 0) {
            d.destroy_image(image, None);
            d.free_memory(memory, None);
            return Err(error(format!("binding an image: {e}")));
        }
        Ok((image, memory))
    }
}

pub fn image_info(
    format: vk::Format,
    (width, height): (u32, u32),
    usage: vk::ImageUsageFlags,
) -> vk::ImageCreateInfo<'static> {
    vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .usage(usage)
        .initial_layout(vk::ImageLayout::UNDEFINED)
}
