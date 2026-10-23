//! A window: an xdg-shell toplevel, or a panel of Beize's shell along an
//! edge of the screen (wlr-layer-shell, ADR 0028 §1), shown through a
//! Vulkan swapchain on the same libwayland display (Mesa's WSI: dmabufs on
//! a GPU, shm on lavapipe; FIFO paces it to the compositor's frames).
//! Opaque, in the size the compositor asks for. The renderer draws into
//! the swapchain's images. Input comes from the seat (`input`).

use std::ffi::c_void;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use ash::vk;
use beize_vulkan::error::{Context, Result, error};
use beize_vulkan::image;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, warn};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use crate::clipboard::Clipboard;
use crate::draw::Scene;
use crate::gpu::Gpu;
use crate::input::{Input, Keyboard, Modifiers, PointerState};
use crate::renderer::{After, Renderer, Target};

/// sRGB: the GPU blends in linear light and stores sRGB (`draw`).
const FORMAT: vk::Format = vk::Format::B8G8R8A8_SRGB;
/// Until the compositor names a size.
const DEFAULT_SIZE: (u32, u32) = (800, 600);

/// What happened while waiting.
#[derive(Debug, Default, PartialEq)]
pub struct Events {
    /// The compositor asked for a size (the first, or a new one): draw.
    pub configured: bool,
    /// The user closed the window.
    pub closed: bool,
    pub input: Vec<Input>,
    /// Which of the app's own fds (`dispatch`'s `also`) are readable or
    /// closed, by their index there.
    pub ready: Vec<usize>,
}

impl Events {
    fn any(&self) -> bool {
        self.configured || self.closed || !self.input.is_empty()
    }
}

pub(crate) struct State {
    compositor: WlCompositor,
    surface: WlSurface,
    /// From the last toplevel configure; 0 means "choose".
    asked: (i32, i32),
    events: Events,
    wl_keyboard: Option<WlKeyboard>,
    wl_pointer: Option<WlPointer>,
    keyboard: Keyboard,
    pointer: PointerState,
    pub(crate) clipboard: Clipboard,
    /// Of the last key press or click: copying names the input it
    /// answers.
    serial: u32,
}

struct Swapchain {
    handle: vk::SwapchainKHR,
    size: (u32, u32),
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    /// Per image: signalled when its frame is drawn, for presenting.
    drawn: Vec<vk::Semaphore>,
}

/// The edge of the screen a panel lies along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Top,
    Bottom,
}

/// What the window is to the compositor.
enum Role {
    /// An app's window.
    Toplevel {
        _wm_base: XdgWmBase,
        _xdg_surface: XdgSurface,
        toplevel: XdgToplevel,
    },
    /// A panel of the shell: the compositor keeps windows out of its strip.
    Panel {
        _layer_shell: ZwlrLayerShellV1,
        _layer: ZwlrLayerSurfaceV1,
    },
}

pub struct Window {
    renderer: Renderer,
    _connection: Connection,
    queue: EventQueue<State>,
    state: State,
    /// None: the compositor has no seat, the window gets no input.
    _seat: Option<WlSeat>,
    role: Role,
    vk_surface: vk::SurfaceKHR,
    swapchain: Option<Swapchain>,
    /// A present said the swapchain no longer fits the surface.
    stale: bool,
}

impl Window {
    /// Opens a window; it shows once the first `configured` is drawn.
    pub fn open(title: &str, app_id: &str) -> Result<Window> {
        Window::new(|globals, qh, surface| {
            let wm_base: XdgWmBase = globals
                .bind(qh, 1..=6, ())
                .context("the compositor has no xdg_wm_base")?;
            let xdg_surface = wm_base.get_xdg_surface(surface, qh, ());
            let toplevel = xdg_surface.get_toplevel(qh, ());
            toplevel.set_title(title.into());
            toplevel.set_app_id(app_id.into());
            Ok(Role::Toplevel {
                _wm_base: wm_base,
                _xdg_surface: xdg_surface,
                toplevel,
            })
        })
    }

