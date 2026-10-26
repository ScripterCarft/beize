//! Beize's renderer: Vulkan only (ADR 0027 §2), behind Smithay's `Renderer`
//! so its damage tracking and render elements work on it.
//!
//! Built for doing little: a frame draws only its damage, each damaged
//! piece of a surface one quad, all quads of a frame in one buffer and few
//! draws (bindless textures, `pipeline`). Two submissions are in flight; the
//! CPU waits for the GPU only when it is that far behind (`current`, the
//! `wait` of a frame's parts). A frame's end goes to KMS and into the
//! dmabufs it touched as a sync_file (`sync`). lavapipe's copy to a dumb
//! buffer waits, as the CPU has to copy.

mod device;
mod frame;
pub mod geometry;
mod sync;
mod target;
mod texture;

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::{Dmabuf, WeakDmabuf};
use smithay::backend::allocator::{Buffer as _, Format, Fourcc, Modifier};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, ContextId, DebugFlags, ExportMem, ImportDma, ImportDmaWl, ImportMem, ImportMemWl,
    Offscreen, Renderer, RendererSuper, Texture, TextureFilter, TextureMapping,
};
use smithay::backend::vulkan::PhysicalDevice;
use smithay::reexports::ash::vk;
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::SurfaceData;
use tracing::{debug, info, warn};

use crate::error::{Context, Error, Result, error};
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::pipeline::{Pipelines, Quad};
use device::{Caps, Ext, Garbage, Shared};
pub use frame::VkFrame;
pub use sync::signalled_at;
pub use target::{Target, VkFramebuffer};
pub use texture::VkTexture;

/// Frames recorded or on the GPU at once.
const IN_FLIGHT: usize = 2;

/// What one frame in flight owns.
struct InFlight {
    commands: vk::CommandBuffer,
    /// Exported as the frame's sync_file (and reset by that).
    fence: vk::Fence,
    quads: Option<HostBuffer>,
    staging: Option<HostBuffer>,
    /// Bytes of `staging` taken by uploads for this frame.
    staged: u64,
    /// GPU time: start and end.
    queries: Option<vk::QueryPool>,
    /// The submission that last used this frame's objects.
    submission: u64,
    /// Ready for this frame: its last submission waited for.
    ready: bool,
}

/// A copy from staging into a texture, recorded with the next submission.
struct Upload {
    texture: VkTexture,
    regions: Vec<vk::BufferImageCopy>,
}

/// A picture the compositor draws for itself (`draw_picture`, the
/// backdrop), recorded with the next submission ahead of its frame, in
/// order: a pass may read what one before it drew.
struct Pass {
    texture: VkTexture,
    quads: Vec<Quad>,
    blend: Vec<bool>,
    /// Drawn with the backdrop's shader (`beize_vulkan::pipeline`).
    backdrop: bool,
    /// What it reads, kept until the GPU is done with it.
    reads: Option<VkTexture>,
}

/// How the base glass is made of the backdrop (ADR 0028 §6): blurred by
/// Dual Kawase `down` halvings and `up` doublings back (the glass keeps
/// 1/2^(down - up) of the size: it is soft, and the frame's filter scales
/// it up), the taps `offset` texels apart; then its saturation times
/// `saturation`, and `tint` (linear, premultiplied) over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub down: u32,
    pub up: u32,
    pub offset: f32,
    pub saturation: f32,
    pub tint: [f32; 4],
}

/// Where the CPU's time went in the submissions since it was last taken
/// (`VkRenderer::take_spent`): waiting for the GPU to give back a frame's
/// objects (or for lavapipe's copy), recording the commands, the fences of
/// the clients' dmabufs and of the frame (sync_file in and out), and the
/// submission to the queue.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spent {
    pub wait: Duration,
    pub record: Duration,
    pub sync: Duration,
    pub queue: Duration,
}

/// A picture's format: read as UNORM, drawn through an sRGB view.
const PICTURE: vk::Format = vk::Format::R8G8B8A8_UNORM;
const PICTURE_DRAWN: vk::Format = vk::Format::R8G8B8A8_SRGB;
/// Where each list of quads starts in a frame's buffer, at least.
const QUADS_ALIGN: usize = 256;

pub struct VkRenderer {
    shared: Arc<Shared>,
    queue: vk::Queue,
    family: u32,
    pool: vk::CommandPool,
    caps: Caps,
    ext: Ext,
    pipelines: Pipelines,
    in_flight: Vec<InFlight>,
    current: usize,
    submitted: u64,
    uploads: Vec<Upload>,
    passes: Vec<Pass>,
    dmabufs: HashMap<WeakDmabuf, VkTexture>,
    dmabuf_formats: smithay::backend::allocator::format::FormatSet,
    context: ContextId<VkTexture>,
    debug: DebugFlags,
    name: String,
    spent: Spent,
}

impl std::fmt::Debug for VkRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkRenderer")
            .field("device", &self.name)
            .finish()
    }
}

