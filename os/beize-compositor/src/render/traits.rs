//! Smithay's renderer traits for `VkRenderer`: binding targets, importing
//! shm and dmabufs, reading back.

use super::*;

impl VkRenderer {
    /// The formats and modifiers clients may send dmabufs in: what Vulkan
    /// can sample with an importable modifier. Without format modifiers
    /// none: a client cannot know Vulkan's row pitch, and shm serves.
    pub(super) fn query_dmabuf_formats(
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

    /// Copies `region` of an image to the CPU, waiting for it.
    pub(super) fn read_image(
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

    pub(super) fn import_new(&self, dmabuf: &Dmabuf) -> Result<VkTexture> {
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

    // Also done after each frame (`submit`, `free_finished`).
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

/// Whether Vulkan imports dmabufs of `format` with `modifier` for sampling.
pub(super) fn importable(phd: &PhysicalDevice, format: vk::Format, modifier: u64) -> bool {
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
