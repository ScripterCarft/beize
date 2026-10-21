//! Vulkan, the only graphics API (ADR 0027 §2). For now it clears the
//! screen's images; the renderer for Smithay's `Renderer` grows from here.
//!
//! A screen image is a dmabuf: from the GPU's own Vulkan driver (allocated
//! by Smithay's `VulkanAllocator`, imported with its modifier), or a dumb
//! buffer of the KMS driver that lavapipe draws into (linear, ADR 0027 §6).
//! Where a dumb buffer is slow to read, or its row pitch is not lavapipe's,
//! lavapipe draws into an image of its own and the frame is copied over.

use std::rc::Rc;

use smithay::backend::allocator::Buffer;
use smithay::backend::allocator::dmabuf::{
    Dmabuf, DmabufMapping, DmabufMappingMode, DmabufSyncFlags,
};
use smithay::backend::vulkan::PhysicalDevice;
use smithay::reexports::ash::{self, vk};
use tracing::info;

use crate::error::{Context, Result, error};

/// The format of every screen image: XRGB8888 for KMS, B8G8R8A8 for Vulkan.
pub const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

struct Device {
    device: ash::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: every object of the device is gone: they hold an Rc of it.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
        }
    }
}

/// A Vulkan device with one queue and one command buffer.
pub struct Vk {
    device: Rc<Device>,
    queue: vk::Queue,
    pool: vk::CommandPool,
    commands: vk::CommandBuffer,
    fence: vk::Fence,
    external_fd: ash::khr::external_memory_fd::Device,
    /// `VK_EXT_image_drm_format_modifier`: dmabufs with any modifier and
    /// their own row pitch; without it (lavapipe), linear ones whose pitch
    /// is Vulkan's.
    modifiers: bool,
    name: String,
}

