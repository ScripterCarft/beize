//! Two pipelines per target format – opaque (no blending: on lavapipe
//! blending costs CPU time) and premultiplied blending – over one shader
//! pair, and one descriptor set holding every texture (bindless). Dynamic
//! rendering: no render pass or framebuffer objects. Mesa keeps compiled
//! pipelines in its disk cache, so a start compiles nothing twice.
//!
//! The backdrop's passes – the background and its blur, drawn once per
//! change (ADR 0028 §6) – have a fragment shader of their own
//! (`shaders/backdrop.frag`, `backdrop`), so the one of every frame stays
//! small.

use std::collections::HashMap;
use std::io::Cursor;

use ash::vk;

use crate::error::{Context, Result};

const VERTEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.vert.spv"));
const FRAGMENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.frag.spv"));
const BACKDROP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/backdrop.frag.spv"));

/// One quad as the shaders read it (`shaders/quad.glsl`, scalar layout).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quad {
    /// Top-left corner and size in framebuffer pixels.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Texel coordinate at the top-left corner, and its change across the
    /// quad's width and height (any of the eight transforms).
    pub uv: [f32; 2],
    pub uv_x: [f32; 2],
    pub uv_y: [f32; 2],
    /// 1 / the texture's size: texel coordinates to normalised ones.
    pub texel: [f32; 2],
    /// Premultiplied colour for solids and glyphs; for textures only
    /// `[3]`, the alpha.
    pub color: [f32; 4],
    /// Index into the texture array.
    pub texture: u32,
    pub flags: u32,
    /// A rounded rectangle in framebuffer pixels (x, y, width, height):
    /// with a `radius`, the quad shows only inside it – a window's round
    /// corners; for a `SHADOW`, the rectangle that casts it.
    pub clip: [f32; 4],
    pub radius: f32,
    /// A `SHADOW`'s blur, the Gaussian's standard deviation in pixels.
    pub sigma: f32,
    /// A `GRADIENT`'s premultiplied colour at the bottom of `clip`.
    pub bottom: [f32; 4],
}

/// A solid colour, no texture.
pub const SOLID: u32 = 1;
/// Texels 1:1 on whole pixels: fetched, not filtered – sharp by
/// construction (ADR 0027 §3).
pub const EXACT: u32 = 2;
/// The texture is a glyph's coverage (red, 8 bits): drawn in `color`
/// (ADR 0027 §9).
pub const MASK: u32 = 4;
/// The shadow of the rounded rectangle `clip` in `color`, blurred by
/// `sigma` (ADR 0028 §2): computed per pixel, no texture.
pub const SHADOW: u32 = 8;
/// A `SOLID` that runs from `color` at the top of `clip` to `bottom` at
/// its bottom (ADR 0028 §8, the apps' symbols), interpolated as CSS does
/// by default: premultiplied, in sRGB as encoded.
pub const GRADIENT: u32 = 16;
/// Base glass (ADR 0028 §6): the texture is the backdrop's blurred copy,
/// with grain of strength `sigma` (the noise's reach, as encoded: 1/255 is
/// one 8-bit step) and a rim of light in `color` (premultiplied; alpha 0:
/// none) along the rounded rectangle `clip`, whose corners of `radius` cut
/// it.
pub const GLASS: u32 = 32;
/// The backdrop's passes (`backdrop`): Beize's background for a target of
/// `clip`'s size, in the appearance `texture` (0 light, 1 dark).
pub const WALLPAPER: u32 = 64;
/// A pass of Dual Kawase down or up from the texture, its taps `sigma`
/// source texels apart.
pub const BLUR_DOWN: u32 = 128;
pub const BLUR_UP: u32 = 256;
/// With `BLUR_UP`: the glass's colour – saturation times `bottom[0]`, then
/// the tint `color` (linear, premultiplied) over it.
pub const MATERIAL: u32 = 512;