impl VkRenderer {
    pub fn new(phd: &PhysicalDevice) -> Result<VkRenderer> {
        let created = device::create(phd)?;
        let shared = created.shared;
        let d = &shared.device;
        let pipelines = Pipelines::new(d, shared.max_textures())?;
        // SAFETY: plain creation; destroyed in Drop.
        let pool = unsafe {
            d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(created.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .context("command pool")?;
        let mut renderer = VkRenderer {
            shared: shared.clone(),
            queue: created.queue,
            family: created.family,
            pool,
            caps: created.caps,
            ext: created.ext,
            pipelines,
            in_flight: Vec::new(),
            current: 0,
            submitted: 0,
            uploads: Vec::new(),
            passes: Vec::new(),
            dmabufs: HashMap::new(),
            dmabuf_formats: Default::default(),
            context: ContextId::new(),
            debug: DebugFlags::empty(),
            name: phd.name().to_owned(),
            spent: Spent::default(),
        };
        for _ in 0..IN_FLIGHT {
            let frame = renderer.in_flight_new()?;
            renderer.in_flight.push(frame);
        }
        renderer.dmabuf_formats = renderer.query_dmabuf_formats(phd);
        info!(
            "renderer on {}: {} textures, dmabufs {}",
            renderer.name,
            shared.max_textures(),
            if renderer.caps.modifiers {
                "with format modifiers"
            } else {
                "linear only"
            }
        );
        Ok(renderer)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The clients' dmabufs it holds, and the textures in use: both stay
    /// flat while windows come, go and change size (a leak shows here).
    pub fn held(&self) -> (usize, u32) {
        (self.dmabufs.len(), self.shared.textures_in_use())
    }

    /// Where the CPU's time went in the submissions since the last call.
    pub fn take_spent(&mut self) -> Spent {
        std::mem::take(&mut self.spent)
    }

    /// The number of the last submission: a frame's, right after it.
    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    /// How long the GPU worked on `submission`, in milliseconds: once it is
    /// done (a frame on the screen is), while its frame's objects are not
    /// yet used again. None without timestamps.
    pub fn gpu_ms(&self, submission: u64) -> Option<f64> {
        let frame = self.in_flight.iter().find(|f| f.submission == submission)?;
        let (pool, period) = (frame.queries?, self.caps.timestamp_period?);
        let mut times = [0u64; 2];
        // SAFETY: the pool belongs to this device; without WAIT the call
        // only reports NOT_READY for a submission still running.
        let read = unsafe {
            self.shared.device.get_query_pool_results(
                pool,
                0,
                &mut times,
                vk::QueryResultFlags::TYPE_64,
            )
        };
        (read.is_ok() && times[1] > times[0])
            .then(|| (times[1] - times[0]) as f64 * f64::from(period) / 1e6)
    }

    /// An empty 8-bit texture of `size` × `size` for coverage: the glyph
    /// atlas of the compositor's title bars (beize-draw), as Beize UI keeps
    /// its own.
    pub fn coverage_texture(&mut self, size: u32) -> Result<VkTexture> {
        let empty = vec![0; size as usize * size as usize];
        let side = size as i32;
        self.memory_texture(&empty, size as usize, Fourcc::R8, (side, side).into())
    }

    /// Writes `coverage`, the texels of `rect` tightly packed, into the
    /// coverage texture `texture`, with the next submission.
    pub fn write_coverage(
        &mut self,
        texture: &VkTexture,
        rect: Rectangle<i32, Buffer>,
        coverage: &[u8],
    ) -> Result<()> {
        if !Rectangle::from_size(texture.0.size).contains_rect(rect) {
            return Err(error("coverage outside its texture"));
        }
        let copy = self.stage_rect(texture, coverage, rect.size.w as usize, (0, 0).into(), rect)?;
        self.queue_copies(texture, vec![copy]);
        Ok(())
    }

    /// A picture of `size` drawn from beize-draw's quads (`blend` per quad),
    /// as Beize UI draws its windows: through an sRGB view, so it blends in
    /// linear light (ADR 0027 §9), and read as it is, as the compositor
    /// reads a Beize UI window's buffer – the same pixels. Into `into` if
    /// it is a picture of that size, else into a new one. Drawn with the
    /// next submission, ahead of its frame.
    pub fn draw_picture(
        &mut self,
        size: Size<i32, Buffer>,
        quads: Vec<Quad>,
        blend: Vec<bool>,
        into: Option<VkTexture>,
    ) -> Result<VkTexture> {
        let texture = match into {
            Some(t) if t.0.size == size && t.0.attachment.is_some() => t,
            _ => self.picture_texture(size)?,
        };
        self.passes
            .retain(|p| !Arc::ptr_eq(&p.texture.0, &texture.0));
        self.passes.push(Pass {
            texture: texture.clone(),
            quads,
            blend,
            backdrop: false,
            reads: None,
        });
        Ok(texture)
    }

    /// Beize's background for a screen of `size`, in `appearance` (0
    /// light, 1 dark): computed by the backdrop's shader (ADR 0028 §6), no
    /// image file. Drawn with the next submission, ahead of its frame.
    pub fn draw_wallpaper(
        &mut self,
        size: Size<i32, Buffer>,
        appearance: u32,
    ) -> Result<VkTexture> {
        let texture = self.picture_texture(size)?;
        let mut quad = pass_quad(size, None);
        quad.flags = beize_vulkan::pipeline::WALLPAPER;
        quad.texture = appearance;
        quad.clip = [0.0, 0.0, size.w as f32, size.h as f32];
        self.backdrop_pass(&texture, quad, None);
        Ok(texture)
    }

    /// The base glass of `source` (`Material`): drawn with the next
    /// submission, after what draws `source`. Into the pictures of `chain`
    /// where they have the step's size (live glass keeps them from one
    /// change to the next), else into new ones, which `chain` keeps.
    pub fn draw_glass(
        &mut self,
        source: &VkTexture,
        material: Material,
        chain: &mut Vec<VkTexture>,
    ) -> Result<VkTexture> {
        self.prepare_glass(source.0.size, material, chain)?;
        // Drawn again in this submission: the passes queued before for these
        // pictures are superseded.
        self.passes
            .retain(|p| !chain.iter().any(|t| Arc::ptr_eq(&p.texture.0, &t.0)));
        let passes = glass_passes(source, material, chain)?;
        let glass = passes
            .last()
            .map(|p| p.texture.clone())
            .context("glass without passes")?;
        self.passes.extend(passes);
        Ok(glass)
    }

    /// The pictures the glass of a `size` source is drawn through (`Material`),
    /// in `chain`: kept where they have the step's size, else made.
    pub fn prepare_glass(
        &mut self,
        size: Size<i32, Buffer>,
        material: Material,
        chain: &mut Vec<VkTexture>,
    ) -> Result<()> {
        let sizes = glass_sizes(size, material)?;
        for (step, &size) in sizes.iter().enumerate() {
            let fits = chain
                .get(step)
                .is_some_and(|t| t.0.size == size && t.0.attachment.is_some());
            if !fits {
                chain.truncate(step);
                chain.push(self.picture_texture(size)?);
            }
        }
        chain.truncate(sizes.len());
        Ok(())
    }

    /// An image in the format of a frame's target, `size` large, that a
    /// part of the frame is copied into and read from (live glass).
    fn copy_texture_for(&mut self, target: &Target, size: Size<i32, Buffer>) -> Result<VkTexture> {
        let (image, memory) = beize_vulkan::image::own_image(
            &self.shared,
            target.format,
            (size.w as u32, size.h as u32),
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        let texture = texture::wrap(
            &self.shared,
            image,
            memory,
            target.fourcc(),
            size,
            None,
            None,
        )?;
        self.pipelines
            .write(&self.shared.device, texture.0.gpu.slot, texture.0.gpu.view);
        Ok(texture)
    }

    fn backdrop_pass(&mut self, texture: &VkTexture, quad: Quad, reads: Option<VkTexture>) {
        self.passes.push(Pass {
            texture: texture.clone(),
            quads: vec![quad],
            blend: vec![false],
            backdrop: true,
            reads,
        });
    }

    fn picture_texture(&mut self, size: Size<i32, Buffer>) -> Result<VkTexture> {
        if size.w <= 0 || size.h <= 0 {
            return Err(error("an empty picture"));
        }
        let (image, memory) = beize_vulkan::image::own_image_with(
            &self.shared,
            PICTURE,
            (size.w as u32, size.h as u32),
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::ImageCreateFlags::MUTABLE_FORMAT,
        )?;
        let d = &self.shared.device;
        let attachment = match beize_vulkan::image::view(d, image, PICTURE_DRAWN, true) {
            Ok(view) => view,
            Err(e) => {
                // SAFETY: never used.
                unsafe {
                    d.destroy_image(image, None);
                    d.free_memory(memory, None);
                }
                return Err(e);
            }
        };
        let texture = texture::wrap(
            &self.shared,
            image,
            memory,
            Fourcc::Abgr8888,
            size,
            None,
            Some(attachment),
        )?;
        self.pipelines
            .write(&self.shared.device, texture.0.gpu.slot, texture.0.gpu.view);
        Ok(texture)
    }

    fn in_flight_new(&self) -> Result<InFlight> {
        let d = &self.shared.device;
        let mut export = vk::ExportFenceCreateInfo::default()
            .handle_types(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        // SAFETY: plain creation; destroyed in Drop.
        unsafe {
            let commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .context("command buffer")?[0];
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default().push_next(&mut export), None)
                .context("fence")?;
            let queries = match self.caps.timestamp_period {
                Some(_) => Some(
                    d.create_query_pool(
                        &vk::QueryPoolCreateInfo::default()
                            .query_type(vk::QueryType::TIMESTAMP)
                            .query_count(2),
                        None,
                    )
                    .context("query pool")?,
                ),
                None => None,
            };
            Ok(InFlight {
                commands,
                fence,
                quads: None,
                staging: None,
                staged: 0,
                queries,
                submission: 0,
                ready: true,
            })
        }
    }

    /// The formats and modifiers clients may send dmabufs in: what Vulkan
    /// can sample with an importable modifier. Without format modifiers
    /// none: a client cannot know Vulkan's row pitch, and shm serves.
    fn query_dmabuf_formats(
        &self,
        phd: &PhysicalDevice,
    ) -> smithay::backend::allocator::format::FormatSet {
        let mut set = Vec::new();
        if !self.caps.modifiers {
            return set.into_iter().collect();
        }
        let instance = phd.instance().handle();
        for fourcc in texture::FORMATS {
            let Some((format, _)) = texture::vk_format(fourcc) else {
                continue;
            };
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            // SAFETY: the physical device belongs to this instance.
            unsafe {
                instance.get_physical_device_format_properties2(phd.handle(), format, &mut props)
            };
            let mut modifiers = vec![
                vk::DrmFormatModifierPropertiesEXT::default();
                list.drm_format_modifier_count as usize
            ];
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
                .drm_format_modifier_properties(&mut modifiers);
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            // SAFETY: as above; the array has the size Vulkan reported.
            unsafe {
                instance.get_physical_device_format_properties2(phd.handle(), format, &mut props)
            };
            for m in &modifiers {
                if m.drm_format_modifier_plane_count == 1
                    && m.drm_format_modifier_tiling_features
                        .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE)
                    && importable(phd, format, m.drm_format_modifier)
                {
                    set.push(Format {
                        code: fourcc,
                        modifier: Modifier::from(m.drm_format_modifier),
                    });
                }
            }
        }
        set.into_iter().collect()
    }

    /// A host-visible buffer, mapped; cached for the CPU to read back.
    fn host_buffer(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, *const u8)> {
        let b = self.host_buffer_full(size, usage, cached)?;
        Ok((b.buffer, b.memory, b.pointer))
    }

    fn host_buffer_full(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<HostBuffer> {
        HostBuffer::new(&self.shared, size, usage, cached)
    }

    /// The frame being recorded, ready: its last submission is done (as a
    /// rule long since – vblank paces frames).
    fn current(&mut self) -> Result<usize> {
        let i = self.current;
        if !self.in_flight[i].ready {
            let submission = self.in_flight[i].submission;
            self.shared.wait(submission)?;
            self.in_flight[i].staged = 0;
            self.in_flight[i].ready = true;
        }
        Ok(i)
    }

    /// Room for `bytes` in the current frame's staging buffer.
    fn stage(&mut self, bytes: u64) -> Result<(u64, *mut u8)> {
        let i = self.current()?;
        let staged = self.in_flight[i].staged;
        let needed = staged + bytes;
        let size = self.in_flight[i].staging.as_ref().map_or(0, |b| b.size);
        if needed > size {
            let new = self.host_buffer_full(
                needed.next_power_of_two().max(1 << 20),
                vk::BufferUsageFlags::TRANSFER_SRC,
                false,
            )?;
            if let Some(old) = self.in_flight[i].staging.take() {
                // What is staged for this frame moves along.
                // SAFETY: both mappings hold at least `staged` bytes.
                unsafe { std::ptr::copy_nonoverlapping(old.pointer, new.pointer, staged as usize) };
                self.shared.defer(
                    self.submitted,
                    Garbage::Buffer {
                        buffer: old.buffer,
                        memory: old.memory,
                    },
                );
            }
            self.in_flight[i].staging = Some(new);
        }
        let pointer = self.in_flight[i]
            .staging
            .as_ref()
            .context("staging")?
            .pointer;
        self.in_flight[i].staged = needed;
        // SAFETY: `staged + bytes` fits the mapping (grown above).
        Ok((staged, unsafe { pointer.add(staged as usize) }))
    }

    /// Copies `regions` of tightly or loosely packed pixels (`stride` bytes
    /// a row) into `texture`, with the next submission.
    fn upload(
        &mut self,
        texture: &VkTexture,
        data: &[u8],
        stride: usize,
        regions: &[Rectangle<i32, Buffer>],
    ) -> Result<()> {
        let bounds = Rectangle::from_size(texture.0.size);
        let mut copies = Vec::new();
        for region in regions {
            let Some(r) = region.intersection(bounds) else {
                continue;
            };
            copies.push(self.stage_rect(texture, data, stride, r.loc, r)?);
        }
        self.queue_copies(texture, copies);
        Ok(())
    }

    /// Stages the texels of `data` (`stride` bytes a row) from `from` on,
    /// for `to` in `texture`, which they fill.
    fn stage_rect(
        &mut self,
        texture: &VkTexture,
        data: &[u8],
        stride: usize,
        from: Point<i32, Buffer>,
        to: Rectangle<i32, Buffer>,
    ) -> Result<vk::BufferImageCopy> {
        let texel = texture::texel_bytes(texture.0.fourcc);
        let row = to.size.w as usize * texel;
        let last = (from.y + to.size.h - 1) as usize * stride + from.x as usize * texel + row;
        if from.x < 0 || from.y < 0 || last > data.len() {
            return Err(error("the buffer is smaller than its size says"));
        }
        let (offset, pointer) = self.stage((row * to.size.h as usize) as u64)?;
        for y in 0..to.size.h as usize {
            let start = (from.y as usize + y) * stride + from.x as usize * texel;
            // SAFETY: the source row lies within `data` (checked above), the
            // target row within the staged range.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(start), pointer.add(y * row), row)
            };
        }
        Ok(vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D {
                x: to.loc.x,
                y: to.loc.y,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: to.size.w as u32,
                height: to.size.h as u32,
                depth: 1,
            }))
    }

    fn queue_copies(&mut self, texture: &VkTexture, copies: Vec<vk::BufferImageCopy>) {
        if copies.is_empty() {
            return;
        }
        match self
            .uploads
            .iter_mut()
            .find(|u| Arc::ptr_eq(&u.texture.0, &texture.0))
        {
            Some(u) => u.regions.extend(copies),
            None => self.uploads.push(Upload {
                texture: texture.clone(),
                regions: copies,
            }),
        }
    }

    /// A texture of the renderer's own, filled with `data`.
    fn memory_texture(
        &mut self,
        data: &[u8],
        stride: usize,
        fourcc: Fourcc,
        size: Size<i32, Buffer>,
    ) -> Result<VkTexture> {
        let (format, _) =
            texture::vk_format(fourcc).context(format!("format {fourcc} is not taken"))?;
        if size.w <= 0 || size.h <= 0 {
            return Err(error("an empty texture"));
        }
        let (image, memory) = beize_vulkan::image::own_image(
            &self.shared,
            format,
            (size.w as u32, size.h as u32),
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let texture = texture::wrap(&self.shared, image, memory, fourcc, size, None, None)?;
        self.pipelines
            .write(&self.shared.device, texture.0.gpu.slot, texture.0.gpu.view);
        self.upload(&texture, data, stride, &[Rectangle::from_size(size)])?;
        Ok(texture)
    }
}

/// A quad over a whole picture of `size`, reading all of `source` (a
/// pass of the backdrop).
/// The sizes of the glass's steps from a `size` source (`Material`): Dual
/// Kawase halvings, then doublings back.
fn glass_sizes(size: Size<i32, Buffer>, material: Material) -> Result<Vec<Size<i32, Buffer>>> {
    if material.down == 0 {
        return Err(error("glass without blur"));
    }
    let mut halved = vec![size];
    for _ in 0..material.down {
        let last = halved[halved.len() - 1];
        halved.push(((last.w + 1) / 2, (last.h + 1) / 2).into());
    }
    let up = material.up.min(material.down) as usize;
    let mut sizes: Vec<_> = halved[1..].to_vec();
    sizes.extend((0..up).map(|i| halved[halved.len() - 2 - i]));
    Ok(sizes)
}

/// The passes that draw the glass of `source` through `chain`, prepared
/// for its size (`prepare_glass`); the last pass's picture is the glass.
fn glass_passes(source: &VkTexture, material: Material, chain: &[VkTexture]) -> Result<Vec<Pass>> {
    use beize_vulkan::pipeline::{BLUR_DOWN, BLUR_UP, MATERIAL};
    let sizes = glass_sizes(source.0.size, material)?;
    if chain.len() != sizes.len() || chain.iter().zip(&sizes).any(|(t, s)| t.0.size != *s) {
        return Err(error("the glass's pictures do not fit its source"));
    }
    let mut passes = Vec::with_capacity(sizes.len());
    let mut from = source.clone();
    for (step, texture) in chain.iter().enumerate() {
        let size = sizes[step];
        let mut quad = pass_quad(size, Some(&from));
        quad.flags = if step < material.down as usize {
            BLUR_DOWN
        } else {
            BLUR_UP
        };
        quad.sigma = material.offset;
        if step + 1 == sizes.len() {
            quad.flags |= MATERIAL;
            quad.color = material.tint;
            quad.bottom = [material.saturation, 0.0, 0.0, 0.0];
        }
        passes.push(Pass {
            texture: texture.clone(),
            quads: vec![quad],
            blend: vec![false],
            backdrop: true,
            reads: Some(from),
        });
        from = texture.clone();
    }
    Ok(passes)
}

fn pass_quad(size: Size<i32, Buffer>, source: Option<&VkTexture>) -> Quad {
    let mut quad = Quad {
        size: [size.w as f32, size.h as f32],
        ..Quad::default()
    };
    if let Some(source) = source {
        let s = source.0.size;
        quad.uv_x = [s.w as f32, 0.0];
        quad.uv_y = [0.0, s.h as f32];
        quad.texel = [1.0 / s.w as f32, 1.0 / s.h as f32];
        quad.texture = source.0.gpu.slot;
    }
    quad
}

/// Whether Vulkan imports dmabufs of `format` with `modifier` for sampling.
fn importable(phd: &PhysicalDevice, format: vk::Format, modifier: u64) -> bool {
    let mut modifier_info =
        vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default().drm_format_modifier(modifier);
    let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .push_next(&mut modifier_info)
        .push_next(&mut external);
    let mut external_props = vk::ExternalImageFormatProperties::default();
    let mut props = vk::ImageFormatProperties2::default().push_next(&mut external_props);
    // SAFETY: the physical device belongs to the instance; the chains live
    // until the call returns.
    let ok = unsafe {
        phd.instance()
            .handle()
            .get_physical_device_image_format_properties2(phd.handle(), &info, &mut props)
    }
    .is_ok();
    ok && external_props
        .external_memory_properties
        .external_memory_features
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        let d = &self.shared.device;
        // SAFETY: after the wait nothing of the renderer is in use; the
        // textures that outlive it keep only their own objects.
        unsafe {
            if let Err(e) = d.device_wait_idle() {
                warn!("waiting for the GPU: {e}");
            }
            for f in self.in_flight.drain(..) {
                d.destroy_fence(f.fence, None);
                if let Some(q) = f.queries {
                    d.destroy_query_pool(q, None);
                }
                for b in [f.quads, f.staging].into_iter().flatten() {
                    d.destroy_buffer(b.buffer, None);
                    d.free_memory(b.memory, None);
                }
            }
            d.destroy_command_pool(self.pool, None);
        }
        self.pipelines.destroy(d);
        self.dmabufs.clear();
        self.uploads.clear();
        self.shared.collect();
    }
}

impl RendererSuper for VkRenderer {
    type Error = Error;
    type TextureId = VkTexture;
    type Framebuffer<'buffer> = VkFramebuffer<'buffer>;
    type Frame<'frame, 'buffer>
        = VkFrame<'frame, 'buffer>
    where
        'buffer: 'frame,
        Self: 'frame;
}

impl Renderer for VkRenderer {
    fn context_id(&self) -> ContextId<VkTexture> {
        self.context.clone()
    }

    // Filtering: linear, and texel by texel where a texture lies 1:1
    // (`geometry::EXACT`). Smithay's choice changes nothing.
    fn downscale_filter(&mut self, _filter: TextureFilter) -> Result<()> {
        Ok(())
    }
    fn upscale_filter(&mut self, _filter: TextureFilter) -> Result<()> {
        Ok(())
    }

    fn set_debug_flags(&mut self, flags: DebugFlags) {
        self.debug = flags;
    }
    fn debug_flags(&self) -> DebugFlags {
        self.debug
    }

    fn render<'frame, 'buffer>(
        &'frame mut self,
        framebuffer: &'frame mut VkFramebuffer<'buffer>,
        _output_size: Size<i32, Physical>,
        dst_transform: Transform,
    ) -> Result<VkFrame<'frame, 'buffer>>
    where
        'buffer: 'frame,
    {
        VkFrame::new(self, framebuffer, dst_transform)
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<()> {
        sync.wait()
            .map_err(|e| error(format!("waiting for a fence: {e}")))
    }

    // Also done by each submission (`submit`).
    fn cleanup_texture_cache(&mut self) -> Result<()> {
        self.dmabufs.retain(|weak, _| !weak.is_gone());
        self.shared.collect();
        Ok(())
    }
}

impl Bind<Target> for VkRenderer {
    fn bind<'a>(&mut self, target: &'a mut Target) -> Result<VkFramebuffer<'a>> {
        Ok(VkFramebuffer(target))
    }

    fn supported_formats(&self) -> Option<smithay::backend::allocator::format::FormatSet> {
        None
    }
}

impl Offscreen<Target> for VkRenderer {
    fn create_buffer(&mut self, format: Fourcc, size: Size<i32, Buffer>) -> Result<Target> {
        self.offscreen_target(format, size)
    }
}

impl ImportMem for VkRenderer {
    fn import_memory(
        &mut self,
        data: &[u8],
        format: Fourcc,
        size: Size<i32, Buffer>,
        flipped: bool,
    ) -> Result<VkTexture> {
        if flipped {
            return Err(error("flipped memory is not taken"));
        }
        self.memory_texture(data, size.w as usize * 4, format, size)
    }

