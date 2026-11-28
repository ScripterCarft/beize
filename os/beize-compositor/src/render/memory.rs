//! Memory the CPU writes for the GPU: host buffers, the staging ring the
//! uploads go through, textures from memory and each frame's quads.

use super::*;

impl VkRenderer {
    /// A host-visible buffer, mapped; cached for the CPU to read back.
    pub(super) fn host_buffer(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<(vk::Buffer, beize_vulkan::memory::Memory, *const u8)> {
        let b = self.host_buffer_full(size, usage, cached)?;
        Ok((b.buffer, b.memory, b.pointer))
    }

    pub(super) fn host_buffer_full(
        &self,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<HostBuffer> {
        HostBuffer::new(&self.shared, size, usage, cached)
    }

    /// Room for `bytes` in the current frame's staging buffer.
    pub(super) fn stage(&mut self, bytes: u64) -> Result<(u64, *mut u8)> {
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
    pub(super) fn upload(
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
    pub(super) fn stage_rect(
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

    pub(super) fn queue_copies(&mut self, texture: &VkTexture, copies: Vec<vk::BufferImageCopy>) {
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
    pub(super) fn memory_texture(
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

    /// The pipelines for the frame's target and its quads, copied into the
    /// frame's buffer.
    /// `lists` one after another in frame `i`'s quad buffer, grown if need
    /// be; each list's device address.
    pub(super) fn write_quads(&mut self, i: usize, lists: &[&[Quad]]) -> Result<Vec<u64>> {
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
}
