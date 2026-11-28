//! Glass behind panels: the blur chain (Dual Kawase) and its passes, drawn
//! into the glass's own textures (ADR 0033 §6).

use super::*;

impl VkRenderer {
    /// The pictures the glass of a `size` source is drawn through
    /// (`material`), in `chain`: kept where they have the step's size, else
    /// made.
    pub fn prepare_glass(
        &mut self,
        size: Size<i32, Buffer>,
        material: &Material,
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
    pub(super) fn copy_texture_for(
        &mut self,
        target: &Target,
        size: Size<i32, Buffer>,
    ) -> Result<VkTexture> {
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

    pub(super) fn backdrop_pass(&mut self, texture: &VkTexture, quad: Quad) {
        self.passes.push(Pass {
            texture: texture.clone(),
            quads: vec![quad],
            blend: vec![false],
            backdrop: true,
            reads: Vec::new(),
        });
    }
}

/// The sizes of the glass's steps from a `size` source (`material`): Dual
/// Kawase halvings, then doublings back.
pub(super) fn glass_sizes(
    size: Size<i32, Buffer>,
    material: &Material,
) -> Result<Vec<Size<i32, Buffer>>> {
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
pub(super) fn glass_passes(
    source: &VkTexture,
    material: &Material,
    chain: &[VkTexture],
) -> Result<Vec<Pass>> {
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
            reads: vec![from],
        });
        from = texture.clone();
    }
    Ok(passes)
}

/// A quad over a whole picture of `size`, reading all of `source` (a
/// pass of the backdrop).
pub(super) fn pass_quad(size: Size<i32, Buffer>, source: Option<&VkTexture>) -> Quad {
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
