//! Host memory the GPU reads (quads, staging) or the CPU reads back:
//! mapped once for its whole life. Destroyed by its owner, after the GPU's
//! last use (`device::Garbage::Buffer`).

use ash::vk;

use crate::device::Shared;
use crate::error::{Context, Result};
use crate::memory::{Location, Memory};

pub struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: Memory,
    pub pointer: *mut u8,
    pub size: u64,
    /// The buffer's device address, if its usage asks for one.
    pub address: u64,
}

impl HostBuffer {
    /// A host-visible buffer of the sub-allocator, mapped; cached for the
    /// CPU to read back.
    pub fn new(
        shared: &Shared,
        size: u64,
        usage: vk::BufferUsageFlags,
        cached: bool,
    ) -> Result<HostBuffer> {
        let d = &shared.device;
        let address = usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
        let location = if cached {
            Location::Readback
        } else {
            Location::Upload
        };
        // SAFETY: plain creation; on failure what was made is destroyed.
        unsafe {
            let buffer = d
                .create_buffer(
                    &vk::BufferCreateInfo::default().size(size).usage(usage),
                    None,
                )
                .context("buffer")?;
            let (memory, pointer) = match shared.bind_buffer(buffer, location, "a buffer") {
                Ok(bound) => bound,
                Err(e) => {
                    d.destroy_buffer(buffer, None);
                    return Err(e);
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
