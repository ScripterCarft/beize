//! Measures a GPU driver for ADR 0027, straight on KMS (no compositor):
//! the monitor's native resolution at its highest refresh rate, a
//! compositor-like load every frame (windows with rounded corners and
//! shadows, one live-blur panel), presented with FIFO. Reports frame pacing
//! (missed vblanks), the GPU time of the load and, where the kernel driver
//! reports them, temperature and power.
//!
//! Measurement tool on the branch measure/nvk only; run as root on a
//! console: `run0 beize-gpu-probe [seconds] [windows]`.

use std::error::Error;
use std::fs;
use std::io::Cursor;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use ash::vk;

type R<T> = Result<T, Box<dyn Error>>;

const SCENE: &[u8] = include_bytes!(concat!(env!("PROBE_SPV_DIR"), "/scene.spv"));
const DOWN: &[u8] = include_bytes!(concat!(env!("PROBE_SPV_DIR"), "/down.spv"));
const GLASS: &[u8] = include_bytes!(concat!(env!("PROBE_SPV_DIR"), "/glass.spv"));

const IN_FLIGHT: usize = 2;

/// `_IOW('d', 0x11, struct drm_auth)`: needs master, so it tells whether we
/// are (EACCES if not) – the test Mesa and libdrm use.
const DRM_IOCTL_AUTH_MAGIC: u64 = 0x4004_6411;
/// `_IO('d', 0x1e)`.
const DRM_IOCTL_SET_MASTER: u64 = 0x641e;
const EACCES: i32 = 13;

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    a: [f32; 4],
    panel: [i32; 4],
}

struct Target {
    image: vk::Image,
    view: vk::ImageView,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("beize-gpu-probe: {error}");
        std::process::exit(1);
    }
}

