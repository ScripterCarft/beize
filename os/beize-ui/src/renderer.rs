//! Drawing a scene into an image – a window's swapchain image or one of
//! its own offscreen: the frames in flight with their quad and staging
//! buffers, and the glyph atlas on the GPU, filled as text needs it
//! (`glyphs`).
//!
//! Two frames are in flight, as in the compositor: a frame's objects are
//! reused once the timeline says its last submission is done.

use ash::vk;
use beize_vulkan::barrier::{
    Access, COLOR_WRITE, NONE, SAME, SHADER_READ, TRANSFER_READ, TRANSFER_WRITE, barrier, barriers,
};
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::device::Garbage;
use beize_vulkan::error::{Context, Result, error};
use beize_vulkan::image::{Sampled, own_image};
use beize_vulkan::pipeline::{Attachment, Quad};
use tracing::{debug, warn};

use crate::draw::Scene;
use crate::glyphs::{ATLAS_SIZE, Full, Glyphs};
use crate::gpu::Gpu;

const IN_FLIGHT: usize = 2;
/// 8 bits of coverage per texel.
const ATLAS_FORMAT: vk::Format = vk::Format::R8_UNORM;
/// The target before drawing: a swapchain image is waited for at this
/// stage (its acquisition), nothing of it is read.
const UNTOUCHED: Access = (
    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
    vk::AccessFlags2::NONE,
);

/// The image a frame draws into, whole.
pub(crate) struct Target {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub size: (u32, u32),
    pub after: After,
}

/// What becomes of the drawn image.
pub(crate) enum After {
    /// Shown by the swapchain.
    Present,
    /// Copied into this buffer, rows tightly packed.
    Read(vk::Buffer),
}

struct InFlight {
    commands: vk::CommandBuffer,
    /// Signalled when a swapchain hands out the frame's image.
    acquired: vk::Semaphore,
    quads: Option<HostBuffer>,
    /// New glyphs on their way to the atlas.
    staging: Option<HostBuffer>,
    /// The submission that last used it.
    submission: u64,
}

pub struct Renderer {
    pub(crate) gpu: Gpu,
    pool: vk::CommandPool,
    in_flight: Vec<InFlight>,
    current: usize,
    submitted: u64,
    glyphs: Glyphs,
    atlas: Option<Sampled>,
    /// The atlas has been written: it waits between frames in
    /// SHADER_READ_ONLY_OPTIMAL.
    atlas_filled: bool,
}

