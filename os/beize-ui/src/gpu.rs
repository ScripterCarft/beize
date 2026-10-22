//! The GPU as an app sees it: a Vulkan instance that makes Wayland
//! surfaces, beize-vulkan's device with a swapchain on top, and the quad
//! pipelines every window draws with (ADR 0027 §9).

use std::ffi::c_void;
use std::sync::Arc;

use ash::vk;
use beize_vulkan::device::{Extensions, Shared};
use beize_vulkan::error::{Context, Result, error};
use beize_vulkan::pipeline::Pipelines;
use tracing::info;

/// The loader and the instance, destroyed when the last device and
/// surface are gone (`Shared` keeps a reference).
pub struct Instance {
    /// Keeps libvulkan loaded for as long as the instance lives.
    _entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface: ash::khr::surface::Instance,
    pub wayland: ash::khr::wayland_surface::Instance,
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the last owner is gone: every device and surface made
        // from the instance is destroyed.
        unsafe { self.instance.destroy_instance(None) };
    }
}

pub struct Gpu {
    pub instance: Arc<Instance>,
    pub physical: vk::PhysicalDevice,
    pub shared: Arc<Shared>,
    pub queue: vk::Queue,
    pub family: u32,
    pub swapchain: ash::khr::swapchain::Device,
    pub pipelines: Pipelines,
    pub name: String,
}

impl Gpu {
    /// Vulkan on a GPU – a real one before lavapipe – that can show on
    /// `display` (a libwayland `wl_display`); without one, for drawing
    /// offscreen.
    pub fn new(display: Option<*mut c_void>) -> Result<Gpu> {
        // SAFETY: libvulkan.so.1 is the Khronos loader (the desktop's
        // vulkan-loader); nothing else of it is used before this.
        let entry = unsafe { ash::Entry::load() }.context("loading Vulkan")?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"Beize UI")
            .api_version(vk::API_VERSION_1_3);
        let extensions = [
            ash::khr::surface::NAME.as_ptr(),
            ash::khr::wayland_surface::NAME.as_ptr(),
        ];
        let info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&extensions);
        // SAFETY: the info lives until the call returns; the instance is
        // destroyed by `Instance`.
        let raw = unsafe { entry.create_instance(&info, None) }.context("creating Vulkan")?;
        let instance = Arc::new(Instance {
            surface: ash::khr::surface::Instance::new(&entry, &raw),
            wayland: ash::khr::wayland_surface::Instance::new(&entry, &raw),
            instance: raw,
            _entry: entry,
        });
        let (physical, name) = choose(&instance, display)?;
        let device_extensions = Extensions {
            required: &[ash::khr::swapchain::NAME],
            optional: &[],
        };
        // SAFETY: the physical device comes from this instance, which the
        // device keeps.
        let created = unsafe {
            beize_vulkan::device::create(
                &instance.instance,
                physical,
                Box::new(instance.clone()),
                &device_extensions,
            )
        }?;
        if let Some(display) = display
            && !presents(&instance, physical, created.family, display)
        {
            return Err(error(format!(
                "{name} cannot show its graphics queue's images on Wayland"
            )));
        }
        let shared = created.shared;
        let pipelines = Pipelines::new(&shared.device, shared.max_textures())?;
        let swapchain = ash::khr::swapchain::Device::new(&instance.instance, &shared.device);
        info!("drawing on {name}");
        Ok(Gpu {
            instance,
            physical,
            shared,
            queue: created.queue,
            family: created.family,
            swapchain,
            pipelines,
            name,
        })
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: nothing records any more; after the wait the GPU uses
        // nothing of the pipelines.
        unsafe {
            if let Err(e) = self.shared.device.device_wait_idle() {
                tracing::warn!("waiting for the GPU: {e}");
            }
        }
        self.pipelines.destroy(&self.shared.device);
    }
}

/// Whether queue family `family` of `physical` can show on `display`.
fn presents(
    instance: &Instance,
    physical: vk::PhysicalDevice,
    family: u32,
    display: *mut c_void,
) -> bool {
    // SAFETY: the physical device comes from this instance; `display` is
    // the caller's live libwayland display.
    unsafe {
        instance
            .wayland
            .get_physical_device_wayland_presentation_support(
                physical,
                family,
                &mut *display.cast(),
            )
    }
}

/// The physical device to draw on: Vulkan 1.3, a graphics queue that can
/// show on `display` if there is one; a GPU before a CPU (lavapipe).
fn choose(
    instance: &Instance,
    display: Option<*mut c_void>,
) -> Result<(vk::PhysicalDevice, String)> {
    // SAFETY: plain queries of this instance.
    let devices =
        unsafe { instance.instance.enumerate_physical_devices() }.context("listing the GPUs")?;
    let mut usable = Vec::new();
    for physical in devices {
        // SAFETY: the physical device comes from this instance.
        let props = unsafe { instance.instance.get_physical_device_properties(physical) };
        // SAFETY: as above.
        let families = unsafe {
            instance
                .instance
                .get_physical_device_queue_family_properties(physical)
        };
        let shows = families.iter().enumerate().any(|(i, f)| {
            f.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                && display.is_none_or(|display| presents(instance, physical, i as u32, display))
        });
        if props.api_version >= vk::API_VERSION_1_3 && shows {
            let name = props
                .device_name_as_c_str()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "the GPU".into());
            usable.push((
                props.device_type == vk::PhysicalDeviceType::CPU,
                physical,
                name,
            ));
        }
    }
    // false (a GPU) sorts before true (a CPU).
    usable.sort_by_key(|(cpu, _, _)| *cpu);
    usable
        .into_iter()
        .next()
        .map(|(_, physical, name)| (physical, name))
        .context("no Vulkan 1.3 device (that can show on this display)")
}