/// Apple's continuous corner (`shaders/quad.frag`, `EXTENT`): a corner of
/// `radius` reaches this many radii along each edge.
pub const CORNER_EXTENT: f32 = 1.48744;

/// How far a corner of `radius` reaches along the edges of a `w`×`h`
/// rectangle, as the shader draws it: the squares there show what is
/// below. Where the curve does not fit – a capsule, a dot – the corners
/// are circular.
pub fn corner_extent(radius: f32, w: f32, h: f32) -> f32 {
    let limit = w.min(h) / 2.0;
    if radius * CORNER_EXTENT <= limit {
        radius * CORNER_EXTENT
    } else {
        radius.min(limit)
    }
}

/// The push constants (`shaders/quad.glsl`, `Push`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Push {
    pub quads: u64,
    pub scale: [f32; 2],
}

/// Where quads are drawn: a colour attachment of `size` pixels.
pub struct Attachment {
    pub view: vk::ImageView,
    pub size: (u32, u32),
    /// Draws over what it holds; otherwise its content is undefined first.
    pub load: bool,
}

pub struct Pipelines {
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    pub layout: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    pub set: vk::DescriptorSet,
    vertex: vk::ShaderModule,
    fragment: vk::ShaderModule,
    backdrop: vk::ShaderModule,
    /// Per target format: opaque, blending.
    by_format: HashMap<vk::Format, [vk::Pipeline; 2]>,
    /// The backdrop's, per target format: opaque only.
    backdrop_by_format: HashMap<vk::Format, vk::Pipeline>,
}

impl Pipelines {
    pub fn new(d: &ash::Device, textures: u32) -> Result<Pipelines> {
        let mut p = Pipelines {
            sampler: vk::Sampler::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
            vertex: vk::ShaderModule::null(),
            fragment: vk::ShaderModule::null(),
            backdrop: vk::ShaderModule::null(),
            by_format: HashMap::new(),
            backdrop_by_format: HashMap::new(),
        };
        // On failure, Drop of the caller's renderer is not reached: destroy
        // what was made so far.
        match p.create(d, textures) {
            Ok(()) => Ok(p),
            Err(e) => {
                p.destroy(d);
                Err(e)
            }
        }
    }

