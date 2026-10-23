//! A frame: Smithay's draw calls become quads (`geometry`), cut to their
//! damage and split into opaque and blended parts; at the end they are
//! recorded as runs of one pipeline each – usually one to three draws.
//!
//! Images shared with others (the screen's dmabuf, clients' dmabufs) pass
//! between Vulkan and their owner by queue family ownership transfer from
//! and back to the foreign queue, as wlroots' Vulkan renderer does.

use std::marker::PhantomData;
use std::os::fd::IntoRawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Color32F, ContextId, Frame, Texture};
use smithay::reexports::ash::{self, vk};
use smithay::utils::{Buffer, Physical, Rectangle, Transform};
use tracing::{debug, warn};

use super::device::Ext;
use super::geometry::{self, Mapping, Quad, Space};
use super::sync;
use super::target::{Target, VkFramebuffer};
use super::texture::VkTexture;
use super::{Pass, Upload, VkRenderer};
use crate::error::{Error, Result};
use beize_vulkan::barrier::{
    COLOR_WRITE, NONE, SAME, SHADER_READ, TRANSFER_READ, TRANSFER_WRITE, barrier, barriers,
};
use beize_vulkan::pipeline::{Attachment, Pipelines, SHADOW};

/// Round corners for what a frame draws (`VkFrame::set_rounding`), in the
/// framebuffer: the rectangle, the radius, the corners' squares.
struct Rounding {
    clip: [f32; 4],
    radius: f32,
    corners: [[f32; 4]; 4],
}

/// Whether `quad` reaches into the rectangle `r` (x, y, w, h).
fn overlaps(quad: &Quad, r: &[f32; 4]) -> bool {
    quad.pos[0] < r[0] + r[2]
        && r[0] < quad.pos[0] + quad.size[0]
        && quad.pos[1] < r[1] + r[3]
        && r[1] < quad.pos[1] + quad.size[1]
}

/// What a frame has gathered for its submission.
pub(super) struct Recorded<'a> {
    pub target: &'a mut Target,
    pub quads: Vec<Quad>,
    /// Per quad: drawn with blending.
    pub blend: Vec<bool>,
    pub textures: Vec<VkTexture>,
    /// The clients' dmabufs among `textures`.
    pub foreign: Vec<VkTexture>,
    /// Fences to wait for (`Frame::wait`).
    pub waits: Vec<SyncPoint>,
    pub family: u32,
}

pub struct VkFrame<'frame, 'buffer> {
    renderer: &'frame mut VkRenderer,
    recorded: Recorded<'frame>,
    space: Space,
    /// What is drawn now shows only inside this rounded rectangle
    /// (`set_rounding`), in the framebuffer.
    rounding: Option<Rounding>,
    finished: bool,
    _buffer: PhantomData<&'buffer ()>,
}

impl<'frame, 'buffer> VkFrame<'frame, 'buffer> {
    pub(super) fn new(
        renderer: &'frame mut VkRenderer,
        framebuffer: &'frame mut VkFramebuffer<'buffer>,
        transform: Transform,
    ) -> Result<Self> {
        let target: &'frame mut Target = framebuffer.0;
        let space = Space {
            framebuffer: (target.size.w, target.size.h).into(),
            transform,
        };
        let family = renderer.family;
        Ok(VkFrame {
            renderer,
            recorded: Recorded {
                target,
                quads: Vec::new(),
                blend: Vec::new(),
                textures: Vec::new(),
                foreign: Vec::new(),
                waits: Vec::new(),
                family,
            },
            space,
            rounding: None,
            finished: false,
            _buffer: PhantomData,
        })
    }

    fn push(&mut self, mut quad: Quad, mut blend: bool) {
        if let Some(r) = &self.rounding {
            quad.clip = r.clip;
            quad.radius = r.radius;
            // Its corners let what is below show through.
            blend |= r.corners.iter().any(|c| overlaps(&quad, c));
        }
        self.recorded.quads.push(quad);
        self.recorded.blend.push(blend);
    }

    /// Round corners: what is drawn from now on shows only inside `rect`
    /// (the output's space) with corners of `radius`, until it is set to
    /// None (ADR 0028 §2).
    pub fn set_rounding(&mut self, rounding: Option<(Rectangle<i32, Physical>, f32)>) {
        self.rounding = rounding.map(|(rect, radius)| {
            let fb = self.space.framebuffer_rect(rect);
            let (x, y, w, h) = (
                fb.loc.x as f32,
                fb.loc.y as f32,
                fb.size.w as f32,
                fb.size.h as f32,
            );
            let r = radius;
            Rounding {
                clip: [x, y, w, h],
                radius,
                corners: [
                    [x, y, r, r],
                    [x + w - r, y, r, r],
                    [x, y + h - r, r, r],
                    [x + w - r, y + h - r, r, r],
                ],
            }
        });
    }

