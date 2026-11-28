//! Glass behind panels: the blur chains (Dual Kawase, Gauss) and their
//! passes, drawn into the glass's own textures (ADR 0028 §6).

use super::*;

impl VkRenderer {
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
        let blur = Blur::Kawase(material);
        self.prepare_glass(source.0.size, &blur, chain)?;
        // Drawn again in this submission: the passes queued before for these
        // pictures are superseded.
        self.passes
            .retain(|p| !chain.iter().any(|t| Arc::ptr_eq(&p.texture.0, &t.0)));
        let passes = glass_passes(source, &blur, chain)?;
        let glass = passes
            .last()
            .map(|p| p.texture.clone())
            .context("glass without passes")?;
        self.passes.extend(passes);
        Ok(glass)
    }

    /// The pictures the glass of a `size` source is drawn through (`blur`),
    /// in `chain`: kept where they have the step's size, else made.
    pub fn prepare_glass(
        &mut self,
        size: Size<i32, Buffer>,
        blur: &Blur,
        chain: &mut Vec<VkTexture>,
    ) -> Result<()> {
        let sizes = glass_sizes(size, blur)?;
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

/// A quad over a whole picture of `size`, reading all of `source` (a
/// pass of the backdrop).
/// The sizes of the glass's steps from a `size` source (`blur`): Dual
/// Kawase halvings, then doublings back; a Gaussian's two passes at the
/// source's size.
pub(super) fn glass_sizes(size: Size<i32, Buffer>, blur: &Blur) -> Result<Vec<Size<i32, Buffer>>> {
    let material = match blur {
        Blur::Kawase(material) => material,
        Blur::Gauss(gauss) if gauss.half => {
            let half = ((size.w + 1) / 2, (size.h + 1) / 2).into();
            return Ok(vec![half, half]);
        }
        Blur::Gauss(_) => return Ok(vec![size, size]),
    };
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
    blur: &Blur,
    chain: &[VkTexture],
) -> Result<Vec<Pass>> {
    use beize_vulkan::pipeline::{BLUR_DOWN, BLUR_UP, MATERIAL};
    let sizes = glass_sizes(source.0.size, blur)?;
    if chain.len() != sizes.len() || chain.iter().zip(&sizes).any(|(t, s)| t.0.size != *s) {
        return Err(error("the glass's pictures do not fit its source"));
    }
    let material = match blur {
        Blur::Kawase(material) => *material,
        Blur::Gauss(gauss) => return Ok(gauss_passes(source, gauss, chain)),
    };
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

/// A Gaussian's two passes of `source` into `chain`'s two pictures, each
/// of its size: across, then down – the first halving the source if `half`.
pub(super) fn gauss_passes(source: &VkTexture, gauss: &Gauss, chain: &[VkTexture]) -> Vec<Pass> {
    use beize_vulkan::pipeline::{GAUSS_ACROSS, GAUSS_DOWN};
    let mut passes = Vec::with_capacity(2);
    let mut from = source.clone();
    for (step, texture) in chain.iter().enumerate() {
        let mut quad = pass_quad(texture.0.size, Some(&from));
        quad.flags = if step == 0 { GAUSS_ACROSS } else { GAUSS_DOWN };
        // In the target's texels: half as many at half the size.
        quad.sigma = if gauss.half {
            gauss.sigma / 2.0
        } else {
            gauss.sigma
        };
        if gauss.half && step == 0 {
            quad.bottom[0] = 2.0;
        }
        if let Some(keeps) = gauss.keeps {
            // The columns it keeps in both passes – the first reads every
            // row, the second needs them –, its rows in the second.
            let picture = texture.0.size;
            let span = |from: i32, length: i32, most: i32| {
                let first = (from.div_euclid(2) - FROST_KEEPS).clamp(0, most);
                let end = ((from + length + 1).div_euclid(2) + FROST_KEEPS).clamp(first, most);
                (first as f32, (end - first) as f32)
            };
            let (x, w) = span(keeps.loc.x, keeps.size.w, picture.w);
            quad.pos[0] = x;
            quad.size[0] = w;
            if step == 1 {
                let (y, h) = span(keeps.loc.y, keeps.size.h, picture.h);
                quad.pos[1] = y;
                quad.size[1] = h;
            }
        }
        let mut reads = vec![from];
        if let Some(mask) = &gauss.mask {
            // A slot of the texture array: exact in an f32 below 2^24.
            quad.bottom[2] = mask.0.gpu.slot as f32;
            quad.bottom[3] = 1.0;
            reads.push(mask.clone());
        }
        passes.push(Pass {
            texture: texture.clone(),
            quads: vec![quad],
            blend: vec![false],
            backdrop: true,
            reads,
        });
        from = texture.clone();
    }
    passes
}

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
