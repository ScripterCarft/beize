//! Textures: images of the renderer's own (shm and memory buffers, filled
//! through staging) or a client's dmabuf, imported. Each is a
//! `beize_vulkan::image::Sampled`: a place in the bindless array for as
//! long as it lives, destroyed only after the last frame that drew it.

use std::fmt;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use smithay::backend::allocator::Buffer as _;
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::{Dmabuf, WeakDmabuf};
use smithay::backend::renderer::Texture;
use smithay::reexports::ash::vk;
use smithay::utils::{Buffer, Size};

use beize_vulkan::image::{Sampled, image_info};

use super::device::{Ext, Shared};
use crate::error::{Context, Result, error};

/// Vulkan's format for a DRM format, and whether it has alpha: the
/// XRGB/XBGR ones read their alpha as 1 (a swizzle in the view), so a
/// texture without alpha is drawn opaque.
pub fn vk_format(fourcc: Fourcc) -> Option<(vk::Format, bool)> {
    match fourcc {
        Fourcc::Argb8888 => Some((vk::Format::B8G8R8A8_UNORM, true)),
        Fourcc::Xrgb8888 => Some((vk::Format::B8G8R8A8_UNORM, false)),
        Fourcc::Abgr8888 => Some((vk::Format::R8G8B8A8_UNORM, true)),
        Fourcc::Xbgr8888 => Some((vk::Format::R8G8B8A8_UNORM, false)),
        _ => None,
    }
}

/// The formats memory and dmabufs are taken in.
pub const FORMATS: [Fourcc; 4] = [
    Fourcc::Argb8888,
    Fourcc::Xrgb8888,
    Fourcc::Abgr8888,
    Fourcc::Xbgr8888,
];

#[derive(Clone)]
pub struct VkTexture(pub(super) Arc<Inner>);

pub(super) struct Inner {
    pub gpu: Sampled,
    pub size: Size<i32, Buffer>,
    pub fourcc: Fourcc,
    pub has_alpha: bool,
    /// A client's dmabuf: owned by the foreign queue between frames, its
    /// writes waited for, our reads added to it.
    pub dmabuf: Option<WeakDmabuf>,
    /// Holds content (an own image after its first upload).
    pub filled: AtomicBool,
}

impl VkTexture {
    pub(super) fn used(&self, submission: u64) {
        self.0.gpu.used(submission);
    }

    pub fn has_alpha(&self) -> bool {
        self.0.has_alpha
    }
}

impl fmt::Debug for VkTexture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VkTexture")
            .field("slot", &self.0.gpu.slot)
            .field("size", &self.0.size)
            .field("format", &self.0.fourcc)
            .field("dmabuf", &self.0.dmabuf.is_some())
            .finish()
    }
}

impl Texture for VkTexture {
    fn width(&self) -> u32 {
        self.0.size.w as u32
    }
    fn height(&self) -> u32 {
        self.0.size.h as u32
    }
    fn format(&self) -> Option<Fourcc> {
        Some(self.0.fourcc)
    }
}

/// A dmabuf as a Vulkan image: with its modifier where the device has
/// format modifiers, otherwise only linear with Vulkan's own row pitch.
pub fn import_image(
    shared: &Shared,
    ext: &Ext,
    modifiers: bool,
    dmabuf: &Dmabuf,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::DeviceMemory)> {
    let d = &shared.device;
    if dmabuf.num_planes() != 1 {
        return Err(error("only dmabufs with one plane"));
    }
    let modifier: u64 = dmabuf.format().modifier.into();
    let stride = dmabuf.strides().next().context("dmabuf without a plane")?;
    let offset = dmabuf.offsets().next().context("dmabuf without a plane")?;
    let layouts = [vk::SubresourceLayout {
        offset: offset.into(),
        row_pitch: stride.into(),
        ..Default::default()
    }];
    let mut external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(&layouts);
    let size = (dmabuf.width(), dmabuf.height());
    let mut info = image_info(format, size, usage).push_next(&mut external);
    if modifiers {
        info = info
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .push_next(&mut explicit);
    } else {
        let linear: u64 = smithay::backend::allocator::Modifier::Linear.into();
        let invalid: u64 = smithay::backend::allocator::Modifier::Invalid.into();
        if modifier != linear && modifier != invalid {
            return Err(error("without format modifiers only linear dmabufs"));
        }
        info = info.tiling(vk::ImageTiling::LINEAR);
    }
    // SAFETY: the create info and its chain live until the call returns.
    let image = unsafe { d.create_image(&info, None) }.context("importing a dmabuf")?;
    let result = bind_dmabuf(shared, ext, modifiers, dmabuf, image, stride, offset);
    match result {
        Ok(memory) => Ok((image, memory)),
        Err(e) => {
            // SAFETY: never used.
            unsafe { d.destroy_image(image, None) };
            Err(e)
        }
    }
}