    /// The shadow of `caster` (the output's space) with corners of
    /// `radius`, blurred by `sigma`, in `color` (premultiplied): in `dst`,
    /// cut to its `damage`, but not under the caster, which covers it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_shadow(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        caster: Rectangle<i32, Physical>,
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    ) {
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        // The caster but its corners, which let the shadow show.
        let covered = [
            Rectangle::new(
                (caster.loc.x + radius, caster.loc.y).into(),
                (caster.size.w - 2 * radius, caster.size.h).into(),
            ),
            Rectangle::new(
                (caster.loc.x, caster.loc.y + radius).into(),
                (caster.size.w, caster.size.h - 2 * radius).into(),
            ),
        ];
        let fb = self.space.framebuffer_rect(caster);
        let clip = [
            fb.loc.x as f32,
            fb.loc.y as f32,
            fb.size.w as f32,
            fb.size.h as f32,
        ];
        for part in Rectangle::subtract_rects_many(parts, covered) {
            let mut quad = geometry::quad(&self.space, part, None);
            quad.flags = SHADOW;
            quad.color = color;
            quad.clip = clip;
            quad.radius = radius as f32;
            quad.sigma = sigma;
            self.recorded.quads.push(quad);
            self.recorded.blend.push(true);
        }
    }

    fn uses(&mut self, texture: &VkTexture) {
        let known = |list: &[VkTexture]| list.iter().any(|t| Arc::ptr_eq(&t.0, &texture.0));
        if !known(&self.recorded.textures) {
            self.recorded.textures.push(texture.clone());
            if texture.0.dmabuf.is_some() {
                self.recorded.foreign.push(texture.clone());
            }
        }
    }

    fn end(&mut self) -> Result<SyncPoint> {
        self.finished = true;
        self.renderer.finish_frame(&mut self.recorded)
    }
}

impl Frame for VkFrame<'_, '_> {
    type Error = Error;
    type TextureId = VkTexture;

    fn context_id(&self) -> ContextId<VkTexture> {
        self.renderer.context.clone()
    }

    fn clear(&mut self, color: Color32F, at: &[Rectangle<i32, Physical>]) -> Result<()> {
        // A clear sets the colour, alpha included: no blending.
        for &rect in at {
            let mut quad = geometry::quad(&self.space, rect, None);
            quad.color = color.components();
            self.push(quad, false);
        }
        Ok(())
    }

    fn draw_solid(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        color: Color32F,
    ) -> Result<()> {
        let blend = color.a() < 1.0;
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        for part in parts {
            let mut quad = geometry::quad(&self.space, part, None);
            quad.color = color.components();
            self.push(quad, blend);
        }
        Ok(())
    }

    fn render_texture_from_to(
        &mut self,
        texture: &VkTexture,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        src_transform: Transform,
        alpha: f32,
    ) -> Result<()> {
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        if parts.is_empty() || dst.size.w <= 0 || dst.size.h <= 0 {
            return Ok(());
        }
        // Opaque where the texture has no alpha, or where the client says
        // so (as Smithay's GLES renderer splits it).
        let (opaque, blended) = if !texture.has_alpha() && alpha == 1.0 {
            (parts, Vec::new())
        } else if alpha != 1.0 || opaque_regions.is_empty() {
            (Vec::new(), parts)
        } else {
            let regions: Vec<_> = geometry::absolute(dst, opaque_regions).collect();
            let blended = Rectangle::subtract_rects_many(parts.iter().copied(), regions);
            let opaque = Rectangle::subtract_rects_many(parts, blended.iter().copied());
            (opaque, blended)
        };
        let mapping = Mapping {
            src,
            dst,
            transform: src_transform,
        };
        let size = texture.size();
        for (rects, blend) in [(opaque, false), (blended, true)] {
            for part in rects {
                let mut quad = geometry::quad(&self.space, part, Some((&mapping, size)));
                quad.color = [1.0, 1.0, 1.0, alpha];
                quad.texture = texture.0.gpu.slot;
                self.push(quad, blend);
            }
        }
        self.uses(texture);
        Ok(())
    }

    fn transformation(&self) -> Transform {
        self.space.transform
    }

    fn wait(&mut self, sync: &SyncPoint) -> Result<()> {
        self.recorded.waits.push(sync.clone());
        Ok(())
    }