    fn create(&mut self, d: &ash::Device, textures: u32) -> Result<()> {
        // SAFETY: plain object creation; every create info lives until its
        // call returns; the objects are destroyed in `destroy`.
        unsafe {
            self.sampler = d
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .context("sampler")?;
            let samplers = [self.sampler];
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(textures)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                    .immutable_samplers(&samplers),
            ];
            // Textures come and go while frames are in flight: a slot is
            // written only while no pending frame uses it.
            let flags = [
                vk::DescriptorBindingFlags::UPDATE_AFTER_BIND
                    | vk::DescriptorBindingFlags::PARTIALLY_BOUND
                    | vk::DescriptorBindingFlags::UPDATE_UNUSED_WHILE_PENDING,
                vk::DescriptorBindingFlags::empty(),
            ];
            let mut binding_flags =
                vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&flags);
            self.set_layout = d
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default()
                        .flags(vk::DescriptorSetLayoutCreateFlags::UPDATE_AFTER_BIND_POOL)
                        .bindings(&bindings)
                        .push_next(&mut binding_flags),
                    None,
                )
                .context("descriptor set layout")?;
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .size(std::mem::size_of::<Push>() as u32)];
            let set_layouts = [self.set_layout];
            self.layout = d
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&set_layouts)
                        .push_constant_ranges(&ranges),
                    None,
                )
                .context("pipeline layout")?;
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: textures,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLER,
                    descriptor_count: 1,
                },
            ];
            self.pool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND)
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .context("descriptor pool")?;
            self.set = d
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.pool)
                        .set_layouts(&set_layouts),
                )
                .context("descriptor set")?[0];
            self.vertex = module(d, VERTEX)?;
            self.fragment = module(d, FRAGMENT)?;
            self.backdrop = module(d, BACKDROP)?;
        }
        Ok(())
    }

    /// Puts a texture into its place in the array.
    pub fn write(&self, d: &ash::Device, slot: u32, view: vk::ImageView) {
        let images = [vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(0)
            .dst_array_element(slot)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&images);
        // SAFETY: the slot is free (no pending frame uses it,
        // UPDATE_UNUSED_WHILE_PENDING); the view lives until the slot is
        // given back after the GPU's last use.
        unsafe { d.update_descriptor_sets(&[write], &[]) };
    }

    /// The opaque and the blending pipeline for `format`.
    pub fn get(&mut self, d: &ash::Device, format: vk::Format) -> Result<[vk::Pipeline; 2]> {
        if let Some(p) = self.by_format.get(&format) {
            return Ok(*p);
        }
        let opaque = self.pipeline(d, format, self.fragment, false)?;
        let blend = match self.pipeline(d, format, self.fragment, true) {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: just created, never used.
                unsafe { d.destroy_pipeline(opaque, None) };
                return Err(e);
            }
        };
        self.by_format.insert(format, [opaque, blend]);
        Ok([opaque, blend])
    }

    /// The backdrop's pipeline for `format` (`shaders/backdrop.frag`), as
    /// the pair `record` takes: its passes never blend.
    pub fn get_backdrop(
        &mut self,
        d: &ash::Device,
        format: vk::Format,
    ) -> Result<[vk::Pipeline; 2]> {
        if let Some(&p) = self.backdrop_by_format.get(&format) {
            return Ok([p, p]);
        }
        let p = self.pipeline(d, format, self.backdrop, false)?;
        self.backdrop_by_format.insert(format, p);
        Ok([p, p])
    }

    fn pipeline(
        &self,
        d: &ash::Device,
        format: vk::Format,
        fragment: vk::ShaderModule,
        blend: bool,
    ) -> Result<vk::Pipeline> {
        let entry = c"main";
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.vertex)
                .name(entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment)
                .name(entry),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Wayland's buffers are premultiplied: src + dst * (1 - src.a).
        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(blend)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [format];
        let mut rendering =
            vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blending)
            .dynamic_state(&dynamic)
            .layout(self.layout)
            .push_next(&mut rendering);
        // SAFETY: every state struct lives until the call returns; the
        // shader modules and layout are valid.
        unsafe { d.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
            .map_err(|(_, e)| e)
            .context("graphics pipeline")
            .map(|p| p[0])
    }

    /// Records quads into `target`: `blend` says for each whether it
    /// blends, the quads lie at device address `quads`. Runs of one
    /// pipeline each, usually one to three draws; returns how many.
    ///
    /// # Safety
    /// `commands` is recording on `d`, `target` is in
    /// `COLOR_ATTACHMENT_OPTIMAL`, `pipelines` come from `get` for its
    /// format; the quads' buffer and every texture they name live until the
    /// submission completes.
    pub unsafe fn record(
        &self,
        d: &ash::Device,
        commands: vk::CommandBuffer,
        [opaque, blend]: [vk::Pipeline; 2],
        target: &Attachment,
        quads: u64,
        blending: &[bool],
    ) -> usize {
        let (w, h) = target.size;
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: w,
                height: h,
            },
        };
        let attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(target.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(if target.load {
                vk::AttachmentLoadOp::LOAD
            } else {
                vk::AttachmentLoadOp::DONT_CARE
            })
            .store_op(vk::AttachmentStoreOp::STORE)];
        let push = Push {
            quads,
            scale: [2.0 / w as f32, 2.0 / h as f32],
        };
        let mut draws = 0;
        // SAFETY: the caller's promise; every info lives until its call
        // returns.
        unsafe {
            d.cmd_begin_rendering(
                commands,
                &vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&attachments),
            );
            d.cmd_set_viewport(
                commands,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: w as f32,
                    height: h as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(commands, 0, &[area]);
            d.cmd_bind_descriptor_sets(
                commands,
                vk::PipelineBindPoint::GRAPHICS,
                self.layout,
                0,
                &[self.set],
                &[],
            );
            d.cmd_push_constants(
                commands,
                self.layout,
                vk::ShaderStageFlags::VERTEX,
                0,
                std::slice::from_raw_parts(
                    (&push as *const Push).cast::<u8>(),
                    std::mem::size_of::<Push>(),
                ),
            );
            let mut start = 0;
            while start < blending.len() {
                let mode = blending[start];
                let end = blending[start..]
                    .iter()
                    .position(|&b| b != mode)
                    .map_or(blending.len(), |n| start + n);
                d.cmd_bind_pipeline(
                    commands,
                    vk::PipelineBindPoint::GRAPHICS,
                    if mode { blend } else { opaque },
                );
                d.cmd_draw(commands, 4, (end - start) as u32, 0, start as u32);
                draws += 1;
                start = end;
            }
            d.cmd_end_rendering(commands);
        }
        draws
    }

    pub fn destroy(&mut self, d: &ash::Device) {
        // SAFETY: the device is idle (the renderer waits before this);
        // null handles are ignored.
        unsafe {
            for [opaque, blend] in self.by_format.drain().map(|(_, p)| p) {
                d.destroy_pipeline(opaque, None);
                d.destroy_pipeline(blend, None);
            }
            for p in self.backdrop_by_format.drain().map(|(_, p)| p) {
                d.destroy_pipeline(p, None);
            }
            d.destroy_shader_module(self.vertex, None);
            d.destroy_shader_module(self.fragment, None);
            d.destroy_shader_module(self.backdrop, None);
            d.destroy_descriptor_pool(self.pool, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            d.destroy_sampler(self.sampler, None);
        }
    }
}