fn run() -> R<()> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(20);
    let windows: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(12);
    report_sensors("before");

    // KMS first, as a compositor: the connected connector, its card opened
    // by us, which makes us DRM master; Mesa gets the display from that fd
    // (VK_EXT_acquire_drm_display) instead of opening the card itself.
    let (card, connector, connector_id) = find_connector()?;
    let drm = fs::OpenOptions::new().read(true).write(true).open(&card)?;
    let fd = drm.as_raw_fd();
    let mut master = is_master(fd);
    if !master {
        // SAFETY: a DRM ioctl without arguments on our own fd.
        let set = unsafe { ioctl(fd, DRM_IOCTL_SET_MASTER) };
        let why = std::io::Error::last_os_error();
        master = is_master(fd);
        println!("KMS:     not master on open; SET_MASTER returned {set} ({why})");
    }
    println!("KMS:     {card}, {connector} (connector {connector_id}), DRM master: {master}");
    if !master {
        return Err("cannot become DRM master of the card – is a graphical program running?".into());
    }

    // SAFETY: loads the system's libvulkan.so.1; nothing else runs Vulkan.
    let entry = unsafe { ash::Entry::load()? };
    let app = vk::ApplicationInfo::default()
        .application_name(c"beize-gpu-probe")
        .api_version(vk::API_VERSION_1_3);
    let extensions = [
        ash::khr::surface::NAME.as_ptr(),
        ash::khr::display::NAME.as_ptr(),
        ash::ext::direct_mode_display::NAME.as_ptr(),
        ash::ext::acquire_drm_display::NAME.as_ptr(),
    ];
    // SAFETY: valid create info with static strings.
    let instance = unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::default()
                .application_info(&app)
                .enabled_extension_names(&extensions),
            None,
        )?
    };
    // SAFETY: the instance is valid for the rest of the program.
    let gpu = *unsafe { instance.enumerate_physical_devices()? }
        .first()
        .ok_or("no Vulkan device")?;

    let mut driver = vk::PhysicalDeviceDriverProperties::default();
    let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
    // SAFETY: gpu comes from this instance.
    unsafe { instance.get_physical_device_properties2(gpu, &mut props2) };
    let props = props2.properties;
    let name = props.device_name_as_c_str()?.to_string_lossy().into_owned();
    let driver_name = driver.driver_name_as_c_str()?.to_string_lossy().into_owned();
    let driver_info = driver.driver_info_as_c_str()?.to_string_lossy().into_owned();
    println!("GPU:     {name}");
    println!("Driver:  {driver_name} {driver_info}");

    // The connector as a Vulkan display, acquired with our master fd; then
    // its native resolution at the highest refresh rate.
    let display_ext = ash::khr::display::Instance::new(&entry, &instance);
    let acquire_ext = ash::ext::acquire_drm_display::Instance::new(&entry, &instance);
    // SAFETY: gpu comes from this instance; fd is our open card.
    let display = unsafe { acquire_ext.get_drm_display(gpu, fd, connector_id) }
        .map_err(|e| format!("vkGetDrmDisplayEXT: {e}"))?;
    // SAFETY: as above; the fd stays open until the end of run().
    unsafe { acquire_ext.acquire_drm_display(gpu, fd, display) }
        .map_err(|e| format!("vkAcquireDrmDisplayEXT: {e}"))?;
    let monitor = connector.clone();
    // SAFETY: the display comes from this device.
    let modes = unsafe { display_ext.get_display_mode_properties(gpu, display) }
        .map_err(|e| format!("vkGetDisplayModePropertiesKHR: {e}"))?;
    for m in &modes {
        let r = m.parameters.visible_region;
        println!(
            "  mode {}x{} @ {:.3} Hz",
            r.width,
            r.height,
            f64::from(m.parameters.refresh_rate) / 1000.0
        );
    }
    let mode = modes
        .iter()
        .max_by_key(|m| {
            let r = m.parameters.visible_region;
            (u64::from(r.width) * u64::from(r.height), m.parameters.refresh_rate)
        })
        .ok_or("the display has no mode")?;
    let extent = mode.parameters.visible_region;
    let hz = f64::from(mode.parameters.refresh_rate) / 1000.0;
    println!("Monitor: {monitor}, using {}x{} @ {hz:.3} Hz", extent.width, extent.height);

    // SAFETY: gpu comes from this instance.
    let planes = unsafe { display_ext.get_physical_device_display_plane_properties(gpu) }
        .map_err(|e| format!("vkGetPhysicalDeviceDisplayPlanePropertiesKHR: {e}"))?;
    let mut plane = None;
    for (index, props) in planes.iter().enumerate() {
        let index = u32::try_from(index)?;
        if props.current_display != vk::DisplayKHR::null() && props.current_display != display {
            continue;
        }
        // SAFETY: index is below the plane count.
        let supported = unsafe { display_ext.get_display_plane_supported_displays(gpu, index)? };
        if supported.contains(&display) {
            plane = Some((index, props.current_stack_index));
            break;
        }
    }
    let (plane_index, stack_index) = plane.ok_or("no plane for the display")?;
    // SAFETY: mode and plane belong to this display.
    let surface = unsafe {
        display_ext.create_display_plane_surface(
            &vk::DisplaySurfaceCreateInfoKHR::default()
                .display_mode(mode.display_mode)
                .plane_index(plane_index)
                .plane_stack_index(stack_index)
                .transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
                .alpha_mode(vk::DisplayPlaneAlphaFlagsKHR::OPAQUE)
                .global_alpha(1.0)
                .image_extent(extent),
            None,
        )?
    };
    let surface_ext = ash::khr::surface::Instance::new(&entry, &instance);

    // A queue for compute, transfer and presentation.
    // SAFETY: gpu comes from this instance.
    let families = unsafe { instance.get_physical_device_queue_family_properties(gpu) };
    let mut family = None;
    for (index, f) in families.iter().enumerate() {
        let index = u32::try_from(index)?;
        // SAFETY: surface and gpu come from this instance.
        let present = unsafe { surface_ext.get_physical_device_surface_support(gpu, index, surface)? };
        if present && f.queue_flags.contains(vk::QueueFlags::COMPUTE) {
            family = Some(index);
            break;
        }
    }
    let family = family.ok_or("no queue that computes and presents")?;
    let priorities = [1.0];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(family)
        .queue_priorities(&priorities)];
    let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
    // SAFETY: valid create info for this gpu.
    let device = unsafe {
        instance.create_device(
            gpu,
            &vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&device_extensions),
            None,
        )?
    };
    // SAFETY: the family was requested with one queue.
    let queue = unsafe { device.get_device_queue(family, 0) };

    // Swapchain: FIFO, the format the display offers in BGRA or RGBA.
    // SAFETY: surface and gpu come from this instance.
    let formats = unsafe { surface_ext.get_physical_device_surface_formats(gpu, surface)? };
    let format = formats
        .iter()
        .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
        .or_else(|| formats.first())
        .copied()
        .ok_or("the surface has no format")?;
    // SAFETY: as above.
    let caps = unsafe { surface_ext.get_physical_device_surface_capabilities(gpu, surface)? };
    let swapchain_ext = ash::khr::swapchain::Device::new(&instance, &device);
    let mut image_count = caps.min_image_count.max(3);
    if caps.max_image_count != 0 {
        image_count = image_count.min(caps.max_image_count);
    }
    // SAFETY: valid create info for this surface.
    let swapchain = unsafe {
        swapchain_ext.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(surface)
                .min_image_count(image_count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true),
            None,
        )?
    };
    // SAFETY: the swapchain was just created.
    let swapchain_images = unsafe { swapchain_ext.get_swapchain_images(swapchain)? };

    // The images of the load: the desktop, its blur at 1/2 and 1/4.
    // SAFETY: gpu comes from this instance.
    let memory = unsafe { instance.get_physical_device_memory_properties(gpu) };
    let (w, h) = (extent.width, extent.height);
    let scene = target(&device, &memory, w, h)?;
    let half = target(&device, &memory, w / 2, h / 2)?;
    let quarter = target(&device, &memory, w / 4, h / 4)?;
    // SAFETY: valid create info.
    let sampler = unsafe {
        device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
            None,
        )?
    };

    // One layout for every pass: a sampled image (binding 0), a storage image (1).
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE),
    ];
    // SAFETY: valid create infos.
    let set_layout = unsafe {
        device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
            None,
        )?
    };
    let push_range = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::COMPUTE)
        .size(u32::try_from(size_of::<Push>())?)];
    let set_layouts = [set_layout; 4];
    // SAFETY: valid create info.
    let layout = unsafe {
        device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts[..1])
                .push_constant_ranges(&push_range),
            None,
        )?
    };
    let pool_sizes = [
        vk::DescriptorPoolSize { ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, descriptor_count: 4 },
        vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 4 },
    ];
    // SAFETY: valid create info.
    let pool = unsafe {
        device.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(4).pool_sizes(&pool_sizes),
            None,
        )?
    };
    // SAFETY: the pool has room for four sets of this layout.
    let sets = unsafe {
        device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&set_layouts),
        )?
    };
    // Sets: 0 scene (writes the desktop), 1 desktop -> half, 2 half -> quarter,
    // 3 quarter -> the panel of the desktop.
    let wiring = [(None, &scene), (Some(&scene), &half), (Some(&half), &quarter), (Some(&quarter), &scene)];
    for (set, (sampled, storage)) in sets.iter().zip(wiring) {
        let storage_info = [vk::DescriptorImageInfo::default()
            .image_view(storage.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let mut writes = vec![vk::WriteDescriptorSet::default()
            .dst_set(*set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&storage_info)];
        let sampled_info;
        if let Some(sampled) = sampled {
            sampled_info = [vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(sampled.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(*set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&sampled_info),
            );
        }
        // SAFETY: the sets, views and sampler are alive and unused.
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }
    let pipelines = [SCENE, DOWN, DOWN, GLASS]
        .iter()
        .map(|spv| pipeline(&device, layout, spv))
        .collect::<R<Vec<_>>>()?;

    // Commands, synchronization, timestamps.
    // SAFETY: valid create infos throughout.
    let command_pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?
    };
    // SAFETY: as above.
    let commands = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(u32::try_from(IN_FLIGHT)?),
        )?
    };
    let mut fences = Vec::new();
    let mut acquired = Vec::new();
    for _ in 0..IN_FLIGHT {
        // SAFETY: as above.
        unsafe {
            fences.push(device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?);
            acquired.push(device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
        }
    }
    let mut rendered = Vec::new();
    for _ in &swapchain_images {
        // SAFETY: as above.
        rendered.push(unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)? });
    }
    // SAFETY: as above.
    let queries = unsafe {
        device.create_query_pool(
            &vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(u32::try_from(2 * IN_FLIGHT)?),
            None,
        )?
    };

    let panel = [
        i32::try_from(w)? * 72 / 100,
        i32::try_from(h)? * 5 / 100,
        i32::try_from(w)? * 25 / 100,
        i32::try_from(h)? * 60 / 100,
    ];
    let period = Duration::from_secs_f64(1.0 / hz);
    let started = Instant::now();
    let mut last = None;
    let mut intervals = Vec::new();
    let mut gpu_ms = Vec::new();
    let mut submitted = [false; IN_FLIGHT];
    let mut frame = 0usize;
    while started.elapsed() < Duration::from_secs(seconds) {
        let slot = frame % IN_FLIGHT;
        // SAFETY: the fence belongs to this device.
        unsafe { device.wait_for_fences(&[fences[slot]], true, u64::MAX)? };
        let now = Instant::now();
        if let Some(last) = last {
            intervals.push(now - last);
        }
        last = Some(now);
        if submitted[slot] {
            let mut stamps = [0u64; 2];
            // SAFETY: both queries of the slot were written by its finished submission.
            unsafe {
                device.get_query_pool_results(
                    queries,
                    u32::try_from(2 * slot)?,
                    &mut stamps,
                    vk::QueryResultFlags::TYPE_64,
                )?;
            }
            let ticks = stamps[1].saturating_sub(stamps[0]) as f64;
            gpu_ms.push(ticks * f64::from(props.limits.timestamp_period) / 1e6);
        }
        // SAFETY: the swapchain and semaphore are alive; the semaphore is unsignaled.
        let (index, _) = unsafe {
            swapchain_ext.acquire_next_image(swapchain, u64::MAX, acquired[slot], vk::Fence::null())?
        };
        let index = usize::try_from(index)?;
        let cb = commands[slot];
        let push = Push {
            a: [started.elapsed().as_secs_f32(), w as f32, h as f32, windows as f32],
            panel,
        };
        // SAFETY: everything recorded belongs to this device; the slot's previous
        // submission has finished (fence above).
        unsafe {
            device.reset_fences(&[fences[slot]])?;
            device.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let first = u32::try_from(2 * slot)?;
            device.cmd_reset_query_pool(cb, queries, first, 2);
            if frame == 0 {
                let to_general: Vec<_> = [&scene, &half, &quarter]
                    .iter()
                    .map(|t| image_barrier(t.image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL))
                    .collect();
                device.cmd_pipeline_barrier(
                    cb,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &to_general,
                );
            }
            // The previous frame's copy reads the desktop this frame overwrites.
            memory_barrier(&device, cb, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_READ);
            device.cmd_write_timestamp(cb, vk::PipelineStageFlags::TOP_OF_PIPE, queries, first);
            let bytes = std::slice::from_raw_parts((&raw const push).cast::<u8>(), size_of::<Push>());
            device.cmd_push_constants(cb, layout, vk::ShaderStageFlags::COMPUTE, 0, bytes);
            let groups = |x: u32, y: u32| (x.div_ceil(16), y.div_ceil(16));
            let passes = [
                groups(w, h),
                groups(w / 2, h / 2),
                groups(w / 4, h / 4),
                groups(u32::try_from(panel[2])?, u32::try_from(panel[3])?),
            ];
            for (pass, (gx, gy)) in passes.into_iter().enumerate() {
                if pass > 0 {
                    memory_barrier(&device, cb, vk::PipelineStageFlags::COMPUTE_SHADER, vk::AccessFlags::SHADER_WRITE);
                }
                device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, pipelines[pass]);
                device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, layout, 0, &[sets[pass]], &[]);
                device.cmd_dispatch(cb, gx, gy, 1);
            }
            // The desktop onto the swapchain image.
            let out = swapchain_images[index];
            let before = [
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(scene.image)
                    .subresource_range(color_range()),
                image_barrier(out, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE),
            ];
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &before,
            );
            let layers = vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1);
            let corner = vk::Offset3D {
                x: i32::try_from(w)?,
                y: i32::try_from(h)?,
                z: 1,
            };
            let blit = vk::ImageBlit::default()
                .src_subresource(layers)
                .src_offsets([vk::Offset3D::default(), corner])
                .dst_subresource(layers)
                .dst_offsets([vk::Offset3D::default(), corner]);
            device.cmd_blit_image(
                cb,
                scene.image,
                vk::ImageLayout::GENERAL,
                out,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::NEAREST,
            );
            let present = [image_barrier(out, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)];
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &present,
            );
            device.cmd_write_timestamp(cb, vk::PipelineStageFlags::BOTTOM_OF_PIPE, queries, first + 1);
            device.end_command_buffer(cb)?;

            let wait = [acquired[slot]];
            let stages = [vk::PipelineStageFlags::TRANSFER];
            let cbs = [cb];
            let signal = [rendered[index]];
            device.queue_submit(
                queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cbs)
                    .signal_semaphores(&signal)],
                fences[slot],
            )?;
            let swapchains = [swapchain];
            let indices = [u32::try_from(index)?];
            swapchain_ext.queue_present(
                queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signal)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            )?;
        }
        submitted[slot] = true;
        frame += 1;
    }
    // SAFETY: waits for the device before the process exits.
    unsafe { device.device_wait_idle()? };

    report(frame, &intervals, &mut gpu_ms, period, hz);
    report_sensors("after");
    // Mesa presents through this fd since the acquire: closed only now.
    drop(drm);
    Ok(())
}