    fn update_memory(
        &mut self,
        texture: &VkTexture,
        data: &[u8],
        region: Rectangle<i32, Buffer>,
    ) -> Result<()> {
        if texture.0.dmabuf.is_some() {
            return Err(error("a dmabuf is not written to"));
        }
        let stride = texture.0.size.w as usize * texture::texel_bytes(texture.0.fourcc);
        self.upload(texture, data, stride, &[region])
    }

    fn mem_formats(&self) -> Box<dyn Iterator<Item = Fourcc>> {
        Box::new(texture::FORMATS.into_iter())
    }
}

/// Textures of a surface per renderer, as Smithay's GLES renderer keeps
/// them: a new buffer of the same size and format reuses the texture and
/// uploads only its damage.
type SurfaceTextures = Mutex<HashMap<ContextId<VkTexture>, VkTexture>>;

impl ImportMemWl for VkRenderer {
    fn import_shm_buffer(
        &mut self,
        buffer: &wl_buffer::WlBuffer,
        surface: Option<&SurfaceData>,
        damage: &[Rectangle<i32, Buffer>],
    ) -> Result<VkTexture> {
        use smithay::wayland::shm::{shm_format_to_fourcc, with_buffer_contents};
        let context = self.context.clone();
        let cache = surface.map(|s| {
            s.data_map
                .get_or_insert_threadsafe(SurfaceTextures::default)
        });
        with_buffer_contents(buffer, |pointer, length, data| {
            let fourcc = shm_format_to_fourcc(data.format)
                .context(format!("shm format {:?} is not taken", data.format))?;
            let size: Size<i32, Buffer> = (data.width, data.height).into();
            let start = data.offset.max(0) as usize;
            if start > length {
                return Err(error("shm offset beyond the pool"));
            }
            // SAFETY: Smithay maps the pool for the length of this closure;
            // `start..length` lies within it.
            let bytes = unsafe { std::slice::from_raw_parts(pointer.add(start), length - start) };
            let stride = data.stride as usize;
            let cached = cache.as_ref().and_then(|c| {
                c.lock()
                    .ok()
                    .and_then(|map| map.get(&context).cloned())
                    .filter(|t| t.0.size == size && t.0.fourcc == fourcc)
            });
            let texture = match cached {
                Some(texture) => {
                    let full = [Rectangle::from_size(size)];
                    let regions = if damage.is_empty() { &full[..] } else { damage };
                    self.upload(&texture, bytes, stride, regions)?;
                    texture
                }
                None => {
                    let texture = self.memory_texture(bytes, stride, fourcc, size)?;
                    if let Some(map) = cache.as_ref().and_then(|c| c.lock().ok()).as_mut() {
                        map.insert(context.clone(), texture.clone());
                    }
                    texture
                }
            };
            Ok(texture)
        })
        .map_err(|e| error(format!("shm buffer: {e}")))?
    }
}

impl ImportDma for VkRenderer {
    fn dmabuf_formats(&self) -> smithay::backend::allocator::format::FormatSet {
        self.dmabuf_formats.clone()
    }