/// A shader module from SPIR-V bytes (`include_bytes!` is not aligned to
/// `u32`, `read_spv` copies).
///
/// # Safety
/// `d` must be a valid device.
unsafe fn module(d: &ash::Device, bytes: &[u8]) -> Result<vk::ShaderModule> {
    let code = ash::util::read_spv(&mut Cursor::new(bytes)).context("SPIR-V")?;
    // SAFETY: valid SPIR-V (spirv-val at build time); the info lives until
    // the call returns.
    unsafe { d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None) }
        .context("shader module")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_draws_the_same_corner() {
        let shader = include_str!("../shaders/quad.frag");
        assert!(shader.contains(&format!("const float EXTENT = {CORNER_EXTENT};")));
    }

    #[test]
    fn the_shaders_know_the_same_flags() {
        let shared = include_str!("../shaders/quad.glsl");
        for (name, value) in [
            ("SOLID", SOLID),
            ("EXACT", EXACT),
            ("MASK", MASK),
            ("SHADOW", SHADOW),
            ("GRADIENT", GRADIENT),
            ("GLASS", GLASS),
            ("WALLPAPER", WALLPAPER),
            ("BLUR_DOWN", BLUR_DOWN),
            ("BLUR_UP", BLUR_UP),
            ("MATERIAL", MATERIAL),
        ] {
            assert!(
                shared.contains(&format!("const uint {name} = {value}u;")),
                "{name}"
            );
        }
    }

    #[test]
    fn a_quad_is_as_large_as_the_shaders_read_it() {
        // shaders/quad.glsl, scalar layout: 28 floats and words.
        assert_eq!(std::mem::size_of::<Quad>(), 28 * 4);
    }

    #[test]
    fn a_corner_that_does_not_fit_is_circular() {
        assert_eq!(corner_extent(16.0, 800.0, 600.0), 16.0 * CORNER_EXTENT);
        // A dot: a circle.
        assert_eq!(corner_extent(2.0, 4.0, 4.0), 2.0);
        // A capsule's ends are half circles.
        assert_eq!(corner_extent(20.0, 200.0, 30.0), 15.0);
    }
}
