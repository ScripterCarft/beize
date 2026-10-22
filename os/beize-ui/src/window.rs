//! A window: an xdg-shell toplevel, shown through a Vulkan swapchain on
//! the same libwayland display (Mesa's WSI: dmabufs on a GPU, shm on
//! lavapipe; FIFO paces it to the compositor's frames). Opaque, in the
//! size the compositor asks for – full screen until the shell arranges
//! windows (ADR 0027 §9).
//!
//! Two frames are in flight, as in the compositor: a frame's objects are
//! reused once the timeline says its last submission is done.

use std::ffi::c_void;
use std::time::Duration;

use ash::vk;
use beize_vulkan::barrier::{COLOR_WRITE, NONE, SAME, barrier, barriers};
use beize_vulkan::buffer::HostBuffer;
use beize_vulkan::error::{Context, Result, error};
use beize_vulkan::image;
use beize_vulkan::pipeline::Attachment;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry::WlRegistry,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::draw::Scene;
use crate::gpu::Gpu;

const IN_FLIGHT: usize = 2;
/// sRGB: the GPU blends in linear light and stores sRGB (`draw`).
const FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;
/// Until the compositor names a size.
const DEFAULT_SIZE: (u32, u32) = (800, 600);
/// The swapchain's image before drawing: the wait for its acquisition
/// happens at this stage (`draw`), nothing of it is read.
const ACQUIRED: beize_vulkan::barrier::Access = (
    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
    vk::AccessFlags2::NONE,
);

/// What happened while waiting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// The compositor asked for a size (the first, or a new one): draw.
    pub configured: bool,
    /// The user closed the window.
    pub closed: bool,
}

struct State {
    compositor: WlCompositor,
    surface: WlSurface,
    /// From the last toplevel configure; 0 means "choose".
    asked: (i32, i32),
    events: Events,
}

struct InFlight {
    commands: vk::CommandBuffer,
    /// Signalled when the swapchain hands out the frame's image.
    acquired: vk::Semaphore,
    quads: Option<HostBuffer>,
    /// The submission that last used it.
    submission: u64,
}

struct Swapchain {
    handle: vk::SwapchainKHR,
    size: (u32, u32),
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    /// Per image: signalled when its frame is drawn, for presenting.
    drawn: Vec<vk::Semaphore>,
}

pub struct Window {
    gpu: Gpu,
    _connection: Connection,
    queue: EventQueue<State>,
    state: State,
    _wm_base: XdgWmBase,
    _xdg_surface: XdgSurface,
    _toplevel: XdgToplevel,
    vk_surface: vk::SurfaceKHR,
    swapchain: Option<Swapchain>,
    /// A present said the swapchain no longer fits the surface.
    stale: bool,
    pool: vk::CommandPool,
    in_flight: Vec<InFlight>,
    current: usize,
    submitted: u64,
}

impl Window {
    /// Opens a window; it shows once the first `configured` is drawn.
    pub fn open(title: &str, app_id: &str) -> Result<Window> {
        let connection = Connection::connect_to_env().context("connecting to the compositor")?;
        let (globals, queue) =
            registry_queue_init::<State>(&connection).context("the compositor's globals")?;
        let qh = queue.handle();
        let compositor: WlCompositor = globals
            .bind(&qh, 4..=6, ())
            .context("the compositor has no wl_compositor")?;
        let wm_base: XdgWmBase = globals
            .bind(&qh, 1..=6, ())
            .context("the compositor has no xdg_wm_base")?;
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.into());
        toplevel.set_app_id(app_id.into());
        surface.commit();

