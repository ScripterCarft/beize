//! The Vulkan device: Vulkan 1.3 with the features Beize's drawing is built
//! on (ADR 0027 §2), checked by name so a missing one says what it is.
//! Both drivers Beize ships have them all: NVK (1.4 from Turing, 1.3 on
//! Maxwell and Pascal) and lavapipe (1.4). The extensions on top are the
//! caller's: the compositor's dmabufs, Beize UI's swapchain.
//!
//! What the GPU may still use is freed only once it is done with it: every
//! submission signals the next value of one timeline semaphore, and objects
//! wait in `garbage` for the value of their last use.

use std::any::Any;
use std::ffi::{CStr, c_char};
use std::sync::{Arc, Mutex};

use ash::vk;
use tracing::warn;

use crate::error::{Context, Result, error};

/// Textures the bindless array holds at most.
const MAX_TEXTURES: u32 = 4096;

/// What waits for the GPU before it is destroyed.
pub enum Garbage {
    Image {
        image: vk::Image,
        view: vk::ImageView,
        memory: vk::DeviceMemory,
    },
    Buffer {
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    },
    Semaphore(vk::Semaphore),
    /// A second view of an image, destroyed before the image.
    View(vk::ImageView),
    /// A place in the texture array, free for the next texture.
    Slot(u32),
}

/// The device and what everything made from it shares; the last owner
/// (renderer, image or target) destroys it.
pub struct Shared {
    pub device: ash::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    /// Signalled with each submission's number.
    pub timeline: vk::Semaphore,
    garbage: Mutex<Vec<(u64, Garbage)>>,
    slots: Mutex<Slots>,
    /// The instance outlives the device (Vulkan requires it; Mesa frees its
    /// shader compiler's type cache with the last instance). Dropped after
    /// `Drop` has destroyed the device.
    _instance: Box<dyn Any + Send + Sync>,
}

struct Slots {
    free: Vec<u32>,
    next: u32,
    max: u32,
}

impl Shared {
    pub fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        (0..self.memory.memory_type_count)
            .find(|&i| {
                bits & (1 << i) != 0
                    && self.memory.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
            .context("no suitable Vulkan memory type")
    }

    /// The last submission the GPU has finished.
    pub fn completed(&self) -> u64 {
        // SAFETY: the semaphore belongs to this device.
        unsafe { self.device.get_semaphore_counter_value(self.timeline) }.unwrap_or(0)
    }

    /// Waits until the GPU has finished submission `value`.
    pub fn wait(&self, value: u64) -> Result<()> {
        if value == 0 || self.completed() >= value {
            return Ok(());
        }
        let semaphores = [self.timeline];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: the semaphore belongs to this device; the info lives
        // until the call returns.
        unsafe { self.device.wait_semaphores(&info, u64::MAX) }.context("waiting for the GPU")
    }

    /// Destroys `garbage` once submission `after` is done.
    pub fn defer(&self, after: u64, garbage: Garbage) {
        if let Ok(mut list) = self.garbage.lock() {
            list.push((after, garbage));
        }
    }

    /// Destroys what the GPU no longer uses.
    pub fn collect(&self) {
        let done = self.completed();
        let ready: Vec<Garbage> = match self.garbage.lock() {
            Ok(mut list) => {
                let (ready, waiting) = std::mem::take(&mut *list)
                    .into_iter()
                    .partition(|(after, _)| *after <= done);
                *list = waiting;
                ready.into_iter().map(|(_, g)| g).collect()
            }
            Err(_) => return,
        };
        for garbage in ready {
            self.destroy(garbage);
        }
    }

    fn destroy(&self, garbage: Garbage) {
        let d = &self.device;
        // SAFETY: the GPU is done with the object (its submission
        // completed, or the device is idle); null handles are ignored.
        unsafe {
            match garbage {
                Garbage::Image {
                    image,
                    view,
                    memory,
                } => {
                    d.destroy_image_view(view, None);
                    d.destroy_image(image, None);
                    d.free_memory(memory, None);
                }
                Garbage::Buffer { buffer, memory } => {
                    d.destroy_buffer(buffer, None);
                    d.free_memory(memory, None);
                }
                Garbage::Semaphore(semaphore) => d.destroy_semaphore(semaphore, None),
                Garbage::View(view) => d.destroy_image_view(view, None),
                Garbage::Slot(slot) => {
                    if let Ok(mut slots) = self.slots.lock() {
                        slots.free.push(slot);
                    }
                }
            }
        }
    }

    /// A free place in the texture array.
    pub fn slot(&self) -> Result<u32> {
        let mut slots = self
            .slots
            .lock()
            .map_err(|_| error("texture slots poisoned"))?;
        if let Some(slot) = slots.free.pop() {
            return Ok(slot);
        }
        if slots.next == slots.max {
            return Err(error(format!("more than {} textures", slots.max)));
        }
        slots.next += 1;
        Ok(slots.next - 1)
    }

    pub fn max_textures(&self) -> u32 {
        self.slots.lock().map(|s| s.max).unwrap_or(0)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: the last owner is gone: nothing records or submits any
        // more; after the wait the GPU uses nothing.
        unsafe {
            if let Err(e) = self.device.device_wait_idle() {
                warn!("waiting for the GPU: {e}");
            }
        }
        let garbage = self
            .garbage
            .get_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        for (_, g) in garbage {
            self.destroy(g);
        }
        // SAFETY: as above; every other object of the device is destroyed.
        unsafe {
            self.device.destroy_semaphore(self.timeline, None);
            self.device.destroy_device(None);
        }
    }
}

/// The extensions the caller asks for beyond the base.
pub struct Extensions<'a> {
    /// Without them there is no device.
    pub required: &'a [&'a CStr],
    /// Enabled where the device has them.
    pub optional: &'a [&'a CStr],
}