impl Renderer {
    pub fn new(gpu: Gpu) -> Result<Renderer> {
        let mut renderer = Renderer {
            gpu,
            pool: vk::CommandPool::null(),
            in_flight: Vec::new(),
            current: 0,
            submitted: 0,
            glyphs: Glyphs::new(),
            atlas: None,
            atlas_filled: false,
        };
        // From here on, Drop cleans up whatever was made.
        let d = &renderer.gpu.shared.device;
        // SAFETY: plain creation; destroyed in Drop.
        renderer.pool = unsafe {
            d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(renderer.gpu.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .context("command pool")?;
        for _ in 0..IN_FLIGHT {
            let frame = renderer.in_flight_new()?;
            renderer.in_flight.push(frame);
        }
        Ok(renderer)
    }

    fn in_flight_new(&self) -> Result<InFlight> {
        let d = &self.gpu.shared.device;
        // SAFETY: plain creation; destroyed in Drop (the command buffer
        // with its pool).
        unsafe {
            let commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .context("command buffer")?[0];
            let acquired = d
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .context("semaphore")?;
            Ok(InFlight {
                commands,
                acquired,
                quads: None,
                staging: None,
                submission: 0,
            })
        }
    }

    /// Waits until the next frame's objects are free; returns the
    /// semaphore a swapchain image for it is acquired with.
    pub(crate) fn next(&mut self) -> Result<vk::Semaphore> {
        let frame = &self.in_flight[self.current];
        self.gpu.shared.wait(frame.submission)?;
        self.gpu.shared.collect();
        Ok(frame.acquired)
    }

    /// Draws `scene` into `target` (the frame after `next`), after `wait`
    /// at the colour output, signalling `signal` when done. Returns the
    /// submission.
    pub(crate) fn draw(
        &mut self,
        scene: &Scene,
        target: &Target,
        wait: Option<vk::Semaphore>,
        signal: Option<vk::Semaphore>,
    ) -> Result<u64> {
        let i = self.current;
        self.next()?;
        let quads = self.place_glyphs(scene)?;
        let uploads = self.stage_glyphs(i)?;
        let address = self.upload_quads(i, &quads)?;
        let shared = self.gpu.shared.clone();
        let d = &shared.device;
        let pipelines = self.gpu.pipelines.get(d, target.format)?;
        let commands = self.in_flight[i].commands;
        let submission = self.submitted + 1;
        // SAFETY: the command buffer is this frame's and free (`next`);
        // every object it records lives until the submission completes.
        unsafe {
            d.reset_command_buffer(commands, vk::CommandBufferResetFlags::empty())
                .context("resetting commands")?;
            d.begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .context("recording")?;
        }
        if let (Some(atlas), Some(staging)) = (&self.atlas, &self.in_flight[i].staging)
            && !uploads.is_empty()
        {
            // Earlier frames may still read places a cleared atlas gives
            // out again: the barrier waits for their reads too.
            let (old, src) = if self.atlas_filled {
                (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, SHADER_READ)
            } else {
                (vk::ImageLayout::UNDEFINED, NONE)
            };
            barriers(
                d,
                commands,
                &[barrier(
                    atlas.image,
                    old,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    src,
                    TRANSFER_WRITE,
                    SAME,
                )],
            );
            // SAFETY: the regions lie in the staging buffer and the atlas.
            unsafe {
                d.cmd_copy_buffer_to_image(
                    commands,
                    staging.buffer,
                    atlas.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &uploads,
                )
            };
            barriers(
                d,
                commands,
                &[barrier(
                    atlas.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    TRANSFER_WRITE,
                    SHADER_READ,
                    SAME,
                )],
            );
            self.atlas_filled = true;
        }
        barriers(
            d,
            commands,
            &[barrier(
                target.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                UNTOUCHED,
                COLOR_WRITE,
                SAME,
            )],
        );
        let attachment = Attachment {
            view: target.view,
            size: target.size,
            load: false,
        };
        // SAFETY: the target is in COLOR_ATTACHMENT_OPTIMAL (above), the
        // pipelines are its format's; the quad buffer and the atlas live
        // until the submission completes.
        let draws = unsafe {
            self.gpu
                .pipelines
                .record(d, commands, pipelines, &attachment, address, &scene.blend)
        };
        debug!(
            "{} quads ({} glyphs, {} new) in {draws} draws",
            quads.len(),
            scene.glyphs.len(),
            uploads.len()
        );
        match target.after {
            After::Present => barriers(
                d,
                commands,
                &[barrier(
                    target.image,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    COLOR_WRITE,
                    NONE,
                    SAME,
                )],
            ),
            After::Read(buffer) => {
                barriers(
                    d,
                    commands,
                    &[barrier(
                        target.image,
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        COLOR_WRITE,
                        TRANSFER_READ,
                        SAME,
                    )],
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: target.size.0,
                        height: target.size.1,
                        depth: 1,
                    });
                // SAFETY: the buffer holds the whole image (the caller's).
                unsafe {
                    d.cmd_copy_image_to_buffer(
                        commands,
                        target.image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        buffer,
                        &[region],
                    )
                };
            }
        }
        // SAFETY: as above.
        unsafe { d.end_command_buffer(commands) }.context("recording")?;

        let waits: Vec<_> = wait
            .iter()
            .map(|&s| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(s)
                    .stage_mask(UNTOUCHED.0)
            })
            .collect();
        let mut signals = vec![
            vk::SemaphoreSubmitInfo::default()
                .semaphore(shared.timeline)
                .value(submission)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
        ];
        signals.extend(signal.map(|s| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(s)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        }));
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(commands)];
        let info = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .command_buffer_infos(&buffers)
            .signal_semaphore_infos(&signals);
        // SAFETY: everything the commands use lives until the submission
        // completes (the buffers are this frame's, the atlas is kept).
        unsafe { d.queue_submit2(self.gpu.queue, &[info], vk::Fence::null()) }
            .context("submitting")?;
        if let Some(atlas) = &self.atlas {
            atlas.used(submission);
        }
        self.submitted = submission;
        self.in_flight[i].submission = submission;
        self.current = (i + 1) % IN_FLIGHT;
        Ok(submission)
    }

    /// The scene's quads with its glyphs filled in, rasterized where new.
    /// A full atlas starts over once.
    fn place_glyphs(&mut self, scene: &Scene) -> Result<Vec<Quad>> {
        let mut quads = scene.quads.clone();
        if scene.glyphs.is_empty() {
            return Ok(quads);
        }
        let slot = self.atlas()?;
        for attempt in 0..2 {
            match self.fill_glyphs(scene, &mut quads, slot) {
                Ok(()) => return Ok(quads),
                Err(Full) if attempt == 0 => {
                    debug!("the glyph atlas is full: starting over");
                    self.glyphs.clear();
                }
                Err(Full) => break,
            }
        }
        Err(error("the scene's glyphs do not fit into the atlas"))
    }

    fn fill_glyphs(
        &mut self,
        scene: &Scene,
        quads: &mut [Quad],
        slot: u32,
    ) -> std::result::Result<(), Full> {
        let texel = 1.0 / ATLAS_SIZE as f32;
        for glyph in &scene.glyphs {
            let (font, face) = &scene.runs[glyph.run];
            let (placed, whole) = self.glyphs.get(font, face, glyph.id, glyph.x)?;
            let quad = &mut quads[glyph.quad];
            let (w, h) = (placed.w as f32, placed.h as f32);
            quad.pos = [(whole + placed.left) as f32, glyph.y + placed.top as f32];
            // Without ink: nothing covered.
            quad.size = [w, h];
            quad.uv = [placed.x as f32, placed.y as f32];
            quad.uv_x = [w, 0.0];
            quad.uv_y = [0.0, h];
            quad.texel = [texel, texel];
            quad.texture = slot;
        }
        Ok(())
    }

    /// The atlas' place in the texture array, made on first use.
    fn atlas(&mut self) -> Result<u32> {
        if let Some(atlas) = &self.atlas {
            return Ok(atlas.slot);
        }
        let shared = &self.gpu.shared;
        let (image, memory) = own_image(
            shared,
            ATLAS_FORMAT,
            (ATLAS_SIZE, ATLAS_SIZE),
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        let atlas = Sampled::new(shared, image, memory, ATLAS_FORMAT, true)?;
        self.gpu
            .pipelines
            .write(&shared.device, atlas.slot, atlas.view);
        let slot = atlas.slot;
        self.atlas = Some(atlas);
        Ok(slot)
    }

    /// The new glyphs' coverage in frame `i`'s staging buffer; the copies
    /// into the atlas.
    fn stage_glyphs(&mut self, i: usize) -> Result<Vec<vk::BufferImageCopy>> {
        let uploads = self.glyphs.take_uploads();
        let bytes: usize = uploads.iter().map(|(_, c)| c.len()).sum();
        if bytes == 0 {
            return Ok(Vec::new());
        }
        let staging = grow(
            &self.gpu.shared,
            &mut self.in_flight[i].staging,
            bytes as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        let mut regions = Vec::with_capacity(uploads.len());
        let mut offset = 0;
        for (placed, coverage) in &uploads {
            // SAFETY: the buffer holds `bytes` (grown above), no submission
            // reads it (`next` waited for this frame's last).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    coverage.as_ptr(),
                    staging.pointer.add(offset),
                    coverage.len(),
                )
            };
            regions.push(
                vk::BufferImageCopy::default()
                    .buffer_offset(offset as u64)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_offset(vk::Offset3D {
                        x: placed.x as i32,
                        y: placed.y as i32,
                        z: 0,
                    })
                    .image_extent(vk::Extent3D {
                        width: placed.w,
                        height: placed.h,
                        depth: 1,
                    }),
            );
            offset += coverage.len();
        }
        Ok(regions)
    }

    /// `quads` in frame `i`'s buffer; its device address.
    fn upload_quads(&mut self, i: usize, quads: &[Quad]) -> Result<u64> {
        let bytes = std::mem::size_of_val(quads);
        if bytes == 0 {
            return Ok(0);
        }
        let buffer = grow(
            &self.gpu.shared,
            &mut self.in_flight[i].quads,
            bytes as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        )?;
        // SAFETY: the buffer holds `bytes` (grown above); no submission
        // reads it (`next` waited for this frame's last).
        unsafe {
            std::ptr::copy_nonoverlapping(quads.as_ptr().cast::<u8>(), buffer.pointer, bytes)
        };
        Ok(buffer.address)
    }
}

