//! What a frame draws into. A screen image is a dmabuf: from the GPU's own
//! Vulkan driver (allocated by Smithay's `VulkanAllocator`, imported with
//! its modifier), or a dumb buffer of the KMS driver that lavapipe draws
//! into (linear, ADR 0027 §6). Where a dumb buffer is slow to read, or its
//! row pitch is not lavapipe's, lavapipe draws into an image of its own
//! and the frame is copied over (`Readback`). Offscreen targets are images
//! of the renderer's own (effects, screenshots, the render test).

use std::sync::Arc;

use smithay::backend::allocator::dmabuf::{
    Dmabuf, DmabufMapping, DmabufMappingMode, DmabufSyncFlags,
};
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::backend::renderer::Texture;
use smithay::reexports::ash::vk;
use smithay::utils::{Buffer, Size};

use super::VkRenderer;
use super::device::{Garbage, Shared};
use super::texture::{self, vk_format};
use crate::error::{Context, Result, error};

const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);

pub struct Target {
    pub(super) shared: Arc<Shared>,
    pub(super) image: vk::Image,
    pub(super) view: vk::ImageView,
    memory: vk::DeviceMemory,
    pub(super) readback: Option<Readback>,
    pub(super) size: Size<i32, Buffer>,
    pub(super) format: vk::Format,
    fourcc: Fourcc,
    /// The screen's dmabuf when drawn into directly: owned by the foreign
    /// queue (KMS) between frames, our write added to it.
    pub(super) dmabuf: Option<Dmabuf>,
    /// Holds a frame: its content is kept (damage) rather than discarded.
    pub(super) filled: bool,
    pub(super) last_used: u64,
}

impl Target {
    /// Whether frames reach the screen through a CPU copy.
    pub fn copies(&self) -> bool {
        self.readback.is_some()
    }
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("size", &self.size)
            .field("format", &self.fourcc)
            .field("copies", &self.copies())
            .finish()
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.shared.defer(
            self.last_used,
            Garbage::Image {
                image: self.image,
                view: self.view,
                memory: self.memory,
            },
        );
    }
}

/// A target as Smithay's framebuffer, for the length of a frame.
#[derive(Debug)]
pub struct VkFramebuffer<'a>(pub(super) &'a mut Target);

impl Texture for VkFramebuffer<'_> {
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

impl VkRenderer {
    fn target(
        &self,
        image: vk::Image,
        memory: vk::DeviceMemory,
        fourcc: Fourcc,
        size: Size<i32, Buffer>,
    ) -> Result<Target> {
        let d = &self.shared.device;
        let (format, _) = vk_format(fourcc).context(format!("format {fourcc} is not drawn"))?;
        // A target is written whole, alpha included.
        let view = match texture::view(d, image, format, true) {
            Ok(v) => v,
            Err(e) => {
                // SAFETY: never used.
                unsafe {
                    d.destroy_image(image, None);
                    d.free_memory(memory, None);
                }
                return Err(e);
            }
        };
        Ok(Target {
            shared: self.shared.clone(),
            image,
            view,
            memory,
            readback: None,
            size,
            format,
            fourcc,
            dmabuf: None,
            filled: false,
            last_used: 0,
        })
    }

    /// The screen's dmabuf itself as the target: drawn into without a copy.
    pub fn import_target(&self, dmabuf: &Dmabuf) -> Result<Target> {
        let fourcc = opaque(dmabuf.format().code);
        let (format, _) = vk_format(fourcc).context(format!("format {fourcc} is not drawn"))?;
        let (image, memory) = texture::import_image(
            &self.shared,
            &self.ext,
            self.caps.modifiers,
            dmabuf,
            format,
            USAGE,
        )?;
        let size = (dmabuf.width() as i32, dmabuf.height() as i32).into();
        let mut target = self.target(image, memory, fourcc, size)?;
        target.dmabuf = Some(dmabuf.clone());
        Ok(target)
    }

    /// An image of lavapipe's own, copied into `dmabuf` after each frame.
    pub fn shadow_target(&self, dmabuf: &Dmabuf) -> Result<Target> {
        // The copy goes row by row: a tiled buffer would show a scramble.
        let modifier = dmabuf.format().modifier;
        if modifier != Modifier::Linear {
            return Err(error(format!(
                "the screen buffer is not linear ({modifier:?}): rows cannot be copied into it"
            )));
        }
        let size = (dmabuf.width() as i32, dmabuf.height() as i32).into();
        let mut target = self.offscreen_target(opaque(dmabuf.format().code), size)?;
        target.readback = Some(Readback::new(self, dmabuf, size)?);
        Ok(target)
    }

    /// An image of the renderer's own to draw into.
    pub fn offscreen_target(&self, fourcc: Fourcc, size: Size<i32, Buffer>) -> Result<Target> {
        let (format, _) = vk_format(fourcc).context(format!("format {fourcc} is not drawn"))?;
        if size.w <= 0 || size.h <= 0 {
            return Err(error("an empty target"));
        }
        let (image, memory) =
            texture::own_image(&self.shared, format, (size.w as u32, size.h as u32), USAGE)?;
        self.target(image, memory, fourcc, size)
    }
}

/// The screen shows no alpha: ARGB buffers are drawn as XRGB (as Smithay's
/// use_opaque).
fn opaque(code: Fourcc) -> Fourcc {
    match code {
        Fourcc::Argb8888 => Fourcc::Xrgb8888,
        Fourcc::Abgr8888 => Fourcc::Xbgr8888,
        code => code,
    }
}

/// Host memory the shadow image is copied to, and the dmabuf it goes to.
pub(super) struct Readback {
    shared: Arc<Shared>,
    pub(super) buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pointer: *const u8,
    dmabuf: Dmabuf,
    mapping: DmabufMapping,
    stride: usize,
}

impl Readback {
    fn new(r: &VkRenderer, dmabuf: &Dmabuf, size: Size<i32, Buffer>) -> Result<Readback> {
        let (buffer, memory, pointer) = r.host_buffer(
            size.w as u64 * size.h as u64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
            true,
        )?;
        let readback = Readback {
            shared: r.shared.clone(),
            buffer,
            memory,
            pointer,
            dmabuf: dmabuf.clone(),
            mapping: dmabuf
                .map_plane(0, DmabufMappingMode::WRITE)
                .context("mapping the screen buffer")?,
            stride: dmabuf.strides().next().context("dmabuf without a plane")? as usize,
        };
        Ok(readback)
    }

    /// Copies the frame in host memory to the screen buffer.
    pub(super) fn copy_out(&self, size: Size<i32, Buffer>) -> Result<()> {
        let row = size.w as usize * 4;
        let height = size.h as usize;
        if self.stride < row || self.mapping.length() < self.stride * height {
            return Err(error("the screen buffer is smaller than the image"));
        }
        self.dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
            .context("dmabuf sync")?;
        let target = self.mapping.ptr() as *mut u8;
        for y in 0..height {
            // SAFETY: both rows lie within their mappings (sizes checked
            // above and at allocation: width * height * 4 bytes), which do
            // not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.pointer.add(y * row),
                    target.add(y * self.stride),
                    row,
                );
            }
        }
        self.dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
            .context("dmabuf sync")?;
        Ok(())
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        // Copies are waited for before they are read: nothing is pending.
        self.shared.defer(
            0,
            Garbage::Buffer {
                buffer: self.buffer,
                memory: self.memory,
            },
        );
    }
}