    fn import_dmabuf(
        &mut self,
        dmabuf: &Dmabuf,
        _damage: Option<&[Rectangle<i32, Buffer>]>,
    ) -> Result<VkTexture> {
        if let Some(texture) = self.dmabufs.get(&dmabuf.weak()) {
            return Ok(texture.clone());
        }
        let texture = self.import_new(dmabuf)?;
        self.pipelines
            .write(&self.shared.device, texture.0.gpu.slot, texture.0.gpu.view);
        self.dmabufs.insert(dmabuf.weak(), texture.clone());
        Ok(texture)
    }
}

impl VkRenderer {
    /// Whether a client's new dmabuf can be shown: imported, then let go.
    /// It keeps a texture once a frame draws it (`import_dmabuf`), as
    /// Weston makes its texture at the first attach: buffers a client
    /// only made, or that wait behind others, hold none – kept since
    /// they were made, one terminal's held all it ever had (0.5.105).
    pub fn check_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<()> {
        self.import_new(dmabuf).map(drop)
    }

    fn import_new(&self, dmabuf: &Dmabuf) -> Result<VkTexture> {
        let fourcc = dmabuf.format().code;
        let (format, _) =
            texture::vk_format(fourcc).context(format!("dmabuf format {fourcc} is not taken"))?;
        let (image, memory) = texture::import_image(
            &self.shared,
            &self.ext,
            self.caps.modifiers,
            dmabuf,
            format,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let size = (dmabuf.width() as i32, dmabuf.height() as i32).into();
        texture::wrap(
            &self.shared,
            image,
            memory,
            fourcc,
            size,
            Some(dmabuf.weak()),
            None,
        )
    }
}

impl ImportDmaWl for VkRenderer {}

/// Pixels read back from the GPU.
#[derive(Debug)]
pub struct VkMapping {
    data: Vec<u8>,
    size: Size<i32, Buffer>,
    fourcc: Fourcc,
}

impl Texture for VkMapping {
    fn width(&self) -> u32 {
        self.size.w as u32
    }
    fn height(&self) -> u32 {
        self.size.h as u32
    }
    fn format(&self) -> Option<Fourcc> {
        Some(self.fourcc)
    }
}

impl TextureMapping for VkMapping {
    fn flipped(&self) -> bool {
        false
    }
}

impl ExportMem for VkRenderer {
    type TextureMapping = VkMapping;

