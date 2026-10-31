//! A frame: Smithay's draw calls become quads (`geometry`), cut to their
//! damage and split into opaque and blended parts; at the end they are
//! recorded as runs of one pipeline each – usually one to three draws.
//! Live glass stops the drawing where it lies (`Insert`): what the frame
//! drew behind it is copied and blurred, in the same submission.
//!
//! Images shared with others (the screen's dmabuf, clients' dmabufs) pass
//! between Vulkan and their owner by queue family ownership transfer from
//! and back to the foreign queue, as wlroots' Vulkan renderer does.

use std::marker::PhantomData;
use std::os::fd::{AsFd, IntoRawFd};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

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
use super::{Blur, Liquid, Pass, Upload, VkRenderer};
use crate::error::{Context, Error, Result, error};
use beize_vulkan::barrier::{
    COLOR_WRITE, NONE, SAME, SHADER_READ, TRANSFER_READ, TRANSFER_WRITE, barrier, barriers,
};
use beize_vulkan::pipeline::{Attachment, CLEAR, GLASS, Pipelines, SHADOW, corner_extent};

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
    /// Where the frame stops drawing to blur what it drew (live glass).
    pub inserts: Vec<Insert>,
}

/// Live glass in the frame (`VkFrame::blur_behind`): once the quads before
/// `at` are drawn, `region` of the target – what lies behind the glass, as
/// this frame drew it – is copied to `copy` and blurred by `passes`; then
/// the frame draws on.
pub(super) struct Insert {
    pub at: usize,
    pub region: Rectangle<i32, Physical>,
    pub copy: VkTexture,
    pub passes: Vec<Pass>,
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
                inserts: Vec::new(),
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
            // The squares the corners' curves lie in.
            let r = corner_extent(radius, w, h).ceil();
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
    /// cut to its `damage`, but not under `cover` – the window or glass,
    /// whose shadow falls offset below it – which hides it there: its
    /// quads leave out the straight parts, the shader the round corners.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_shadow(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        caster: Rectangle<i32, Physical>,
        cover: Rectangle<i32, Physical>,
        radius: i32,
        sigma: f32,
        color: [f32; 4],
    ) {
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        // The window but its corners, which let the shadow show. Corners
        // of half an odd side round up past the middle: that band is empty,
        // not negative – one of -1 px drew a line of shadow twice beside
        // a 75 px capsule.
        let c =
            corner_extent(radius as f32, cover.size.w as f32, cover.size.h as f32).ceil() as i32;
        let covered = [
            Rectangle::new(
                (cover.loc.x + c, cover.loc.y).into(),
                ((cover.size.w - 2 * c).max(0), cover.size.h).into(),
            ),
            Rectangle::new(
                (cover.loc.x, cover.loc.y + c).into(),
                (cover.size.w, (cover.size.h - 2 * c).max(0)).into(),
            ),
        ];
        let rect = |r: Rectangle<i32, Physical>| {
            let fb = self.space.framebuffer_rect(r);
            [
                fb.loc.x as f32,
                fb.loc.y as f32,
                fb.size.w as f32,
                fb.size.h as f32,
            ]
        };
        let (clip, hides) = (rect(caster), rect(cover));
        for part in Rectangle::subtract_rects_many(parts, covered) {
            let mut quad = geometry::quad(&self.space, part, None);
            quad.flags = SHADOW;
            quad.color = color;
            quad.clip = clip;
            quad.bottom = hides;
            quad.radius = radius as f32;
            quad.sigma = sigma;
            self.recorded.quads.push(quad);
            self.recorded.blend.push(true);
        }
    }

    /// Glass (ADR 0028 §6) in the rounded rectangle `shape` (the output's
    /// space) with corners of `radius`: `glass` – a blurred copy of what
    /// lies in `covers`, smaller – read where it lies, with `grain` and a
    /// `rim` of light (premultiplied; alpha 0: none); or, `liquid`, as
    /// liquid glass – its curved edge, tone and line of light. In `dst`, cut to its
    /// `damage`; opaque but at the corners.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_glass(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        shape: Rectangle<i32, Physical>,
        radius: f32,
        glass: &VkTexture,
        covers: Rectangle<i32, Physical>,
        grain: f32,
        rim: [f32; 4],
        liquid: Option<Liquid>,
    ) {
        let size = glass.0.size;
        if covers.size.w <= 0 || covers.size.h <= 0 {
            return;
        }
        let (sx, sy) = (
            f64::from(size.w) / f64::from(covers.size.w),
            f64::from(size.h) / f64::from(covers.size.h),
        );
        let (x, y) = (dst.loc.x - covers.loc.x, dst.loc.y - covers.loc.y);
        let mapping = Mapping {
            src: Rectangle::new(
                (f64::from(x) * sx, f64::from(y) * sy).into(),
                (f64::from(dst.size.w) * sx, f64::from(dst.size.h) * sy).into(),
            ),
            dst,
            transform: Transform::Normal,
        };
        let fb = self.space.framebuffer_rect(shape);
        let clip = [
            fb.loc.x as f32,
            fb.loc.y as f32,
            fb.size.w as f32,
            fb.size.h as f32,
        ];
        let r = corner_extent(radius, clip[2], clip[3]).ceil();
        let corners = [
            [clip[0], clip[1], r, r],
            [clip[0] + clip[2] - r, clip[1], r, r],
            [clip[0], clip[1] + clip[3] - r, r, r],
            [clip[0] + clip[2] - r, clip[1] + clip[3] - r, r, r],
        ];
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        for part in parts {
            let mut quad = geometry::quad(&self.space, part, Some((&mapping, size)));
            quad.flags = GLASS;
            quad.texture = glass.0.gpu.slot;
            quad.color = rim;
            quad.clip = clip;
            quad.radius = radius;
            quad.sigma = grain;
            if let Some(liquid) = liquid {
                // A pixel of the screen in the glass's normalised
                // coordinates: the refraction's and the tone's step.
                quad.flags |= CLEAR;
                quad.bottom = [
                    1.0 / covers.size.w as f32,
                    1.0 / covers.size.h as f32,
                    if liquid.ink { 1.0 } else { 0.0 },
                    if liquid.dark { 1.0 } else { 0.0 },
                ];
            }
            // Opaque but where its corners let what is below show.
            let blend = corners.iter().any(|c| overlaps(&quad, c));
            self.recorded.quads.push(quad);
            self.recorded.blend.push(blend);
        }
        self.uses(glass);
    }

    /// Live glass (ADR 0028 §6): blurs what this frame has drawn so far in
    /// `region` (the output's space) – what lies behind the glass, the frame
    /// drawing back to front – through `chain`, prepared for its size
    /// (`VkRenderer::prepare_glass`), in this frame's submission. `copy`
    /// holds the region, made or made again here to fit the target. The
    /// glass, `chain`'s last picture, is ready for the quads drawn after.
    pub fn blur_behind(
        &mut self,
        region: Rectangle<i32, Physical>,
        blur: &Blur,
        chain: &[VkTexture],
        copy: &mut Option<VkTexture>,
    ) -> Result<VkTexture> {
        let fb = self.space.framebuffer_rect(region);
        let target = &*self.recorded.target;
        let bounds = Rectangle::from_size((target.size.w, target.size.h).into());
        if self.space.transform != Transform::Normal || !bounds.contains_rect(fb) {
            return Err(error("live glass off its target"));
        }
        let size = (fb.size.w, fb.size.h).into();
        let fits = copy
            .as_ref()
            .is_some_and(|c| c.0.size == size && c.0.fourcc == target.fourcc());
        if !fits {
            *copy = Some(self.renderer.copy_texture_for(target, size)?);
        }
        let copy = copy.clone().context("live glass's copy")?;
        let passes = super::glass_passes(&copy, blur, chain)?;
        let glass = passes
            .last()
            .map(|p| p.texture.clone())
            .context("live glass without passes")?;
        self.recorded.inserts.push(Insert {
            at: self.recorded.quads.len(),
            region: fb,
            copy,
            passes,
        });
        Ok(glass)
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
        let began = Instant::now();
        // A clear sets the colour, alpha included: no blending.
        for &rect in at {
            let mut quad = geometry::quad(&self.space, rect, None);
            quad.color = color.components();
            self.push(quad, false);
        }
        self.renderer.spent.quads += began.elapsed();
        Ok(())
    }

    fn draw_solid(
        &mut self,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        color: Color32F,
    ) -> Result<()> {
        let began = Instant::now();
        let blend = color.a() < 1.0;
        let parts: Vec<_> = geometry::absolute(dst, damage).collect();
        for part in parts {
            let mut quad = geometry::quad(&self.space, part, None);
            quad.color = color.components();
            self.push(quad, blend);
        }
        self.renderer.spent.quads += began.elapsed();
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
        let began = Instant::now();
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
        self.renderer.spent.quads += began.elapsed();
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

/// The compositor's own pictures (`VkRenderer::draw_picture`, the
/// backdrop), each drawn whole through its sRGB view, then ready to be
/// sampled – by the frame or by the next pass (a blur reads the one
/// before). `pipelines`: the quads' and the backdrop's.
pub(super) fn record_passes(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    layouts: &Pipelines,
    pipelines: [[vk::Pipeline; 2]; 2],
    passes: &[Pass],
    addresses: &[u64],
    submission: u64,
) {
    for (pass, &address) in passes.iter().zip(addresses) {
        let Some(view) = pass.texture.0.attachment else {
            continue;
        };
        let image = pass.texture.0.gpu.image;
        // Drawn whole: what it held goes, once earlier reads are done.
        barriers(
            d,
            commands,
            &[barrier(
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                SHADER_READ,
                COLOR_WRITE,
                SAME,
            )],
        );
        let size = pass.texture.0.size;
        let attachment = Attachment {
            view,
            size: (size.w as u32, size.h as u32),
            load: false,
        };
        let pipelines = pipelines[usize::from(pass.backdrop)];
        // SAFETY: the picture is in COLOR_ATTACHMENT_OPTIMAL (above), the
        // pipelines are its view's format's; the quads live in this frame's
        // buffer until the submission completes, and what they read was
        // drawn by an earlier pass or is filled.
        unsafe { layouts.record(d, commands, pipelines, &attachment, address, &pass.blend) };
        barriers(
            d,
            commands,
            &[barrier(
                image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                COLOR_WRITE,
                SHADER_READ,
                SAME,
            )],
        );
        pass.texture.0.filled.store(true, Ordering::Relaxed);
        pass.texture.used(submission);
        for read in &pass.reads {
            read.used(submission);
        }
    }
}

/// Live glass in the frame (`Insert`): the target's region copied, then
/// blurred by its passes (`pass_pipelines`, their quads at `addresses`);
/// the target back to be drawn on.
#[allow(clippy::too_many_arguments)]
fn record_insert(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    layouts: &Pipelines,
    pass_pipelines: [[vk::Pipeline; 2]; 2],
    target: &Target,
    insert: &Insert,
    addresses: &[u64],
    submission: u64,
) {
    let copy = insert.copy.0.gpu.image;
    let copy_old = if insert.copy.0.filled.load(Ordering::Relaxed) {
        (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, SHADER_READ)
    } else {
        (vk::ImageLayout::UNDEFINED, NONE)
    };
    barriers(
        d,
        commands,
        &[
            barrier(
                target.image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                COLOR_WRITE,
                TRANSFER_READ,
                SAME,
            ),
            // Written whole: what it held goes, once earlier reads are done.
            barrier(
                copy,
                copy_old.0,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                copy_old.1,
                TRANSFER_WRITE,
                SAME,
            ),
        ],
    );
    let layers = vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    };
    let r = insert.region;
    let region = vk::ImageCopy::default()
        .src_subresource(layers)
        .src_offset(vk::Offset3D {
            x: r.loc.x,
            y: r.loc.y,
            z: 0,
        })
        .dst_subresource(layers)
        .extent(vk::Extent3D {
            width: r.size.w as u32,
            height: r.size.h as u32,
            depth: 1,
        });
    // SAFETY: the region lies within the target (`blur_behind`) and is the
    // copy's size; both are in the layouts named (barriers above); the copy
    // has the target's format.
    unsafe {
        d.cmd_copy_image(
            commands,
            target.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            copy,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        )
    };
    barriers(
        d,
        commands,
        &[
            barrier(
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                TRANSFER_READ,
                COLOR_WRITE,
                SAME,
            ),
            barrier(
                copy,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                TRANSFER_WRITE,
                SHADER_READ,
                SAME,
            ),
        ],
    );
    insert.copy.0.filled.store(true, Ordering::Relaxed);
    insert.copy.used(submission);
    record_passes(
        d,
        commands,
        layouts,
        pass_pipelines,
        &insert.passes,
        addresses,
        submission,
    );
}