    fn finish(mut self) -> Result<SyncPoint> {
        self.end()
    }
}

impl Drop for VkFrame<'_, '_> {
    fn drop(&mut self) {
        if !self.finished
            && let Err(e) = self.end()
        {
            warn!("a frame dropped unfinished: {e}");
        }
    }
}

/// The layout a filled target keeps between frames.
pub(super) fn target_layout(target: &Target) -> vk::ImageLayout {
    if target.dmabuf.is_some() {
        vk::ImageLayout::GENERAL
    } else {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
    }
}

/// Staging → textures, before anything draws them.
pub(super) fn record_uploads(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    staging: vk::Buffer,
    uploads: &[Upload],
    submission: u64,
) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for u in uploads {
        let filled = u.texture.0.filled.load(Ordering::Relaxed);
        let (old, src) = if filled {
            (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, SHADER_READ)
        } else {
            (vk::ImageLayout::UNDEFINED, NONE)
        };
        let image = u.texture.0.gpu.image;
        before.push(barrier(
            image,
            old,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            src,
            TRANSFER_WRITE,
            SAME,
        ));
        after.push(barrier(
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            TRANSFER_WRITE,
            SHADER_READ,
            SAME,
        ));
    }
    barriers(d, commands, &before);
    for u in uploads {
        // SAFETY: the regions lie in the staging buffer and the texture.
        unsafe {
            d.cmd_copy_buffer_to_image(
                commands,
                staging,
                u.texture.0.gpu.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &u.regions,
            )
        };
        u.texture.0.filled.store(true, Ordering::Relaxed);
        u.texture.used(submission);
    }
    barriers(d, commands, &after);
}

/// The compositor's own pictures (`VkRenderer::draw_picture`), each drawn
/// whole through its sRGB view, then ready for the frame to sample.
pub(super) fn record_passes(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    layouts: &Pipelines,
    pipelines: [vk::Pipeline; 2],
    passes: &[Pass],
    addresses: &[u64],
    submission: u64,
) {
    // Drawn whole: what it held goes, once earlier frames have read it.
    let before: Vec<_> = passes
        .iter()
        .map(|p| {
            barrier(
                p.texture.0.gpu.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                SHADER_READ,
                COLOR_WRITE,
                SAME,
            )
        })
        .collect();
    barriers(d, commands, &before);
    for (pass, &address) in passes.iter().zip(addresses) {
        let Some(view) = pass.texture.0.attachment else {
            continue;
        };
        let size = pass.texture.0.size;
        let attachment = Attachment {
            view,
            size: (size.w as u32, size.h as u32),
            load: false,
        };
        // SAFETY: the picture is in COLOR_ATTACHMENT_OPTIMAL (above), the
        // pipelines are its view's format's; the quads live in this frame's
        // buffer until the submission completes.
        unsafe { layouts.record(d, commands, pipelines, &attachment, address, &pass.blend) };
        pass.texture.0.filled.store(true, Ordering::Relaxed);
        pass.texture.used(submission);
    }
    let after: Vec<_> = passes
        .iter()
        .map(|p| {
            barrier(
                p.texture.0.gpu.image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                COLOR_WRITE,
                SHADER_READ,
                SAME,
            )
        })
        .collect();
    barriers(d, commands, &after);
}

/// The frame's quads, drawn into its target.
pub(super) fn record_frame(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    layouts: &Pipelines,
    pipelines: [vk::Pipeline; 2],
    quads: u64,
    frame: &mut Recorded<'_>,
    submission: u64,
) {
    let target = &*frame.target;
    let foreign = vk::QUEUE_FAMILY_FOREIGN_EXT;
    let own = frame.family;
    let shared_target = target.dmabuf.is_some();
    let old = if target.filled {
        target_layout(target)
    } else {
        vk::ImageLayout::UNDEFINED
    };
    let mut before = vec![barrier(
        target.image,
        old,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        if shared_target { NONE } else { COLOR_WRITE },
        COLOR_WRITE,
        if shared_target && target.filled {
            (foreign, own)
        } else {
            SAME
        },
    )];
    let mut after = Vec::new();
    for t in &frame.foreign {
        before.push(barrier(
            t.0.gpu.image,
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            NONE,
            SHADER_READ,
            (foreign, own),
        ));
        after.push(barrier(
            t.0.gpu.image,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::GENERAL,
            SHADER_READ,
            NONE,
            (own, foreign),
        ));
    }
    barriers(d, commands, &before);

    let (w, h) = (target.size.w as u32, target.size.h as u32);
    let attachment = Attachment {
        view: target.view,
        size: (w, h),
        load: target.filled,
    };
    // SAFETY: recording into this frame's command buffer, the target in
    // COLOR_ATTACHMENT_OPTIMAL (barrier above); the pipelines are its
    // format's, the quad buffer and textures live until the submission
    // completes.
    let draws = unsafe { layouts.record(d, commands, pipelines, &attachment, quads, &frame.blend) };
    debug!("{} quads in {draws} draws", frame.quads.len());

    // Kept for the next frame, or handed to KMS.
    let target = &*frame.target;
    if let Some(readback) = &target.readback {
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            });
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
        // SAFETY: the buffer holds the whole image (Readback::new).
        unsafe {
            d.cmd_copy_image_to_buffer(
                commands,
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                readback.buffer,
                &[region],
            )
        };
        after.push(barrier(
            target.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            TRANSFER_READ,
            COLOR_WRITE,
            SAME,
        ));
    } else if shared_target {
        after.push(barrier(
            target.image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::GENERAL,
            COLOR_WRITE,
            NONE,
            (own, foreign),
        ));
    }
    barriers(d, commands, &after);
    for t in &frame.textures {
        t.used(submission);
    }
}

