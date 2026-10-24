//! Drawing a scene into an image – a window's swapchain image or one of
//! its own offscreen: the frames in flight with their quad and staging
//! buffers, and the atlases on the GPU – glyphs and symbols, images in
//! colour – filled as the scenes need them (`glyphs`).
//!
//! Two frames are in flight, as in the compositor: a frame's objects are
//! reused once the timeline says its last submission is done.

use ash::vk;
use beize_vulkan::barrier::{
    Access, COLOR_WRITE, NONE, SAME, SHADER_READ, TRANSFER_READ, TRANSFER_WRITE, barrier, barriers,
};
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::device::Garbage;
use beize_vulkan::error::{Context, Result};
use beize_vulkan::image::{Sampled, own_image};
use beize_vulkan::pipeline::{Attachment, MASK, Quad};
use tracing::{debug, warn};

use crate::gpu::Gpu;
use beize_draw::glyphs::{ATLAS_SIZE, COLOUR_ATLAS_SIZE, Glyphs};
use beize_draw::{Atlases, Scene};

const IN_FLIGHT: usize = 2;
/// 8 bits of coverage per texel.
const ATLAS_FORMAT: vk::Format = vk::Format::R8_UNORM;
/// Colour in sRGB, decoded to linear light when sampled, and alpha
/// (`beize_draw::glyphs`, `take_colour_uploads`).
const COLOUR_FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
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
    /// New glyphs and images on their way to the atlases.
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
    atlas: GpuAtlas,
    colour: GpuAtlas,
}

/// An atlas on the GPU, made on first use.
struct GpuAtlas {
    format: vk::Format,
    size: u32,
    sampled: Option<Sampled>,
    /// It has been written: it waits between frames in
    /// SHADER_READ_ONLY_OPTIMAL.
    filled: bool,
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
            atlas: GpuAtlas::new(ATLAS_FORMAT, ATLAS_SIZE),
            colour: GpuAtlas::new(COLOUR_FORMAT, COLOUR_ATLAS_SIZE),
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
        let atlases = Atlases {
            coverage: if scene.has_glyphs() {
                self.atlas.slot(&self.gpu)?
            } else {
                0
            },
            colour: if scene.has_images() {
                Some(self.colour.slot(&self.gpu)?)
            } else {
                None
            },
        };
        let quads = scene
            .quads(&mut self.glyphs, atlases)
            .context("the scene's glyphs and images do not fit into the atlases")?;
        let [coverage, colour] = self.stage(i)?;
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
        if let Some(staging) = &self.in_flight[i].staging {
            self.atlas.copy(d, commands, staging.buffer, &coverage);
            self.colour.copy(d, commands, staging.buffer, &colour);
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
                .record(d, commands, pipelines, &attachment, address, scene.blend())
        };
        debug!(
            "{} quads ({} glyphs and symbols, {} new) in {draws} draws",
            quads.len(),
            quads.iter().filter(|q| q.flags & MASK != 0).count(),
            coverage.len() + colour.len()
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
        for atlas in [&self.atlas, &self.colour] {
            if let Some(sampled) = &atlas.sampled {
                sampled.used(submission);
            }
        }
        self.submitted = submission;
        self.in_flight[i].submission = submission;
        self.current = (i + 1) % IN_FLIGHT;
        Ok(submission)
    }

    /// The new glyphs' and images' texels in frame `i`'s staging buffer;
    /// the copies into each atlas.
    fn stage(&mut self, i: usize) -> Result<[Vec<vk::BufferImageCopy>; 2]> {
        let uploads = [
            self.glyphs.take_uploads(),
            self.glyphs.take_colour_uploads(),
        ];
        // Each region starts on a whole texel of the colour atlas (Vulkan:
        // a multiple of the format's texel size).
        let bytes: usize = uploads
            .iter()
            .flatten()
            .map(|(_, texels)| texels.len().next_multiple_of(4))
            .sum();
        if bytes == 0 {
            return Ok([Vec::new(), Vec::new()]);
        }
        let staging = grow(
            &self.gpu.shared,
            &mut self.in_flight[i].staging,
            bytes as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        let mut regions = [Vec::new(), Vec::new()];
        let mut offset = 0;
        for (uploads, regions) in uploads.iter().zip(&mut regions) {
            for (placed, texels) in uploads {
                // SAFETY: the buffer holds `bytes` (grown above), no
                // submission reads it (`next` waited for this frame's last).
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        texels.as_ptr(),
                        staging.pointer.add(offset),
                        texels.len(),
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
                offset += texels.len().next_multiple_of(4);
            }
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

impl GpuAtlas {
    const fn new(format: vk::Format, size: u32) -> GpuAtlas {
        GpuAtlas {
            format,
            size,
            sampled: None,
            filled: false,
        }
    }

    /// Its place in the texture array, made on first use.
    fn slot(&mut self, gpu: &Gpu) -> Result<u32> {
        if let Some(sampled) = &self.sampled {
            return Ok(sampled.slot);
        }
        let shared = &gpu.shared;
        let (image, memory) = own_image(
            shared,
            self.format,
            (self.size, self.size),
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        let sampled = Sampled::new(shared, image, memory, self.format, true)?;
        gpu.pipelines
            .write(&shared.device, sampled.slot, sampled.view);
        let slot = sampled.slot;
        self.sampled = Some(sampled);
        Ok(slot)
    }

    /// Records the copies `regions` from `staging` into it.
    fn copy(
        &mut self,
        d: &ash::Device,
        commands: vk::CommandBuffer,
        staging: vk::Buffer,
        regions: &[vk::BufferImageCopy],
    ) {
        let Some(sampled) = &self.sampled else {
            return;
        };
        if regions.is_empty() {
            return;
        }
        // Earlier frames may still read places a cleared atlas gives out
        // again: the barrier waits for their reads too.
        let (old, src) = if self.filled {
            (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, SHADER_READ)
        } else {
            (vk::ImageLayout::UNDEFINED, NONE)
        };
        barriers(
            d,
            commands,
            &[barrier(
                sampled.image,
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
                staging,
                sampled.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                regions,
            )
        };
        barriers(
            d,
            commands,
            &[barrier(
                sampled.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                TRANSFER_WRITE,
                SHADER_READ,
                SAME,
            )],
        );
        self.filled = true;
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