fn bind_dmabuf(
    shared: &Shared,
    ext: &Ext,
    modifiers: bool,
    dmabuf: &Dmabuf,
    image: vk::Image,
    stride: u32,
    offset: u32,
) -> Result<vk::DeviceMemory> {
    let d = &shared.device;
    if !modifiers {
        let subresource = vk::ImageSubresource {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            array_layer: 0,
        };
        // SAFETY: a linear image of this device.
        let layout = unsafe { d.get_image_subresource_layout(image, subresource) };
        if layout.row_pitch != u64::from(stride) || offset != 0 {
            return Err(error(format!(
                "row pitch {stride} of the dmabuf is not Vulkan's {}",
                layout.row_pitch
            )));
        }
    }
    let fd = dmabuf
        .handles()
        .next()
        .context("dmabuf without a plane")?
        .try_clone_to_owned()
        .context("duplicating the dmabuf")?;
    let mut properties = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: fd is an open dmabuf; the call only reads it.
    unsafe {
        ext.memory_fd.get_memory_fd_properties(
            vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            fd.as_raw_fd(),
            &mut properties,
        )
    }
    .context("the dmabuf's memory types")?;
    // SAFETY: the image belongs to this device.
    let requirements = unsafe { d.get_image_memory_requirements(image) };
    let kind = shared.memory_type(
        requirements.memory_type_bits & properties.memory_type_bits,
        vk::MemoryPropertyFlags::empty(),
    )?;
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd.into_raw_fd());
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let allocate = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(kind)
        .push_next(&mut import)
        .push_next(&mut dedicated);
    // SAFETY: on success Vulkan owns the fd; on failure it stays open, once
    // (VK_KHR_external_memory_fd passes ownership only on success).
    let memory =
        unsafe { d.allocate_memory(&allocate, None) }.context("importing the dmabuf's memory")?;
    // SAFETY: memory was allocated for this image.
    if let Err(e) = unsafe { d.bind_image_memory(image, memory, 0) } {
        // SAFETY: never used.
        unsafe { d.free_memory(memory, None) };
        return Err(error(format!("binding the dmabuf: {e}")));
    }
    Ok(memory)
}

/// A new texture around an image the caller made; its place in the array
/// is written by the caller.
pub(super) fn wrap(
    shared: &Arc<Shared>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    fourcc: Fourcc,
    size: Size<i32, Buffer>,
    dmabuf: Option<WeakDmabuf>,
) -> Result<VkTexture> {
    let Some((format, has_alpha)) = vk_format(fourcc) else {
        // SAFETY: never used.
        unsafe {
            shared.device.destroy_image(image, None);
            shared.device.free_memory(memory, None);
        }
        return Err(error(format!("format {fourcc} is not taken")));
    };
    let gpu = Sampled::new(shared, image, memory, format, has_alpha)?;
    let filled = dmabuf.is_some();
    Ok(VkTexture(Arc::new(Inner {
        gpu,
        size,
        fourcc,
        has_alpha,
        dmabuf,
        filled: AtomicBool::new(filled),
    })))
}