    /// Opens a panel of Beize's shell: `height` pixels across the whole
    /// `edge`, above the windows, which the compositor keeps out of that
    /// strip. Only the shell may (the compositor offers the protocol to no
    /// other client). `namespace` names it in the compositor's log.
    pub fn panel(namespace: &str, edge: Edge, height: u32) -> Result<Window> {
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        Window::new(|globals, qh, surface| {
            let layer_shell: ZwlrLayerShellV1 = globals
                .bind(qh, 1..=4, ())
                .context("the compositor offers no panels (wlr-layer-shell) to this program")?;
            let layer = layer_shell.get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Top,
                namespace.into(),
                qh,
                (),
            );
            let along = match edge {
                Edge::Top => Anchor::Top,
                Edge::Bottom => Anchor::Bottom,
            };
            layer.set_anchor(Anchor::Left | Anchor::Right | along);
            // Width 0: as wide as the screen (the anchors left and right).
            layer.set_size(0, height);
            layer.set_exclusive_zone(i32::try_from(height).unwrap_or(i32::MAX));
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            Ok(Role::Panel {
                _layer_shell: layer_shell,
                _layer: layer,
            })
        })
    }

    /// Connects, gives a new surface its `role` and readies the renderer;
    /// the surface's first commit asks the compositor for a configure.
    fn new(
        role: impl FnOnce(&GlobalList, &QueueHandle<State>, &WlSurface) -> Result<Role>,
    ) -> Result<Window> {
        let connection = Connection::connect_to_env().context("connecting to the compositor")?;
        let (globals, queue) =
            registry_queue_init::<State>(&connection).context("the compositor's globals")?;
        let qh = queue.handle();
        let compositor: WlCompositor = globals
            .bind(&qh, 4..=6, ())
            .context("the compositor has no wl_compositor")?;
        // From version 5 on, pointer events come in frames.
        let seat: Option<WlSeat> = globals.bind(&qh, 5..=9, ()).ok();
        if seat.is_none() {
            warn!("the compositor has no seat: no input");
        }
        let mut clipboard = Clipboard::new(globals.bind(&qh, 3..=3, ()).ok());
        if let Some(seat) = &seat {
            clipboard.attach(seat, &qh);
        }
        let surface = compositor.create_surface(&qh, ());
        let role = role(&globals, &qh, &surface)?;
        surface.commit();

        let display = connection.backend().display_ptr().cast::<c_void>();
        let renderer = Renderer::new(Gpu::new(Some(display))?)?;
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(display)
            .surface(surface.id().as_ptr().cast());
        // SAFETY: display and surface are libwayland's and live as long as
        // the connection, which outlives the Vulkan surface (`Drop`).
        let vk_surface = unsafe {
            renderer
                .gpu
                .instance
                .wayland
                .create_wayland_surface(&info, None)
        }
        .context("a Vulkan surface for the window")?;
        Ok(Window {
            renderer,
            _connection: connection,
            queue,
            state: State {
                compositor,
                surface,
                asked: (0, 0),
                events: Events::default(),
                wl_keyboard: None,
                wl_pointer: None,
                keyboard: Keyboard::new(),
                pointer: PointerState::default(),
                clipboard,
                serial: 0,
            },
            _seat: seat,
            role,
            vk_surface,
            swapchain: None,
            stale: false,
        })
    }

    /// The modifiers held now (for the pointer: Shift+click and the like).
    pub fn modifiers(&self) -> Modifiers {
        self.state.keyboard.modifiers()
    }

    /// Puts `text` on the clipboard, for the input just handled
    /// (Ctrl+Shift+C).
    pub fn copy(&mut self, text: String) {
        let qh = self.queue.handle();
        self.state.clipboard.copy(text, self.state.serial, &qh);
    }

    /// Asks for the clipboard's text: it arrives on the returned pipe,
    /// which ends when all is there (wait for it with `dispatch`'s
    /// `also`). None if the clipboard holds no text.
    pub fn paste(&mut self) -> Result<Option<OwnedFd>> {
        let pipe = self
            .state
            .clipboard
            .paste()
            .context("asking for the clipboard")?;
        self.queue.flush().context("sending to the compositor")?;
        Ok(pipe)
    }

    /// The title the shell shows for the window (a panel has none).
    pub fn set_title(&self, title: &str) {
        if let Role::Toplevel { toplevel, .. } = &self.role {
            toplevel.set_title(title.into());
        }
    }

    /// The GPU the window draws on.
    pub fn gpu_name(&self) -> &str {
        &self.renderer.gpu.name
    }

    /// The size the next frame is drawn in.
    pub fn size(&self) -> (u32, u32) {
        match self.state.asked {
            (w, h) if w > 0 && h > 0 => (w as u32, h as u32),
            _ => DEFAULT_SIZE,
        }
    }

    /// Handles what the compositor sent and repeats a held key, waiting
    /// at most `timeout` for either (`None`: until something comes) – or
    /// for one of the app's own fds in `also` to become readable (a
    /// terminal's PTY): one thread waits for all.
    pub fn dispatch(
        &mut self,
        timeout: Option<Duration>,
        also: &[BorrowedFd<'_>],
    ) -> Result<Events> {
        let queue = &mut self.queue;
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        queue.flush().context("sending to the compositor")?;
        // Mesa's WSI reads the same socket and may have queued events
        // already: then only a look, no waiting.
        let mut timeout = if self.state.events.any() {
            Some(Duration::ZERO)
        } else {
            timeout
        };
        if let Some(next) = self.state.keyboard.next_repeat() {
            let until = next.saturating_duration_since(Instant::now());
            timeout = Some(timeout.map_or(until, |t| t.min(until)));
        }
        // Without the guard, events wait in the queue already: the
        // compositor's socket is not read now, the app's fds are.
        let guard = queue.prepare_read();
        let timeout = match guard {
            Some(_) => timeout,
            None => Some(Duration::ZERO),
        };
        let connection = guard.as_ref().map(|g| g.connection_fd());
        let mut fds: Vec<PollFd<'_>> = connection
            .iter()
            .chain(also)
            .map(|fd| PollFd::new(fd, PollFlags::IN))
            .collect();
        let timeout = timeout
            .map(Timespec::try_from)
            .transpose()
            .context("timeout")?;
        poll(&mut fds, timeout.as_ref()).context("poll")?;
        let readable = |fd: &PollFd<'_>| !fd.revents().is_empty();
        let own = usize::from(connection.is_some());
        let ready = fds[own..]
            .iter()
            .enumerate()
            .filter(|(_, fd)| readable(fd))
            .map(|(i, _)| i)
            .collect();
        let incoming = connection.is_some() && readable(&fds[0]);
        drop(fds);
        if let Some(guard) = guard
            && incoming
        {
            guard.read().context("reading from the compositor")?;
        }
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        let state = &mut self.state;
        if let Some(repeat) = state.keyboard.repeat_due(Instant::now()) {
            state.events.input.push(repeat);
        }
        state.events.ready = ready;
        Ok(std::mem::take(&mut state.events))
    }

    /// Draws `scene` and shows it with the compositor's next frame. The
    /// scene covers the whole window: what it leaves is undefined.
    pub fn draw(&mut self, scene: &Scene) -> Result<()> {
        let size = self.size();
        for _ in 0..2 {
            if self.stale || self.swapchain.as_ref().is_none_or(|s| s.size != size) {
                self.recreate(size)?;
            }
            if self.draw_once(scene)? {
                return Ok(());
            }
            // Out of date before drawing: once more with a new one.
            self.stale = true;
        }
        Err(error("the swapchain stays out of date"))
    }

    /// One frame; false if the swapchain was out of date to begin with.
    fn draw_once(&mut self, scene: &Scene) -> Result<bool> {
        let acquired = self.renderer.next()?;
        let swapchain = self.swapchain.as_ref().context("no swapchain")?;
        let gpu = &self.renderer.gpu;
        // SAFETY: the swapchain is this device's; the semaphore is
        // unsignalled (its frame's last submission waited on it and is done).
        let acquire = unsafe {
            gpu.swapchain.acquire_next_image(
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
        let target = Target {
            image: swapchain.images[index],
            view: swapchain.views[index],
            format: FORMAT,
            size: swapchain.size,
            after: After::Present,
        };
        let (handle, drawn) = (swapchain.handle, swapchain.drawn[index]);
        self.renderer
            .draw(scene, &target, Some(acquired), Some(drawn))?;

        let swapchains = [handle];
        let indices = [index as u32];
        let wait = [drawn];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait)
            .swapchains(&swapchains)
            .image_indices(&indices);
        let gpu = &self.renderer.gpu;
        // SAFETY: the image was acquired above and is drawn when `drawn`
        // is signalled.
        match unsafe { gpu.swapchain.queue_present(gpu.queue, &present) } {
            Ok(false) => {}
            // Suboptimal: shown, but the next frame gets a new swapchain.
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale = true,
            Err(e) => return Err(error(format!("presenting: {e}"))),
        }
        Ok(true)
    }

    /// A swapchain of `size` (the surface's own size where it has one),
    /// replacing the old one.
    fn recreate(&mut self, size: (u32, u32)) -> Result<()> {
        // A new swapchain is rare (configure): the old one goes once nothing
        // uses it any more, presents included.
        let gpu = &self.renderer.gpu;
        let d = &gpu.shared.device;
        // SAFETY: nothing records at the moment.
        unsafe { d.device_wait_idle() }.context("waiting for the GPU")?;
        let surface = &gpu.instance.surface;
        // SAFETY: the surface and physical device belong to this instance.
        let (caps, formats) = unsafe {
            (
                surface.get_physical_device_surface_capabilities(gpu.physical, self.vk_surface),
                surface.get_physical_device_surface_formats(gpu.physical, self.vk_surface),
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
        let created = unsafe { gpu.swapchain.create_swapchain(&info, None) };
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
            swapchain.images = unsafe { gpu.swapchain.get_swapchain_images(handle) }
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
        let gpu = &self.renderer.gpu;
        let d = &gpu.shared.device;
        // SAFETY: the device is idle (callers wait); the images belong to
        // the swapchain and go with it.
        unsafe {
            for view in swapchain.views {
                d.destroy_image_view(view, None);
            }
            for semaphore in swapchain.drawn {
                d.destroy_semaphore(semaphore, None);
            }
            gpu.swapchain.destroy_swapchain(swapchain.handle, None);
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
        let gpu = &self.renderer.gpu;
        // SAFETY: nothing records any more; after the wait the GPU uses
        // nothing of the window.
        if let Err(e) = unsafe { gpu.shared.device.device_wait_idle() } {
            warn!("waiting for the GPU: {e}");
        }
        if let Some(swapchain) = self.swapchain.take() {
            self.destroy_swapchain(swapchain);
        }
        // SAFETY: as above; the Wayland surface outlives the Vulkan one (the
        // connection is dropped after this), the renderer after it.
        unsafe {
            self.renderer
                .gpu
                .instance
                .surface
                .destroy_surface(self.vk_surface, None)
        };
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

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut State,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                // As for a toplevel (`XdgSurface`): acknowledged before the
                // frame for it is presented.
                layer.ack_configure(serial);
                let size = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
                state.asked = (size(width), size(height));
                state.events.configured = true;
            }
            // The compositor took the panel away (its screen went).
            zwlr_layer_surface_v1::Event::Closed => state.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut State,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        let keyboard = caps.contains(wl_seat::Capability::Keyboard);
        match (state.wl_keyboard.take(), keyboard) {
            (None, true) => state.wl_keyboard = Some(seat.get_keyboard(qh, ())),
            (Some(gone), false) => {
                gone.release();
                // As if the window lost it: no key stays held.
                let leave = wl_keyboard::Event::Leave {
                    serial: 0,
                    surface: state.surface.clone(),
                };
                state.keyboard.event(leave, &mut state.events.input);
            }
            (kept, _) => state.wl_keyboard = kept,
        }
        let pointer = caps.contains(wl_seat::Capability::Pointer);
        match (state.wl_pointer.take(), pointer) {
            (None, true) => state.wl_pointer = Some(seat.get_pointer(qh, ())),
            (Some(gone), false) => gone.release(),
            (kept, _) => state.wl_pointer = kept,
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_keyboard::Event::Key { serial, .. } | wl_keyboard::Event::Enter { serial, .. } =
            &event
        {
            state.serial = *serial;
        }
        state.keyboard.event(event, &mut state.events.input);
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut State,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = &event {
            state.serial = *serial;
        }
        state.pointer.event(event, &mut state.events.input);
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore ZwlrLayerShellV1);