/// The frame's quads, drawn into its target, with its live glass between
/// (`inserts`; their passes' quads at `insert_addresses`), the first of them
/// timed in `queries` (`gpu_time`, from its third query on). Returns the
/// number of draw calls.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_frame(
    d: &ash::Device,
    commands: vk::CommandBuffer,
    layouts: &Pipelines,
    pipelines: [vk::Pipeline; 2],
    pass_pipelines: [[vk::Pipeline; 2]; 2],
    quads: u64,
    insert_addresses: &[Vec<u64>],
    frame: &mut Recorded<'_>,
    submission: u64,
    queries: Option<(vk::QueryPool, usize)>,
) -> usize {
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
    let mut draws = 0;
    let mut start = 0;
    let quad_size = std::mem::size_of::<Quad>();
    let ends = frame
        .inserts
        .iter()
        .map(|i| (i.at, Some(i)))
        .chain([(frame.quads.len(), None)]);
    for (n, (end, insert)) in ends.enumerate() {
        if end > start {
            let attachment = Attachment {
                view: target.view,
                size: (w, h),
                // After the first piece, what the frame drew so far stays.
                load: target.filled || start > 0 || n > 0,
            };
            // SAFETY: recording into this frame's command buffer, the target
            // in COLOR_ATTACHMENT_OPTIMAL (barrier above, and after each
            // copy); the pipelines are its format's, the quad buffer and
            // textures live until the submission completes; `start..end`
            // lies within the frame's quads.
            draws += unsafe {
                layouts.record(
                    d,
                    commands,
                    pipelines,
                    &attachment,
                    quads + (start * quad_size) as u64,
                    &frame.blend[start..end],
                )
            };
        }
        start = end;
        if let Some(insert) = insert {
            let timed = queries.filter(|&(_, most)| n < most).map(|(pool, _)| pool);
            let stamp = |query: usize| {
                if let Some(pool) = timed {
                    // SAFETY: outside a render pass, in this frame's command
                    // buffer; the pool holds the query, reset at its start.
                    unsafe {
                        d.cmd_write_timestamp2(
                            commands,
                            vk::PipelineStageFlags2::ALL_COMMANDS,
                            pool,
                            (2 + 2 * n + query) as u32,
                        );
                    }
                }
            };
            stamp(0);
            record_insert(
                d,
                commands,
                layouts,
                pass_pipelines,
                target,
                insert,
                &insert_addresses[n],
                submission,
            );
            stamp(1);
        }
    }
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
    draws
}