pub struct Created {
    pub shared: Arc<Shared>,
    pub queue: vk::Queue,
    pub family: u32,
    /// Nanoseconds per timestamp tick, if the queue has timestamps.
    pub timestamp_period: Option<f32>,
    /// Which of the optional extensions are enabled, in their order.
    pub optional: Vec<bool>,
}

/// The device on `physical`, with one graphics queue.
///
/// # Safety
/// `physical` belongs to `instance`, and `keep` keeps `instance` alive.
pub unsafe fn create(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    keep: Box<dyn Any + Send + Sync>,
    extensions: &Extensions<'_>,
) -> Result<Created> {
    let mut props12 = vk::PhysicalDeviceVulkan12Properties::default();
    let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut props12);
    // SAFETY: the physical device belongs to this instance (the caller's
    // promise); the chain lives until the call returns. The same for every
    // query below.
    unsafe { instance.get_physical_device_properties2(physical, &mut props) };
    let limits = props.properties.limits;
    let name = props
        .properties
        .device_name_as_c_str()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "the GPU".into());
    if props.properties.api_version < vk::API_VERSION_1_3 {
        return Err(error(format!("{name} has no Vulkan 1.3")));
    }
    // SAFETY: as above.
    let available = unsafe { instance.enumerate_device_extension_properties(physical) }
        .context("the device's extensions")?;
    let has = |wanted: &CStr| {
        available
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(wanted))
    };
    if let Some(missing) = extensions.required.iter().find(|e| !has(e)) {
        return Err(error(format!("{name} lacks {missing:?}")));
    }
    let optional: Vec<bool> = extensions.optional.iter().map(|e| has(e)).collect();
    let enabled: Vec<*const c_char> = extensions
        .required
        .iter()
        .chain(
            extensions
                .optional
                .iter()
                .zip(&optional)
                .filter_map(|(e, on)| on.then_some(e)),
        )
        .map(|e| e.as_ptr())
        .collect();

    // The features everything is built on, by name.
    let mut has12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut has13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut has12)
        .push_next(&mut has13);
    // SAFETY: as above.
    unsafe { instance.get_physical_device_features2(physical, &mut features) };
    let checks = [
        ("timelineSemaphore", has12.timeline_semaphore),
        ("bufferDeviceAddress", has12.buffer_device_address),
        ("scalarBlockLayout", has12.scalar_block_layout),
        ("runtimeDescriptorArray", has12.runtime_descriptor_array),
        (
            "descriptorBindingPartiallyBound",
            has12.descriptor_binding_partially_bound,
        ),
        (
            "descriptorBindingSampledImageUpdateAfterBind",
            has12.descriptor_binding_sampled_image_update_after_bind,
        ),
        (
            "descriptorBindingUpdateUnusedWhilePending",
            has12.descriptor_binding_update_unused_while_pending,
        ),
        (
            "shaderSampledImageArrayNonUniformIndexing",
            has12.shader_sampled_image_array_non_uniform_indexing,
        ),
        ("dynamicRendering", has13.dynamic_rendering),
        ("synchronization2", has13.synchronization2),
    ];
    if let Some((feature, _)) = checks.iter().find(|(_, on)| *on != vk::TRUE) {
        return Err(error(format!("{name} lacks the feature {feature}")));
    }
    let mut on12 = vk::PhysicalDeviceVulkan12Features::default()
        .timeline_semaphore(true)
        .buffer_device_address(true)
        .scalar_block_layout(true)
        .runtime_descriptor_array(true)
        .descriptor_binding_partially_bound(true)
        .descriptor_binding_sampled_image_update_after_bind(true)
        .descriptor_binding_update_unused_while_pending(true)
        .shader_sampled_image_array_non_uniform_indexing(true);
    let mut on13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true);

    let max = MAX_TEXTURES
        .min(props12.max_descriptor_set_update_after_bind_sampled_images)
        .min(limits.max_per_stage_descriptor_sampled_images);

    // SAFETY: as above.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
    let family = families
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .context("no graphics queue")?;
    let timestamp_period = (families[family].timestamp_valid_bits > 0
        && limits.timestamp_period > 0.0)
        .then_some(limits.timestamp_period);
    let family = family as u32;
    let priorities = [1.0];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(family)
        .queue_priorities(&priorities)];
    let info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues)
        .enabled_extension_names(&enabled)
        .push_next(&mut on12)
        .push_next(&mut on13);
    // SAFETY: the create info and its chain live until the call returns;
    // extensions and features were checked above.
    let device = unsafe { instance.create_device(physical, &info, None) }
        .context("creating the Vulkan device")?;
    // SAFETY: as above.
    let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
    let mut timeline_info =
        vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
    // SAFETY: a plain timeline semaphore, destroyed with the device; the
    // queue was requested at creation.
    let (timeline, queue) = unsafe {
        let timeline = device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(&mut timeline_info),
            None,
        );
        let timeline = match timeline {
            Ok(t) => t,
            Err(e) => {
                device.destroy_device(None);
                return Err(error(format!("timeline semaphore: {e}")));
            }
        };
        (timeline, device.get_device_queue(family, 0))
    };
    let shared = Arc::new(Shared {
        device,
        memory,
        timeline,
        garbage: Mutex::new(Vec::new()),
        slots: Mutex::new(Slots {
            free: Vec::new(),
            next: 0,
            max,
        }),
        _instance: keep,
    });
    Ok(Created {
        shared,
        queue,
        family,
        timestamp_period,
        optional,
    })
}