    fn copy_framebuffer(
        &mut self,
        target: &VkFramebuffer<'_>,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<VkMapping> {
        let t = &target.0;
        let layout = if t.filled {
            frame::target_layout(t)
        } else {
            vk::ImageLayout::UNDEFINED
        };
        let foreign = t.dmabuf.is_some();
        self.read_image(t.image, t.format, layout, foreign, region, format)
    }

    fn copy_texture(
        &mut self,
        texture: &VkTexture,
        region: Rectangle<i32, Buffer>,
        format: Fourcc,
    ) -> Result<VkMapping> {
        let (vk_format, _) = texture::vk_format(texture.0.fourcc).context("texture format")?;
        let foreign = texture.0.dmabuf.is_some();
        let layout = if foreign {
            vk::ImageLayout::GENERAL
        } else {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        };
        let texture = texture.clone();
        let mapping = self.read_image(
            texture.0.gpu.image,
            vk_format,
            layout,
            foreign,
            region,
            format,
        )?;
        texture.used(self.submitted);
        Ok(mapping)
    }

    fn can_read_texture(&mut self, _texture: &VkTexture) -> Result<bool> {
        Ok(true)
    }

    fn map_texture<'a>(&mut self, mapping: &'a VkMapping) -> Result<&'a [u8]> {
        Ok(&mapping.data)
    }
}