/// The first connector with a monitor: its card's device node, its name
/// (`DP-1`) and its KMS object id, from sysfs.
fn find_connector() -> R<(String, String, u32)> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/drm")?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("card") && name.contains('-'))
        .collect();
    names.sort();
    for name in names {
        let dir = format!("/sys/class/drm/{name}");
        if fs::read_to_string(format!("{dir}/status"))?.trim() != "connected" {
            continue;
        }
        let id = fs::read_to_string(format!("{dir}/connector_id"))
            .map_err(|e| format!("{dir}/connector_id: {e}"))?
            .trim()
            .parse()?;
        let (card, connector) = name.split_once('-').ok_or("connector without a card")?;
        return Ok((format!("/dev/dri/{card}"), connector.to_owned(), id));
    }
    Err("no connector with a monitor in /sys/class/drm".into())
}

fn is_master(fd: i32) -> bool {
    let magic: u32 = 0;
    // SAFETY: AUTH_MAGIC reads one u32 from the pointer, which outlives the call.
    let result = unsafe { ioctl(fd, DRM_IOCTL_AUTH_MAGIC, &raw const magic) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(EACCES)
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

fn image_barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range())
}

/// Everything `src` wrote or read before is done before the next compute pass.
fn memory_barrier(device: &ash::Device, cb: vk::CommandBuffer, stage: vk::PipelineStageFlags, access: vk::AccessFlags) {
    let barrier = [vk::MemoryBarrier::default()
        .src_access_mask(access)
        .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE)];
    // SAFETY: cb is recording.
    unsafe {
        device.cmd_pipeline_barrier(
            cb,
            stage,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &barrier,
            &[],
            &[],
        );
    }
}