impl Vk {
    pub fn new(phd: &PhysicalDevice) -> Result<Vk> {
        let instance = phd.instance().handle();
        let modifiers = phd.has_device_extension(ash::ext::image_drm_format_modifier::NAME);
        let mut extensions = vec![
            ash::khr::external_memory_fd::NAME.as_ptr(),
            ash::ext::external_memory_dma_buf::NAME.as_ptr(),
        ];
        for needed in [
            ash::khr::external_memory_fd::NAME,
            ash::ext::external_memory_dma_buf::NAME,
        ] {
            if !phd.has_device_extension(needed) {
                return Err(error(format!("{} lacks {needed:?}", phd.name())));
            }
        }
        if modifiers {
            extensions.push(ash::ext::image_drm_format_modifier::NAME.as_ptr());
        }
        // SAFETY: the physical device belongs to this instance.
        let families =
            unsafe { instance.get_physical_device_queue_family_properties(phd.handle()) };
        let family = families
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .context("no graphics queue")? as u32;
        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions);
        // SAFETY: the create infos live until the call returns; the
        // extensions were checked above.
        let device = unsafe { instance.create_device(phd.handle(), &info, None) }
            .context("creating the Vulkan device")?;
        // SAFETY: as above.
        let memory = unsafe { instance.get_physical_device_memory_properties(phd.handle()) };
        let device = Rc::new(Device { device, memory });
        let d = &device.device;
        // SAFETY: the queue was requested at device creation; the pool,
        // buffer and fence are destroyed in Drop, before the device.
        let (queue, pool, commands, fence) = unsafe {
            let queue = d.get_device_queue(family, 0);
            let pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .context("command pool")?;
            let commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )
                .context("command buffer")?[0];
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .context("fence")?;
            (queue, pool, commands, fence)
        };
        let external_fd = ash::khr::external_memory_fd::Device::new(instance, d);
        Ok(Vk {
            device,
            queue,
            pool,
            commands,
            fence,
            external_fd,
            modifiers,
            name: phd.name().to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        let memory = &self.device.memory;
        (0..memory.memory_type_count)
            .find(|&i| {
                bits & (1 << i) != 0
                    && memory.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
            .context("no suitable Vulkan memory type")
    }

    fn image_info(width: u32, height: u32) -> vk::ImageCreateInfo<'static> {
        vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED)
    }

    /// The dmabuf itself as the target: drawn into without a copy.
    pub fn import(&self, dmabuf: &Dmabuf) -> Result<Target> {
        let d = &self.device.device;
        let (width, height) = (dmabuf.width(), dmabuf.height());
        let modifier: u64 = dmabuf.format().modifier.into();
        let strides: Vec<u32> = dmabuf.strides().collect();
        let offsets: Vec<u32> = dmabuf.offsets().collect();
        if dmabuf.num_planes() != 1 {
            return Err(error("screen images have one plane"));
        }
        let layouts = [vk::SubresourceLayout {
            offset: offsets[0] as u64,
            row_pitch: strides[0] as u64,
            ..Default::default()
        }];
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(modifier)
            .plane_layouts(&layouts);
        let mut info = Self::image_info(width, height).push_next(&mut external);
        if self.modifiers {
            info = info
                .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                .push_next(&mut explicit);
        } else {
            // Linear only, with Vulkan's own row pitch (checked below).
            if modifier != 0 && modifier != drm_modifier_invalid() {
                return Err(error("without format modifiers only linear images"));
            }
            info = info.tiling(vk::ImageTiling::LINEAR);
        }
        // SAFETY: the create info and its chain live until the call returns.
        let image = unsafe { d.create_image(&info, None) }.context("importing a dmabuf")?;
        let target = Target {
            device: self.device.clone(),
            image,
            memory: vk::DeviceMemory::null(),
            readback: None,
            width,
            height,
        };
        if !self.modifiers {
            let subresource = vk::ImageSubresource {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                array_layer: 0,
            };
            // SAFETY: a linear image of this device.
            let layout = unsafe { d.get_image_subresource_layout(image, subresource) };
            if layout.row_pitch != strides[0] as u64 || offsets[0] != 0 {
                return Err(error(format!(
                    "row pitch {} of the buffer is not Vulkan's {}",
                    strides[0], layout.row_pitch
                )));
            }
        }
        let fd = dmabuf
            .handles()
            .next()
            .context("dmabuf without a plane")?
            .try_clone_to_owned()
            .context("duplicating the dmabuf")?;
        let mut properties = vk::MemoryFdPropertiesKHR::default();
        // SAFETY: fd is an open dmabuf; the call only reads it.
        unsafe {
            self.external_fd.get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                &mut properties,
            )
        }
        .context("the dmabuf's memory types")?;
        // SAFETY: the image belongs to this device.
        let requirements = unsafe { d.get_image_memory_requirements(image) };
        let kind = self.memory_type(
            requirements.memory_type_bits & properties.memory_type_bits,
            vk::MemoryPropertyFlags::empty(),
        )?;
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(std::os::fd::IntoRawFd::into_raw_fd(fd));
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(kind)
            .push_next(&mut import)
            .push_next(&mut dedicated);
        // SAFETY: on success Vulkan owns the fd; on failure it is closed
        // with the error (VK_KHR_external_memory_fd: ownership passes only
        // on success, so a failed import leaks one fd, once).
        let memory = unsafe { d.allocate_memory(&allocate, None) }
            .context("importing the dmabuf's memory")?;
        let mut target = target;
        target.memory = memory;
        // SAFETY: memory was allocated for this image.
        unsafe { d.bind_image_memory(image, memory, 0) }.context("binding the dmabuf")?;
        Ok(target)
    }

    /// An image of lavapipe's own, copied into `dmabuf` after each frame.
    pub fn shadow(&self, dmabuf: &Dmabuf) -> Result<Target> {
        let d = &self.device.device;
        let (width, height) = (dmabuf.width(), dmabuf.height());
        let info = Self::image_info(width, height).tiling(vk::ImageTiling::OPTIMAL);
        // SAFETY: the create info lives until the call returns.
        let image = unsafe { d.create_image(&info, None) }.context("shadow image")?;
        let mut target = Target {
            device: self.device.clone(),
            image,
            memory: vk::DeviceMemory::null(),
            readback: None,
            width,
            height,
        };
        // SAFETY: the image belongs to this device.
        let requirements = unsafe { d.get_image_memory_requirements(image) };
        let kind = self.memory_type(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(kind);
        // SAFETY: plain allocation, bound to the image right after.
        unsafe {
            target.memory = d
                .allocate_memory(&allocate, None)
                .context("shadow memory")?;
            d.bind_image_memory(image, target.memory, 0)
                .context("binding the shadow image")?;
        }
        target.readback = Some(Readback::new(self, dmabuf, width, height)?);
        Ok(target)
    }

    /// Clears `target` to `color` (and copies it to its dmabuf).
    pub fn clear(&self, target: &Target, color: [f32; 4]) -> Result<()> {
        let d = &self.device.device;
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_general = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .image(target.image)
            .subresource_range(range);
        let written = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::MEMORY_READ)
            .image(target.image)
            .subresource_range(range);
        let c = self.commands;
        // SAFETY: one command buffer, used by one frame at a time: the
        // fence is waited for before this returns.
        unsafe {
            d.begin_command_buffer(c, &vk::CommandBufferBeginInfo::default())
                .context("recording")?;
            d.cmd_pipeline_barrier(
                c,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_general],
            );
            d.cmd_clear_color_image(
                c,
                target.image,
                vk::ImageLayout::GENERAL,
                &vk::ClearColorValue { float32: color },
                &[range],
            );
            d.cmd_pipeline_barrier(
                c,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[written],
            );
            if let Some(readback) = &target.readback {
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: target.width,
                        height: target.height,
                        depth: 1,
                    });
                d.cmd_copy_image_to_buffer(
                    c,
                    target.image,
                    vk::ImageLayout::GENERAL,
                    readback.buffer,
                    &[region],
                );
            }
            d.end_command_buffer(c).context("recording")?;
            let buffers = [c];
            d.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default().command_buffers(&buffers)],
                self.fence,
            )
            .context("submitting")?;
            d.wait_for_fences(&[self.fence], true, u64::MAX)
                .context("waiting for the frame")?;
            d.reset_fences(&[self.fence]).context("fence")?;
        }
        if let Some(readback) = &target.readback {
            readback.copy_out(target.width, target.height)?;
        }
        Ok(())
    }
}

