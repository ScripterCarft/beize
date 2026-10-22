//! Host memory the GPU reads (quads, staging) or the CPU reads back:
//! mapped once for its whole life. Destroyed by its owner, after the GPU's
//! last use (`device::Garbage::Buffer`).

use ash::vk;

use crate::device::Shared;
use crate::error::{Context, Result, error};

pub struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub pointer: *mut u8,
    pub size: u64,
    /// The buffer's device address, if its usage asks for one.
    pub address: u64,
}

impl HostBuffer {
    /// A host-visible buffer, mapped; cached for the CPU to read back.
    pub fn new(
        shared: &Shared,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<HostBuffer> {
        let d = &shared.device;
        let address = usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
        // SAFETY: plain creation; on failure what was made is destroyed.
        unsafe {
            let buffer = d
                .create_buffer(
                    &vk::BufferCreateInfo::default().size(size).usage(usage),
                    None,
                )
                .context("buffer")?;
            let requirements = d.get_buffer_memory_requirements(buffer);
            let coherent =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let kind = if cached {
                shared
                    .memory_type(
                        requirements.memory_type_bits,
                        coherent | vk::MemoryPropertyFlags::HOST_CACHED,
                    )
                    .or_else(|_| shared.memory_type(requirements.memory_type_bits, coherent))
            } else {
                shared.memory_type(requirements.memory_type_bits, coherent)
            };
            let mut flags = vk::MemoryAllocateFlagsInfo::default()
                .flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
            let memory = kind.and_then(|kind| {
                let mut info = vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(kind);
                if address {
                    info = info.push_next(&mut flags);
                }
                d.allocate_memory(&info, None).context("buffer memory")
            });
            let memory = match memory {
                Ok(m) => m,
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    return Err(e);
                }
            };
            let mapped = d
                .bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| d.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()));
            let pointer = match mapped {
                Ok(p) => p as *mut u8,
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    d.free_memory(memory, None);
                    return Err(error(format!("mapping a buffer: {e}")));
                }
            };
            let address = if address {
                d.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer))
            } else {
                0
            };
            Ok(HostBuffer {
                buffer,
                memory,
                pointer,
                size,
                address,
            })
        }
    }
}