impl VkRenderer {
    /// Copies `region` of an image to the CPU, waiting for it.
    fn read_image(
        &mut self,
        image: vk::Image,
        image_format: vk::Format,
        layout: vk::ImageLayout,
        foreign: bool,
        region: Rectangle<i32, Buffer>,
        fourcc: Fourcc,
    ) -> Result<VkMapping> {
        match texture::vk_format(fourcc) {
            Some((f, _)) if f == image_format => {}
            _ => return Err(error(format!("cannot read back as {fourcc}"))),
        }
        if region.size.w <= 0 || region.size.h <= 0 {
            return Err(error("an empty region"));
        }
        let bytes = region.size.w as u64 * region.size.h as u64 * 4;
        let (buffer, memory, pointer) =
            self.host_buffer(bytes, vk::BufferUsageFlags::TRANSFER_DST, true)?;
        let copy = frame::Readout {
            image,
            layout,
            foreign,
            buffer,
            region,
            family: self.family,
        };
        let submission = self.submit(None, Some(&copy));
        let result = submission.and_then(|value| {
            self.shared.wait(value)?;
            let mut data = vec![0; bytes as usize];
            // SAFETY: the copy is done; the mapping holds `bytes` bytes.
            unsafe { std::ptr::copy_nonoverlapping(pointer, data.as_mut_ptr(), bytes as usize) };
            Ok(data)
        });
        self.shared
            .defer(self.submitted, Garbage::Buffer { buffer, memory });
        Ok(VkMapping {
            data: result?,
            size: region.size,
            fourcc,
        })
    }