fn target(device: &ash::Device, memory: &vk::PhysicalDeviceMemoryProperties, w: u32, h: u32) -> R<Target> {
    // SAFETY: valid create info; memory is bound before use.
    unsafe {
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let needs = device.get_image_memory_requirements(image);
        let kind = (0..memory.memory_type_count)
            .find(|&i| {
                needs.memory_type_bits & (1 << i) != 0
                    && memory.memory_types[i as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .ok_or("no device-local memory for an image")?;
        let mem = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(needs.size)
                .memory_type_index(kind),
            None,
        )?;
        device.bind_image_memory(image, mem, 0)?;
        let view = device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM)
                .subresource_range(color_range()),
            None,
        )?;
        Ok(Target { image, view })
    }
}

fn pipeline(device: &ash::Device, layout: vk::PipelineLayout, spv: &[u8]) -> R<vk::Pipeline> {
    let words = ash::util::read_spv(&mut Cursor::new(spv))?;
    // SAFETY: valid SPIR-V from glslang; the module lives until the pipeline is made.
    unsafe {
        let module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?;
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::COMPUTE)
                    .module(module)
                    .name(c"main"),
            )
            .layout(layout);
        let pipeline = device
            .create_compute_pipelines(vk::PipelineCache::null(), &[info], None)
            .map_err(|(_, error)| error)?[0];
        device.destroy_shader_module(module, None);
        Ok(pipeline)
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[at]
}