        let display = connection.backend().display_ptr().cast::<c_void>();
        let gpu = Gpu::new(display)?;
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(display)
            .surface(surface.id().as_ptr().cast());
        // SAFETY: display and surface are libwayland's and live as long as
        // the connection, which outlives the Vulkan surface (`Drop`).
        let vk_surface = unsafe { gpu.instance.wayland.create_wayland_surface(&info, None) }
            .context("a Vulkan surface for the window")?;
        let mut window = Window {
            gpu,
            _connection: connection,
            queue,
            state: State {
                compositor,
                surface,
                asked: (0, 0),
                events: Events::default(),
            },
            _wm_base: wm_base,
            _xdg_surface: xdg_surface,
            _toplevel: toplevel,
            vk_surface,
            swapchain: None,
            stale: false,
            pool: vk::CommandPool::null(),
            in_flight: Vec::new(),
            current: 0,
            submitted: 0,
        };
        // From here on, Drop cleans up whatever was made.
        let d = &window.gpu.shared.device;
        // SAFETY: plain creation; destroyed in Drop.
        window.pool = unsafe {
            d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(window.gpu.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .context("command pool")?;
        for _ in 0..IN_FLIGHT {
            let frame = window.in_flight_new()?;
            window.in_flight.push(frame);
        }
        Ok(window)
    }

    fn in_flight_new(&self) -> Result<InFlight> {
        let d = &self.gpu.shared.device;
        // SAFETY: plain creation; destroyed in Drop (the command buffer
        // with its pool).
        unsafe {
            let commands = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .context("command buffer")?[0];
            let acquired = d
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .context("semaphore")?;
            Ok(InFlight {
                commands,
                acquired,
                quads: None,
                submission: 0,
            })
        }
    }

    /// The GPU the window draws on.
    pub fn gpu_name(&self) -> &str {
        &self.gpu.name
    }

    /// The size the next frame is drawn in.
    pub fn size(&self) -> (u32, u32) {
        match self.state.asked {
            (w, h) if w > 0 && h > 0 => (w as u32, h as u32),
            _ => DEFAULT_SIZE,
        }
    }

    /// Handles what the compositor sent, waiting at most `timeout` for it
    /// (`None`: until something comes).
    pub fn dispatch(&mut self, timeout: Option<Duration>) -> Result<Events> {
        let queue = &mut self.queue;
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        queue.flush().context("sending to the compositor")?;
        if let Some(guard) = queue.prepare_read() {
            let fd = guard.connection_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            let timeout = timeout
                .map(Timespec::try_from)
                .transpose()
                .context("timeout")?;
            let ready = poll(&mut fds, timeout.as_ref()).context("poll")?;
            if ready > 0 {
                guard.read().context("reading from the compositor")?;
            }
        }
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        Ok(std::mem::take(&mut self.state.events))
    }

    /// Draws `scene` and shows it with the compositor's next frame. The
    /// scene covers the whole window: what it leaves is undefined.
    pub fn draw(&mut self, scene: &Scene) -> Result<()> {
        let size = self.size();
        for _ in 0..2 {
            if self.stale || self.swapchain.as_ref().is_none_or(|s| s.size != size) {
                self.recreate(size)?;
            }
            match self.draw_once(scene)? {
                true => return Ok(()),
                // Out of date before drawing: once more with a new one.
                false => self.stale = true,
            }
        }
        Err(error("the swapchain stays out of date"))
    }

    /// One frame; false if the swapchain was out of date to begin with.
    fn draw_once(&mut self, scene: &Scene) -> Result<bool> {
        let shared = self.gpu.shared.clone();
        let d = &shared.device;
        let i = self.current;
        shared.wait(self.in_flight[i].submission)?;
        shared.collect();
        let swapchain = self.swapchain.as_ref().context("no swapchain")?;
        let acquired = self.in_flight[i].acquired;
        // SAFETY: the swapchain is this device's; the semaphore is unsignalled
        // (its last wait, by this frame's previous submission, is done).
        let acquire = unsafe {
            self.gpu.swapchain.acquire_next_image(
                swapchain.handle,
                u64::MAX,
                acquired,
                vk::Fence::null(),
            )
        };
        let index = match acquire {
            Ok((index, _suboptimal)) => index as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(e) => return Err(error(format!("acquiring a swapchain image: {e}"))),
        };
        let (image, view, drawn) = (
            swapchain.images[index],
            swapchain.views[index],
            swapchain.drawn[index],
        );
        let target = Attachment {
            view,
            size: swapchain.size,
            load: false,
        };
        let handle = swapchain.handle;
        let quads = self.upload_quads(i, scene)?;
        let pipelines = self.gpu.pipelines.get(d, FORMAT)?;
        let commands = self.in_flight[i].commands;
        let submission = self.submitted + 1;
        // SAFETY: the command buffer is this frame's and free (waited for
        // above); the image is acquired and stays so until presented.
        unsafe {
            d.reset_command_buffer(commands, vk::CommandBufferResetFlags::empty())
                .context("resetting commands")?;
            d.begin_command_buffer(
                commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .context("recording")?;
            barriers(
                d,
                commands,
                &[barrier(
                    image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    ACQUIRED,
                    COLOR_WRITE,
                    SAME,
                )],
            );
            let draws =
                self.gpu
                    .pipelines
                    .record(d, commands, pipelines, &target, quads, &scene.blend);
            debug!("{} quads in {draws} draws", scene.len());
            barriers(
                d,
                commands,
                &[barrier(
                    image,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    COLOR_WRITE,
                    NONE,
                    SAME,
                )],
            );
            d.end_command_buffer(commands).context("recording")?;
        }
        let waits = [vk::SemaphoreSubmitInfo::default()
            .semaphore(acquired)
            .stage_mask(ACQUIRED.0)];
        let signals = [
            vk::SemaphoreSubmitInfo::default()
                .semaphore(drawn)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
            vk::SemaphoreSubmitInfo::default()
                .semaphore(shared.timeline)
                .value(submission)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
        ];
        let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(commands)];
        let info = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .command_buffer_infos(&buffers)
            .signal_semaphore_infos(&signals);
        // SAFETY: everything the commands use lives until the submission
        // completes (the quad buffer is this frame's).
        unsafe { d.queue_submit2(self.gpu.queue, &[info], vk::Fence::null()) }
            .context("submitting")?;
        self.submitted = submission;
        self.in_flight[i].submission = submission;
        self.current = (i + 1) % IN_FLIGHT;

        let swapchains = [handle];
        let indices = [index as u32];
        let wait = [drawn];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the image was acquired above and is drawn when `drawn`
        // is signalled.
        match unsafe { self.gpu.swapchain.queue_present(self.gpu.queue, &present) } {
            Ok(false) => {}
            // Suboptimal: shown, but the next frame gets a new swapchain.
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale = true,
            Err(e) => return Err(error(format!("presenting: {e}"))),
        }
        Ok(true)
    }

    /// The scene's quads in frame `i`'s buffer; its device address.
    fn upload_quads(&mut self, i: usize, scene: &Scene) -> Result<u64> {
        let bytes = std::mem::size_of_val(scene.quads.as_slice()) as u64;
        if bytes == 0 {
            return Ok(0);
        }
        let shared = &self.gpu.shared;
        let frame = &mut self.in_flight[i];
        if frame.quads.as_ref().is_none_or(|b| b.size < bytes) {
            let new = HostBuffer::new(
                shared,
                bytes.next_power_of_two().max(16 << 10),
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                false,
            )?;
            if let Some(old) = frame.quads.replace(new) {
                // This frame's last use is done (`draw_once` waited).
                shared.defer(
                    0,
                    beize_vulkan::device::Garbage::Buffer {
                        buffer: old.buffer,
                        memory: old.memory,
                    },
                );
            }
        }
        let quads = frame.quads.as_ref().context("quads")?;
        // SAFETY: the buffer holds `bytes` (grown above); no submission
        // reads it (`draw_once` waited for this frame's last).
        unsafe {
            std::ptr::copy_nonoverlapping(
                scene.quads.as_ptr().cast::<u8>(),
                quads.pointer,
                bytes as usize,
            )
        };
        Ok(quads.address)
    }

    /// A swapchain of `size` (the surface's own size where it has one),
    /// replacing the old one.
    fn recreate(&mut self, size: (u32, u32)) -> Result<()> {
        let shared = self.gpu.shared.clone();
        let d = &shared.device;
        // A new swapchain is rare (configure): the old one goes once nothing
        // uses it any more, presents included.
        // SAFETY: nothing records at the moment.
        unsafe { d.device_wait_idle() }.context("waiting for the GPU")?;
        let surface = &self.gpu.instance.surface;
        let physical = self.gpu.physical;
        // SAFETY: the surface and physical device belong to this instance.
        let (caps, formats) = unsafe {
            (
                surface.get_physical_device_surface_capabilities(physical, self.vk_surface),
                surface.get_physical_device_surface_formats(physical, self.vk_surface),
            )
        };
        let caps = caps.context("the surface's capabilities")?;
        let formats = formats.context("the surface's formats")?;
        if !formats
            .iter()
            .any(|f| f.format == FORMAT && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        {
            return Err(error(format!("the surface does not offer {FORMAT:?}")));
        }
        if !caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
        {
            return Err(error("the surface cannot be opaque"));
        }
        // Wayland leaves the size to the window (u32::MAX).
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size
                    .0
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: size
                    .1
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let old = self.swapchain.take();
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.vk_surface)
            .min_image_count(count)
            .image_format(FORMAT)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(old.as_ref().map_or(vk::SwapchainKHR::null(), |s| s.handle));
        // SAFETY: the info lives until the call returns; the old swapchain
        // is retired by it and destroyed below.
        let created = unsafe { self.gpu.swapchain.create_swapchain(&info, None) };
        if let Some(old) = old {
            self.destroy_swapchain(old);
        }
        let handle = created.context("creating the swapchain")?;
        let mut swapchain = Swapchain {
            handle,
            size: (extent.width, extent.height),
            images: Vec::new(),
            views: Vec::new(),
            drawn: Vec::new(),
        };
        // On failure the half-made swapchain is destroyed like a whole one.
        let filled = (|| -> Result<()> {
            // SAFETY: the swapchain was just made by this device.
            swapchain.images = unsafe { self.gpu.swapchain.get_swapchain_images(handle) }
                .context("the swapchain's images")?;
            for &image in &swapchain.images {
                swapchain.views.push(image::view(d, image, FORMAT, true)?);
                // SAFETY: plain creation; destroyed with the swapchain.
                swapchain.drawn.push(
                    unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                        .context("semaphore")?,
                );
            }
            Ok(())
        })();
        if let Err(e) = filled {
            self.destroy_swapchain(swapchain);
            return Err(e);
        }
        debug!(
            "swapchain {}x{} with {} images",
            extent.width,
            extent.height,
            swapchain.images.len()
        );
        self.swapchain = Some(swapchain);
        self.stale = false;
        self.set_opaque(extent);
        Ok(())
    }

    fn destroy_swapchain(&self, swapchain: Swapchain) {
        let d = &self.gpu.shared.device;
        // SAFETY: the device is idle (callers wait); the images belong to
        // the swapchain and go with it.
        unsafe {
            for view in swapchain.views {
                d.destroy_image_view(view, None);
            }
            for semaphore in swapchain.drawn {
                d.destroy_semaphore(semaphore, None);
            }
            self.gpu.swapchain.destroy_swapchain(swapchain.handle, None);
        }
    }

    /// The window is opaque: the compositor need not draw what is under it.
    fn set_opaque(&self, extent: vk::Extent2D) {
        let qh = self.queue.handle();
        let region: WlRegion = self.state.compositor.create_region(&qh, ());
        region.add(0, 0, extent.width as i32, extent.height as i32);
        self.state.surface.set_opaque_region(Some(&region));
        region.destroy();
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let d = &self.gpu.shared.device;
        // SAFETY: nothing records any more; after the wait the GPU uses
        // nothing of the window.
        if let Err(e) = unsafe { d.device_wait_idle() } {
            warn!("waiting for the GPU: {e}");
        }
        if let Some(swapchain) = self.swapchain.take() {
            self.destroy_swapchain(swapchain);
        }
        let d = &self.gpu.shared.device;
        // SAFETY: as above; the command buffers go with their pool, the
        // Wayland surface outlives the Vulkan one (the connection is
        // dropped after this).
        unsafe {
            for frame in self.in_flight.drain(..) {
                d.destroy_semaphore(frame.acquired, None);
                if let Some(quads) = frame.quads {
                    d.destroy_buffer(quads.buffer, None);
                    d.free_memory(quads.memory, None);
                }
            }
            d.destroy_command_pool(self.pool, None);
            self.gpu
                .instance
                .surface
                .destroy_surface(self.vk_surface, None);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut State,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut State,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            // Acknowledged before the frame for it is presented (the
            // present commits).
            xdg_surface.ack_configure(serial);
            state.events.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut State,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.asked = (width, height);
            }
            xdg_toplevel::Event::Close => state.events.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlRegion);
