//! GPU memory. Beize's own images and buffers share large blocks through a
//! sub-allocator (gpu-allocator, which wgpu uses too), so making or freeing
//! one is bookkeeping, not a call into the kernel: on NVK each
//! `vkAllocateMemory` maps and each `vkFreeMemory` unmaps in the GPU's
//! address space and waits for it (Mesa `nvkmd_nouveau_va_free`, a
//! `DRM_NOUVEAU_VM_BIND` without the async flag). GL drivers do the same
//! inside Mesa, which is why Mutter and KWin never see it; the Vulkan spec
//! leaves it to the application (`maxMemoryAllocationCount`). Imported
//! memory (dmabufs) stays dedicated: its pages are the exporter's.
//!
//! Blocks start small and grow with use (`Blocks`), so an app holds little;
//! an empty block is given back unless it is a type's last, and freeing it
//! is a `vkFreeMemory` again – which is why renderers free after a frame's
//! commit, never within it (`Shared::collect`).

use std::collections::HashMap;

use ash::vk;
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};
use gpu_allocator::{AllocationSizes, AllocatorDebugSettings, MemoryLocation};

use crate::device::Shared;
use crate::error::{Result, error};

const MB: u64 = 1 << 20;

/// What an image or buffer is bound to; given back with `Shared::free` (or
/// through `Garbage`) once the GPU is done with it. A plain value like the
/// Vulkan handle it replaces: freeing it twice is a fault, as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// None of its own: another owner frees the memory.
    None,
    /// Memory of its own: an import (dmabuf).
    Own(vk::DeviceMemory),
    /// A place in one of the sub-allocator's blocks.
    Sub(u64),
}

/// Where a buffer lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// The CPU writes it, the GPU reads it: quads, staging.
    Upload,
    /// The GPU writes it, the CPU reads it back: cached where it can be.
    Readback,
}

/// How large the sub-allocator's blocks start and grow, in MiB (multiples
/// of 4, at most 256). An allocation larger than a block gets one of its
/// own.
#[derive(Clone, Copy, Debug)]
pub struct Blocks {
    pub device: (u64, u64),
    pub host: (u64, u64),
}

impl Blocks {
    /// The compositor: screen-sized pictures and blur chains fit a block,
    /// on a 4K screen too (3840×2160 RGBA is 33 MiB; larger would get a
    /// block of its own, a kernel call each time). gpu-allocator keeps a
    /// type's last block; VMA prefers 256 MiB.
    pub const COMPOSITOR: Blocks = Blocks {
        device: (64, 256),
        host: (8, 32),
    };
    /// An app: a few windows' images and its glyph atlas.
    pub const APP: Blocks = Blocks {
        device: (8, 64),
        host: (4, 16),
    };
}

/// The sub-allocator and what it has handed out, by number.
pub(crate) struct Pool {
    allocator: Option<Allocator>,
    live: HashMap<u64, Allocation>,
    next: u64,
}

impl Pool {
    pub(crate) fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical: vk::PhysicalDevice,
        blocks: Blocks,
    ) -> Result<Pool> {
        let sizes = AllocationSizes::new(blocks.device.0 * MB, blocks.host.0 * MB)
            .with_max_device_memblock_size(blocks.device.1 * MB)
            .with_max_host_memblock_size(blocks.host.1 * MB);
        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: physical,
            debug_settings: AllocatorDebugSettings::default(),
            // The quads are read through their device address.
            buffer_device_address: true,
            allocation_sizes: sizes,
        })
        .map_err(|e| error(format!("the GPU memory allocator: {e}")))?;
        Ok(Pool {
            allocator: Some(allocator),
            live: HashMap::new(),
            next: 1,
        })
    }

    fn allocate(&mut self, desc: &AllocationCreateDesc<'_>) -> Result<(u64, &Allocation)> {
        let allocator = self
            .allocator
            .as_mut()
            .ok_or_else(|| error("the GPU memory allocator is gone"))?;
        let allocation = allocator
            .allocate(desc)
            .map_err(|e| error(format!("GPU memory for {}: {e}", desc.name)))?;
        let id = self.next;
        self.next += 1;
        let allocation = self.live.entry(id).or_insert(allocation);
        Ok((id, allocation))
    }

    fn free(&mut self, id: u64) {
        if let Some(allocation) = self.live.remove(&id)
            && let Some(allocator) = self.allocator.as_mut()
            && let Err(e) = allocator.free(allocation)
        {
            tracing::warn!("freeing GPU memory: {e}");
        }
    }

    /// Gives every block back; before the device goes.
    pub(crate) fn close(&mut self) {
        for id in self.live.keys().copied().collect::<Vec<_>>() {
            self.free(id);
        }
        // Dropping the allocator frees its blocks.
        self.allocator = None;
    }
}

impl Shared {
    /// Memory for `image`, bound: on the GPU, in a block.
    pub fn bind_image(&self, image: vk::Image, name: &str) -> Result<Memory> {
        let d = &self.device;
        // SAFETY: the image belongs to this device.
        let requirements = unsafe { d.get_image_memory_requirements(image) };
        let mut pool = self.pool.lock().map_err(|_| error("GPU memory poisoned"))?;
        let (id, allocation) = pool.allocate(&AllocationCreateDesc {
            name,
            requirements,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        })?;
        // SAFETY: the allocation fits the image's requirements; neither is
        // bound or used yet.
        let bound = unsafe { d.bind_image_memory(image, allocation.memory(), allocation.offset()) };
        if let Err(e) = bound {
            pool.free(id);
            return Err(error(format!("binding {name}: {e}")));
        }
        Ok(Memory::Sub(id))
    }

    /// Memory for `buffer`, bound and mapped for the CPU: the pointer.
    pub fn bind_buffer(
        &self,
        buffer: vk::Buffer,
        location: Location,
        name: &str,
    ) -> Result<(Memory, *mut u8)> {
        let d = &self.device;
        // SAFETY: the buffer belongs to this device.
        let requirements = unsafe { d.get_buffer_memory_requirements(buffer) };
        let location = match location {
            Location::Upload => MemoryLocation::CpuToGpu,
            Location::Readback => MemoryLocation::GpuToCpu,
        };
        let mut pool = self.pool.lock().map_err(|_| error("GPU memory poisoned"))?;
        let (id, allocation) = pool.allocate(&AllocationCreateDesc {
            name,
            requirements,
            location,
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        })?;
        let pointer = allocation.mapped_ptr().map(|p| p.as_ptr() as *mut u8);
        // SAFETY: the allocation fits the buffer's requirements; neither is
        // bound or used yet.
        let bound =
            unsafe { d.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) };
        match (bound, pointer) {
            (Ok(()), Some(pointer)) => Ok((Memory::Sub(id), pointer)),
            (Err(e), _) => {
                pool.free(id);
                Err(error(format!("binding {name}: {e}")))
            }
            (Ok(()), None) => {
                pool.free(id);
                Err(error(format!("{name}: host memory that is not mapped")))
            }
        }
    }

    /// Gives `memory` back now: the GPU no longer uses it (or never did).
    pub fn free(&self, memory: Memory) {
        match memory {
            Memory::None => {}
            // SAFETY: the caller's promise: nothing uses it any more.
            Memory::Own(memory) => unsafe { self.device.free_memory(memory, None) },
            Memory::Sub(id) => {
                if let Ok(mut pool) = self.pool.lock() {
                    pool.free(id);
                }
            }
        }
    }
}