    /// Submits what is recorded: pending uploads, then a frame or a read
    /// back. Returns the submission's number.
    fn submit(
        &mut self,
        frame: Option<&mut frame::Recorded<'_>>,
        readout: Option<&frame::Readout>,
    ) -> Result<u64> {
        let began = Instant::now();
        let i = self.current()?;
        let recording = Instant::now();
        self.spent.wait += recording - began;
        // Each submission lets go of the clients' dmabufs that are gone (a
        // window's old size, a closed app), as Smithay's GLES renderer does
        // each frame: Smithay asks for it only in its DrmCompositor, which
        // Beize does not use. Kept, every resize held the buffers of each
        // size until the GPU's memory ran out (0.5.103).
        self.dmabufs.retain(|weak, _| !weak.is_gone());
        self.shared.collect();
        let d = self.shared.device.clone();
        let commands = self.in_flight[i].commands;
        let submission = self.submitted + 1;
        // SAFETY: the command buffer is this frame's and free (`current`
        // waited for its last submission).
        unsafe {
            d.reset_command_buffer(commands, vk::CommandBufferResetFlags::empty())
                .context("resetting commands")?;
            d.begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .context("recording")?;
            if let Some(pool) = self.in_flight[i].queries {
                d.cmd_reset_query_pool(commands, pool, 0, 2);
                d.cmd_write_timestamp2(commands, vk::PipelineStageFlags2::NONE, pool, 0);
            }
        }
        let uploads = std::mem::take(&mut self.uploads);
        if let Some(staging) = &self.in_flight[i].staging {
            frame::record_uploads(&d, commands, staging.buffer, &uploads, submission);
        }
        drop(uploads);
        let passes = std::mem::take(&mut self.passes);
        // The passes', then the frame's, then those of its live glass.
        let mut lists: Vec<&[Quad]> = passes.iter().map(|p| p.quads.as_slice()).collect();
        if let Some(frame) = &frame {
            lists.push(&frame.quads);
            for insert in &frame.inserts {
                lists.extend(insert.passes.iter().map(|p| p.quads.as_slice()));
            }
        }
        let addresses = self.write_quads(i, &lists)?;
        let pass_pipelines = [
            self.pipelines.get(&d, PICTURE_DRAWN)?,
            self.pipelines.get_backdrop(&d, PICTURE_DRAWN)?,
        ];
        if !passes.is_empty() {
            let pipelines = pass_pipelines;
            frame::record_passes(
                &d,
                commands,
                &self.pipelines,
                pipelines,
                &passes,
                &addresses,
                submission,
            );
        }
        let mut waits = Vec::new();
        let mut synced = Duration::ZERO;
        if let Some(frame) = frame {
            let pipelines = self.pipelines.get(&d, frame.target.format)?;
            let quads = addresses.get(passes.len()).copied().unwrap_or_default();
            let mut rest = addresses.iter().skip(passes.len() + 1).copied();
            let insert_addresses: Vec<Vec<u64>> = frame
                .inserts
                .iter()
                .map(|insert| rest.by_ref().take(insert.passes.len()).collect())
                .collect();
            frame::record_frame(
                &d,
                commands,
                &self.pipelines,
                pipelines,
                pass_pipelines,
                quads,
                &insert_addresses,
                frame,
                submission,
            );
            let syncing = Instant::now();
            waits = frame::implicit_waits(&d, &self.ext, frame, submission);
            synced = syncing.elapsed();
        }
        if let Some(readout) = readout {
            frame::record_readout(&d, commands, readout);
        }
        // SAFETY: as above.
        unsafe {
            if let Some(pool) = self.in_flight[i].queries {
                d.cmd_write_timestamp2(commands, vk::PipelineStageFlags2::ALL_COMMANDS, pool, 1);
            }
            d.end_command_buffer(commands).context("recording")?;
        }
        let fence = self.in_flight[i].fence;
        let wait_infos: Vec<vk::SemaphoreSubmitInfo> = waits
            .iter()
            .map(|&s| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(s)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            })
            .collect();
        let signal = [vk::SemaphoreSubmitInfo::default()
            .semaphore(self.shared.timeline)
            .value(submission)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(commands)];
        let info = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait_infos)
            .command_buffer_infos(&buffers)
            .signal_semaphore_infos(&signal);
        let queueing = Instant::now();
        self.spent.record += (queueing - recording).saturating_sub(synced);
        self.spent.sync += synced;
        // SAFETY: the fence is not pending (its frame is done); every
        // object the commands use lives until this submission completes.
        unsafe {
            d.reset_fences(&[fence]).context("fence")?;
            d.queue_submit2(self.queue, &[info], fence)
                .context("submitting")?;
        }
        self.spent.queue += queueing.elapsed();
        for s in waits {
            self.shared.defer(submission, Garbage::Semaphore(s));
        }
        self.submitted = submission;
        self.in_flight[i].submission = submission;
        self.in_flight[i].ready = false;
        self.current = (i + 1) % IN_FLIGHT;
        Ok(submission)
    }

    /// The pipelines for the frame's target and its quads, copied into the
    /// frame's buffer.
    /// `lists` one after another in frame `i`'s quad buffer, grown if need
    /// be; each list's device address.
    fn write_quads(&mut self, i: usize, lists: &[&[Quad]]) -> Result<Vec<u64>> {
        let starts: Vec<usize> = lists
            .iter()
            .scan(0, |end: &mut usize, list| {
                let start = end.next_multiple_of(QUADS_ALIGN);
                *end = start + std::mem::size_of_val(*list);
                Some(start)
            })
            .collect();
        let bytes = lists
            .last()
            .zip(starts.last())
            .map_or(0, |(list, start)| start + std::mem::size_of_val(*list))
            as u64;
        let size = self.in_flight[i].quads.as_ref().map_or(0, |b| b.size);
        if bytes > size {
            let new = self.host_buffer_full(
                bytes.next_power_of_two().max(64 << 10),
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                false,
            )?;
            if let Some(old) = self.in_flight[i].quads.replace(new) {
                // This frame's previous use is done (`current`).
                self.shared.defer(
                    0,
                    Garbage::Buffer {
                        buffer: old.buffer,
                        memory: old.memory,
                    },
                );
            }
        }
        let Some(quads) = self.in_flight[i].quads.as_ref() else {
            return Ok(vec![0; lists.len()]);
        };
        for (list, &start) in lists.iter().zip(&starts) {
            // SAFETY: the buffer holds `bytes`, past every list's end (grown
            // above); no submission reads it (`current`).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    list.as_ptr() as *const u8,
                    quads.pointer.add(start),
                    std::mem::size_of_val(*list),
                )
            };
        }
        Ok(starts.iter().map(|&s| quads.address + s as u64).collect())
    }

    /// Ends a frame: submits it and hands out its end.
    fn finish_frame(&mut self, frame: &mut frame::Recorded<'_>) -> Result<SyncPoint> {
        let submission = self.submit(Some(frame), None)?;
        let target = &mut *frame.target;
        target.filled = true;
        target.last_used = submission;
        if let Some(readback) = &target.readback {
            // lavapipe's copy: the CPU copies, so it waits.
            let waiting = Instant::now();
            self.shared.wait(submission)?;
            readback.copy_out(target.size)?;
            self.spent.wait += waiting.elapsed();
            return Ok(SyncPoint::signaled());
        }
        let syncing = Instant::now();
        let point = self.export_frame(frame, submission);
        self.spent.sync += syncing.elapsed();
        point
    }

    /// The frame's fence as a sync_file, for KMS and into the dmabufs it
    /// touched.
    fn export_frame(
        &mut self,
        frame: &mut frame::Recorded<'_>,
        submission: u64,
    ) -> Result<SyncPoint> {
        let target = &*frame.target;
        let i = (self.current + IN_FLIGHT - 1) % IN_FLIGHT;
        let fence = self.in_flight[i].fence;
        let info = vk::FenceGetFdInfoKHR::default()
            .fence(fence)
            .handle_type(vk::ExternalFenceHandleTypeFlags::SYNC_FD);
        // SAFETY: the fence has a pending signal (just submitted); export
        // resets it, which `submit` does anyway before its next use.
        let fd = match unsafe { self.ext.fence_fd.get_fence_fd(&info) } {
            Ok(fd) if fd >= 0 => {
                // SAFETY: a new file descriptor we own.
                unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) }
            }
            // -1: already signalled.
            Ok(_) => return Ok(SyncPoint::signaled()),
            Err(e) => {
                warn!("exporting the frame's fence: {e}; waiting instead");
                self.shared.wait(submission)?;
                return Ok(SyncPoint::signaled());
            }
        };
        // Into the dmabufs the frame touched: readers and writers of them
        // elsewhere see it (implicit sync).
        for texture in &frame.foreign {
            if let Some(dmabuf) = texture.0.dmabuf.as_ref().and_then(|w| w.upgrade())
                && let Some(handle) = dmabuf.handles().next()
                && let Err(e) = sync::attach(handle, fd.as_fd(), false)
            {
                debug!("adding the frame's read to a dmabuf: {e}");
            }
        }
        if let Some(dmabuf) = &target.dmabuf
            && let Some(handle) = dmabuf.handles().next()
            && let Err(e) = sync::attach(handle, fd.as_fd(), true)
        {
            debug!("adding the frame's write to the screen buffer: {e}");
        }
        Ok(SyncPoint::from(sync::SyncFile(fd)))
    }
}
