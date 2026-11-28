//! Pictures the compositor draws itself: a window's picture for its
//! animations, the wallpaper, masks and coverage textures.

use super::glass::pass_quad;
use super::*;

impl VkRenderer {
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
            reads: Vec::new(),
        });
        Ok(texture)
    }

    /// An 8-bit texture of `size` holding `values`, a byte a texel, rows
    /// tightly packed: a smoke's mask (`backdrop`).
    pub fn mask_texture(&mut self, values: &[u8], size: Size<i32, Buffer>) -> Result<VkTexture> {
        self.memory_texture(values, size.w.max(0) as usize, Fourcc::R8, size)
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
        self.backdrop_pass(&texture, quad);
        Ok(texture)
    }

    pub(super) fn picture_texture(&mut self, size: Size<i32, Buffer>) -> Result<VkTexture> {
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
}