/// Semaphores the submission waits for: the pending writes of the clients'
/// dmabufs it reads, and the fences given to `Frame::wait`; and how many of
/// the writes were still pending. A client's commit applies only once its
/// buffer is written (`timing`), so a pending write is later work of that
/// client: nouveau marks each submission a writer of every buffer shared
/// in its VM (`nouveau_exec.c`, `DMA_RESV_USAGE_WRITE`) – the frame then
/// waits for the client's next frame. Not for dmabufs synchronised
/// explicitly: their commit applied once the acquire point signalled
/// (`syncobj`), nothing is left to wait for.
pub(super) fn implicit_waits(
    d: &ash::Device,
    ext: &Ext,
    frame: &Recorded<'_>,
    _submission: u64,
) -> (Vec<vk::Semaphore>, usize) {
    let mut fds = Vec::new();
    let mut pending = 0;
    for t in frame.foreign.iter().filter(|t| !t.explicit()) {
        let Some(dmabuf) = t.0.dmabuf.as_ref().and_then(|w| w.upgrade()) else {
            continue;
        };
        // Counted per buffer, not per plane (a video's NV12, a modifier's
        // auxiliary plane).
        let mut writing = false;
        for handle in dmabuf.handles() {
            match sync::pending_writes(handle) {
                Ok(fd) => {
                    writing |= sync::pending(fd.as_fd());
                    fds.push(fd);
                }
                Err(e) => debug!("a dmabuf's pending writes: {e}"),
            }
        }
        pending += usize::from(writing);
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
    (semaphores, pending)
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