/// `slot`'s buffer, replaced by a bigger one if it holds less than
/// `bytes`. Its frame's last use is done (`next`).
fn grow<'a>(
    shared: &beize_vulkan::device::Shared,
    slot: &'a mut Option<HostBuffer>,
    bytes: u64,
    usage: vk::BufferUsageFlags,
) -> Result<&'a HostBuffer> {
    if slot.as_ref().is_none_or(|b| b.size < bytes) {
        let new = HostBuffer::new(
            shared,
            bytes.next_power_of_two().max(16 << 10),
            usage,
            false,
        )?;
        if let Some(old) = slot.replace(new) {
            shared.defer(
                0,
                Garbage::Buffer {
                    buffer: old.buffer,
                    memory: old.memory,
                },
            );
        }
    }
    slot.as_ref().context("buffer")
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let d = &self.gpu.shared.device;
        // SAFETY: nothing records any more; after the wait the GPU uses
        // nothing of the renderer. The atlas goes to the device's garbage.
        unsafe {
            if let Err(e) = d.device_wait_idle() {
                warn!("waiting for the GPU: {e}");
            }
            for frame in self.in_flight.drain(..) {
                d.destroy_semaphore(frame.acquired, None);
                for buffer in [frame.quads, frame.staging].into_iter().flatten() {
                    d.destroy_buffer(buffer.buffer, None);
                    d.free_memory(buffer.memory, None);
                }
            }
            d.destroy_command_pool(self.pool, None);
        }
    }
}