/// Semaphores the submission waits for: the pending writes of the clients'
/// dmabufs it reads, and the fences given to `Frame::wait`.
pub(super) fn implicit_waits(
    d: &ash::Device,
    ext: &Ext,
    frame: &Recorded<'_>,
    _submission: u64,
) -> Vec<vk::Semaphore> {
    let mut fds = Vec::new();
    for t in &frame.foreign {
        let Some(dmabuf) = t.0.dmabuf.as_ref().and_then(|w| w.upgrade()) else {
            continue;
        };
        for handle in dmabuf.handles() {
            match sync::pending_writes(handle) {
                Ok(fd) => fds.push(fd),
                Err(e) => debug!("a dmabuf's pending writes: {e}"),
            }
        }
    }
    for point in &frame.waits {
        match point.export() {
            Some(fd) => fds.push(fd),
            None => {
                if let Err(e) = point.wait() {
                    warn!("waiting for a fence: {e}");
                }
            }
        }
    }
    let mut semaphores = Vec::new();
    for fd in fds {
        // SAFETY: plain creation; the semaphore is destroyed after the
        // submission (`Garbage::Semaphore`).
        let semaphore =
            match unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) } {
                Ok(s) => s,
                Err(e) => {
                    warn!("a semaphore: {e}");
                    continue;
                }
            };
        let info = vk::ImportSemaphoreFdInfoKHR::default()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::TEMPORARY)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
            .fd(fd.into_raw_fd());
        // SAFETY: on success Vulkan owns the fd (sync_file payloads are
        // always imported temporarily).
        match unsafe { ext.semaphore_fd.import_semaphore_fd(&info) } {
            Ok(()) => semaphores.push(semaphore),
            Err(e) => {
                warn!("importing a fence: {e}");
                // SAFETY: never used.
                unsafe { d.destroy_semaphore(semaphore, None) };
            }
        }
    }
    semaphores
}

/// An image copied to the CPU (`ExportMem`).
pub(super) struct Readout {
    pub image: vk::Image,
    pub layout: vk::ImageLayout,
    pub foreign: bool,
    pub buffer: vk::Buffer,
    pub region: Rectangle<i32, Buffer>,
    pub family: u32,
}

pub(super) fn record_readout(d: &ash::Device, commands: vk::CommandBuffer, r: &Readout) {
    let foreign = vk::QUEUE_FAMILY_FOREIGN_EXT;
    let (acquire, release) = if r.foreign {
        ((foreign, r.family), (r.family, foreign))
    } else {
        (SAME, SAME)
    };
    let all = (
        vk::PipelineStageFlags2::ALL_COMMANDS,
        vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
    );
    barriers(
        d,
        commands,
        &[barrier(
            r.image,
            r.layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            all,
            TRANSFER_READ,
            acquire,
        )],
    );
    let copy = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D {
            x: r.region.loc.x,
            y: r.region.loc.y,
            z: 0,
        })
        .image_extent(vk::Extent3D {
            width: r.region.size.w as u32,
            height: r.region.size.h as u32,
            depth: 1,
        });
    // SAFETY: the buffer holds the region (`read_image`).
    unsafe {
        d.cmd_copy_image_to_buffer(
            commands,
            r.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            r.buffer,
            &[copy],
        )
    };
    barriers(
        d,
        commands,
        &[barrier(
            r.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            r.layout,
            TRANSFER_READ,
            all,
            release,
        )],
    );
}
