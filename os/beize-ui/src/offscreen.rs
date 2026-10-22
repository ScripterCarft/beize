//! Drawing without a window: into an sRGB image of its own, read back to
//! the CPU. What the tests compare – the same renderer, glyphs and blending
//! a window has, without a compositor.

use ash::vk;
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::device::Garbage;
use beize_vulkan::error::Result;
use beize_vulkan::image::{self, own_image};

use crate::draw::Scene;
use crate::gpu::Gpu;
use crate::renderer::{After, Renderer, Target};

const FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;

pub struct Offscreen {
    renderer: Renderer,
    size: (u32, u32),
    image: vk::Image,
    view: vk::ImageView,
    memory: vk::DeviceMemory,
    readback: HostBuffer,
}

impl Offscreen {
    /// An image of `size` pixels to draw into.
    pub fn new(size: (u32, u32)) -> Result<Offscreen> {
        let renderer = Renderer::new(Gpu::new(None)?)?;
        let shared = &renderer.gpu.shared;
        let (image, memory) = own_image(
            shared,
            FORMAT,
            size,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let view = image::view(&shared.device, image, FORMAT, true);
        let readback = view.and_then(|view| {
            let bytes = u64::from(size.0) * u64::from(size.1) * 4;
            HostBuffer::new(shared, bytes, vk::BufferUsageFlags::TRANSFER_DST, true)
                .map(|buffer| (view, buffer))
                .inspect_err(|_| {
                    // SAFETY: never used.
                    unsafe { shared.device.destroy_image_view(view, None) }
                })
        });
        let (view, readback) = match readback {
            Ok(made) => made,
            Err(e) => {
                // SAFETY: never used.
                unsafe {
                    shared.device.destroy_image(image, None);
                    shared.device.free_memory(memory, None);
                }
                return Err(e);
            }
        };
        Ok(Offscreen {
            renderer,
            size,
            image,
            view,
            memory,
            readback,
        })
    }

    /// The GPU it draws on.
    pub fn gpu_name(&self) -> &str {
        &self.renderer.gpu.name
    }

    /// `scene` drawn: its pixels, row by row, as B, G, R, A in sRGB.
    pub fn draw(&mut self, scene: &Scene) -> Result<Vec<u8>> {
        let target = Target {
            image: self.image,
            view: self.view,
            format: FORMAT,
            size: self.size,
            after: After::Read(self.readback.buffer),
        };
        let submission = self.renderer.draw(scene, &target, None, None)?;
        self.renderer.gpu.shared.wait(submission)?;
        let bytes = (self.size.0 * self.size.1 * 4) as usize;
        let mut pixels = vec![0; bytes];
        // SAFETY: the copy is done (waited for); the mapping holds the
        // whole image.
        unsafe { std::ptr::copy_nonoverlapping(self.readback.pointer, pixels.as_mut_ptr(), bytes) };
        Ok(pixels)
    }
}

impl Drop for Offscreen {
    fn drop(&mut self) {
        let shared = &self.renderer.gpu.shared;
        // Destroyed once the device is idle (the renderer waits when it
        // goes, and the device's last owner).
        shared.defer(
            u64::MAX,
            Garbage::Image {
                image: self.image,
                view: self.view,
                memory: self.memory,
            },
        );
        shared.defer(
            u64::MAX,
            Garbage::Buffer {
                buffer: self.readback.buffer,
                memory: self.readback.memory,
            },
        );
    }
}