impl Drop for Vk {
    fn drop(&mut self) {
        let d = &self.device.device;
        // SAFETY: nothing is in flight (every submit is waited for).
        unsafe {
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

fn drm_modifier_invalid() -> u64 {
    smithay::backend::allocator::Modifier::Invalid.into()
}

/// A screen image as Vulkan's target.
pub struct Target {
    device: Rc<Device>,
    image: vk::Image,
    memory: vk::DeviceMemory,
    readback: Option<Readback>,
    width: u32,
    height: u32,
}

impl Target {
    pub fn copies(&self) -> bool {
        self.readback.is_some()
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        let d = &self.device.device;
        // SAFETY: no command uses the image any more (frames are waited
        // for); null handles are ignored by Vulkan.
        unsafe {
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
        }
    }
}

/// Host memory the shadow image is copied to, and the dmabuf it goes to.
struct Readback {
    device: Rc<Device>,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pointer: *const u8,
    dmabuf: Dmabuf,
    mapping: DmabufMapping,
    stride: usize,
}

impl Readback {
    fn new(vk: &Vk, dmabuf: &Dmabuf, width: u32, height: u32) -> Result<Readback> {
        let d = &vk.device.device;
        let size = u64::from(width) * u64::from(height) * 4;
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        // SAFETY: plain buffer creation and allocation; the memory is bound
        // and mapped once and freed in Drop.
        let (buffer, memory, pointer) = unsafe {
            let buffer = d.create_buffer(&info, None).context("readback buffer")?;
            let requirements = d.get_buffer_memory_requirements(buffer);
            // Read by the CPU: cached if the device has it; coherent, so
            // no invalidation is needed after each frame.
            let coherent =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let kind = vk
                .memory_type(
                    requirements.memory_type_bits,
                    coherent | vk::MemoryPropertyFlags::HOST_CACHED,
                )
                .or_else(|_| vk.memory_type(requirements.memory_type_bits, coherent))?;
            let memory = d
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirements.size)
                        .memory_type_index(kind),
                    None,
                )
                .context("readback memory")?;
            d.bind_buffer_memory(buffer, memory, 0)
                .context("binding the readback buffer")?;
            let pointer = d
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
                .context("mapping the readback buffer")?;
            (buffer, memory, pointer as *const u8)
        };
        let mapping = dmabuf
            .map_plane(0, DmabufMappingMode::WRITE)
            .context("mapping the screen buffer")?;
        let stride = dmabuf.strides().next().context("dmabuf without a plane")? as usize;
        Ok(Readback {
            device: vk.device.clone(),
            buffer,
            memory,
            pointer,
            dmabuf: dmabuf.clone(),
            mapping,
            stride,
        })
    }

    fn copy_out(&self, width: u32, height: u32) -> Result<()> {
        let row = width as usize * 4;
        if self.stride < row || self.mapping.length() < self.stride * height as usize {
            return Err(error("the screen buffer is smaller than the image"));
        }
        self.dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
            .context("dmabuf sync")?;
        let target = self.mapping.ptr() as *mut u8;
        for y in 0..height as usize {
            // SAFETY: both rows lie within their mappings (sizes checked
            // above and at allocation: width * height * 4 bytes), which do
            // not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.pointer.add(y * row),
                    target.add(y * self.stride),
                    row,
                );
            }
        }
        self.dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
            .context("dmabuf sync")?;
        Ok(())
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        let d = &self.device.device;
        // SAFETY: no command uses the buffer any more.
        unsafe {
            d.destroy_buffer(self.buffer, None);
            d.free_memory(self.memory, None);
        }
    }
}

/// Logs which way frames reach the screen.
pub fn describe(vk: &Vk, target: &Target) {
    let way = if target.copies() {
        "into its own image, copied to the screen buffer"
    } else {
        "straight into the screen buffer"
    };
    info!("rendering with {} {way}", vk.name());
}