fn report(frames: usize, intervals: &[Duration], gpu_ms: &mut [f64], period: Duration, hz: f64) {
    let mut ms: Vec<f64> = intervals.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(f64::total_cmp);
    let missed = intervals
        .iter()
        .filter(|d| d.as_secs_f64() > period.as_secs_f64() * 1.5)
        .count();
    let total: f64 = intervals.iter().map(Duration::as_secs_f64).sum();
    gpu_ms.sort_by(f64::total_cmp);
    println!();
    println!("Frames:      {frames}, {:.2} per second (display {hz:.3} Hz)", intervals.len() as f64 / total.max(1e-9));
    println!(
        "Frame time:  median {:.3} ms, p99 {:.3} ms, max {:.3} ms (budget {:.3} ms)",
        percentile(&ms, 0.5),
        percentile(&ms, 0.99),
        ms.last().copied().unwrap_or(0.0),
        period.as_secs_f64() * 1000.0
    );
    println!("Missed:      {missed} frames longer than 1.5 periods");
    println!(
        "GPU load:    median {:.3} ms, p99 {:.3} ms, max {:.3} ms per frame",
        percentile(gpu_ms, 0.5),
        percentile(gpu_ms, 0.99),
        gpu_ms.last().copied().unwrap_or(0.0)
    );
}

/// Temperature and power the GPU's kernel driver reports (hwmon), if any.
fn report_sensors(when: &str) {
    let Ok(cards) = fs::read_dir("/sys/class/drm") else { return };
    for card in cards.flatten() {
        let hwmons = card.path().join("device/hwmon");
        let Ok(entries) = fs::read_dir(&hwmons) else { continue };
        for hwmon in entries.flatten() {
            let read = |file: &str| fs::read_to_string(hwmon.path().join(file)).ok().and_then(|s| s.trim().parse::<f64>().ok());
            let name = fs::read_to_string(hwmon.path().join("name")).unwrap_or_default();
            let temp = read("temp1_input").map(|t| format!("{:.0} °C", t / 1000.0));
            let power = read("power1_input").map(|p| format!("{:.1} W", p / 1e6));
            println!(
                "Sensors {when}: {} {} {}",
                name.trim(),
                temp.unwrap_or_else(|| "no temperature".into()),
                power.unwrap_or_else(|| "no power reading".into())
            );
        }
    }
}
