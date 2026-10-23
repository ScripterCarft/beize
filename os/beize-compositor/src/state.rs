//! The compositor's state and the Wayland protocols it serves: surfaces
//! (wl_compositor, wl_shm, linux-dmabuf), the seat, the screen as
//! wl_output, copy and paste (wl_data_device), windows (xdg-shell,
//! `shell`) and the shell's panels (wlr-layer-shell, `layers`).
//!
//! Drawing follows the screen, not the clients: any change marks the
//! screen for a redraw at the deadline of the next vblank (`schedule`),
//! which draws all changes until then at once, and clients hear "frame
//! done" when their frame is on the screen (its vblank) – a client draws
//! at most as often as the screen shows. With nothing to draw, they hear
//! it one refresh later.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::{DrmEvent, DrmNode};
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::{PopupManager, Space, Window, layer_map_for_output};
use smithay::input::keyboard::{KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutputMode, Output as WlOutput, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, LoopSignal, RegistrationToken};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{
    Clock, Logical, Monotonic, Point, Rectangle, SERIAL_COUNTER, Scale, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::shell::wlr_layer::{Layer, WlrLayerShellState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    delegate_shm, delegate_xdg_shell,
};
use tracing::{error, info, warn};

use crate::cursor::Cursor;
use crate::gpu::{self, Screen};
use crate::keyboard::Layout;
use crate::output::{Frame, Info};
use beize_compositor::error::{Context, Error, Result, error};
use beize_compositor::render::VkRenderer;

/// In a module of its own: the macro's code names `Result`, which here is
/// the compositor's. Generic over the renderer, as Smithay's concrete form
/// cannot express a frame with two lifetimes (`VkFrame`).
mod scene {
    use smithay::backend::renderer::ImportAll;
    use smithay::backend::renderer::ImportMem;
    use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
    use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
    use smithay::render_elements;

    render_elements! {
        /// What a frame draws, front to back: the pointer where no cursor
        /// plane shows it, then the surfaces – panels above the windows,
        /// windows, panels below them.
        pub Element<R> where R: ImportAll + ImportMem;
        Cursor=MemoryRenderBufferRenderElement<R>,
        Surface=WaylandSurfaceRenderElement<R>,
    }
}
type Element = scene::Element<VkRenderer>;

/// Key repeat: GNOME's defaults (500 ms, then every 30 ms) until Beize's
/// settings offer a choice.
const REPEAT_DELAY: i32 = 500;
const REPEAT_RATE: i32 = 33;

/// Surfaces not on the screen hear "frame done" at most this often.
const HIDDEN_FRAMES: Duration = Duration::from_secs(1);
/// How often a run of frames is checked for its end (`Pacing`).
const PACING_CHECK: Duration = Duration::from_millis(500);

pub struct State {
    pub display: DisplayHandle,
    pub handle: LoopHandle<'static, State>,
    signal: LoopSignal,
    pub session: LibSeatSession,
    pub libinput: Libinput,

    compositor: CompositorState,
    shm: ShmState,
    dmabuf: DmabufState,
    pub xdg_shell: XdgShellState,
    pub layer_shell: WlrLayerShellState,
    _outputs: OutputManagerState,
    seat_state: SeatState<State>,
    data_device: DataDeviceState,

    pub seat: Seat<State>,
    pub keyboard: KeyboardHandle<State>,
    /// Keys pressed for the compositor (`input`): their release stays with
    /// it too, the window never hears of them.
    pub taken_keys: HashSet<Keycode>,
    pub space: Space<Window>,
    pub popups: PopupManager,
    /// The screen: None until the session is first active, as only then
    /// does logind hand out the GPU (a session started on a background VT
    /// waits).
    screen: Option<Screen>,
    pub output: Option<WlOutput>,
    /// Where the pointer is, on the screen.
    pub pointer: Point<f64, Logical>,
    cursor: Cursor,
    clock: Clock<Monotonic>,
    /// Something on the screen may have changed.
    redraw: bool,
    /// When the first change since the last frame arrived (`Pacing`).
    changed_at: Option<Duration>,
    /// Draws at the deadline of the next vblank.
    draw_timer: Option<RegistrationToken>,
    /// Ends a measured run of frames once they stop (`Pacing`).
    pacing_timer: Option<RegistrationToken>,
    /// "Frame done" for clients when nothing was drawn.
    frame_timer: Option<RegistrationToken>,
    failure: Option<Error>,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor: CompositorClientState,
    /// The client is Beize's shell (`trust`): it may draw panels.
    pub shell: bool,
}

impl ClientData for ClientState {
    fn initialized(&self, _client: ClientId) {}
    fn disconnected(&self, _client: ClientId, _reason: DisconnectReason) {}
}

impl State {
    pub fn new(
        display: DisplayHandle,
        handle: LoopHandle<'static, State>,
        signal: LoopSignal,
        session: LibSeatSession,
        libinput: Libinput,
    ) -> Result<State> {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display, session.seat());
        let layout = Layout::current();
        let keyboard = match seat.add_keyboard(layout.xkb(), REPEAT_DELAY, REPEAT_RATE) {
            Ok(keyboard) => {
                info!("keyboard layout {layout}");
                keyboard
            }
            Err(e) => {
                warn!("keyboard layout {layout}: {e}; using XKB's default");
                seat.add_keyboard(XkbConfig::default(), REPEAT_DELAY, REPEAT_RATE)
                    .context("keyboard")?
            }
        };
        seat.add_pointer();
        Ok(State {
            compositor: CompositorState::new::<State>(&display),
            shm: ShmState::new::<State>(&display, Vec::new()),
            dmabuf: DmabufState::new(),
            xdg_shell: XdgShellState::new::<State>(&display),
            // Other clients do not even see the global.
            layer_shell: WlrLayerShellState::new_with_filter::<State, _>(&display, |client| {
                client.get_data::<ClientState>().is_some_and(|c| c.shell)
            }),
            _outputs: OutputManagerState::new_with_xdg_output::<State>(&display),
            data_device: DataDeviceState::new::<State>(&display),
            seat_state,
            seat,
            keyboard,
            taken_keys: HashSet::new(),
            space: Space::default(),
            popups: PopupManager::default(),
            screen: None,
            output: None,
            pointer: Point::default(),
            cursor: Cursor::new(),
            clock: Clock::new(),
            redraw: false,
            changed_at: None,
            draw_timer: None,
            pacing_timer: None,
            frame_timer: None,
            failure: None,
            display,
            handle,
            signal,
            session,
            libinput,
        })
    }

    /// Ends the compositor with `e`.
    pub fn fail(&mut self, e: Error) {
        self.failure = Some(e);
        self.signal.stop();
    }

    pub fn failure(&mut self) -> Option<Error> {
        self.failure.take()
    }

    /// The session became active: the screen, taken the first time.
    pub fn activate(&mut self) -> Result<()> {
        if self.libinput.resume().is_err() {
            warn!("libinput could not take the input devices");
        }
        match &mut self.screen {
            Some(screen) => screen.resume()?,
            None => self.start()?,
        }
        self.redraw = true;
        self.schedule_render();
        Ok(())
    }

    /// Another session took the screen and the input devices.
    pub fn pause(&mut self) {
        self.libinput.suspend();
        if let Some(screen) = &mut self.screen {
            screen.pause();
        }
    }

    /// Takes the GPU and the screen, and offers the screen to clients.
    fn start(&mut self) -> Result<()> {
        let (screen, info, events, node) = gpu::open(&mut self.session)?;
        self.handle
            .insert_source(events, |event, metadata, state| match event {
                DrmEvent::VBlank(_) => {
                    let time = match &mut state.screen {
                        Some(screen) => screen.vblank(metadata.take()),
                        None => None,
                    };
                    state.frame_done(time);
                    if state.redraw {
                        state.schedule_render();
                    }
                    state.watch_pacing();
                }
                DrmEvent::Error(e) => error!("DRM: {e}"),
            })
            .map_err(|e| error(format!("DRM events: {}", e.error)))?;
        self.screen = Some(screen);
        self.offer_output(&info);
        self.offer_dmabuf(node);
        Ok(())
    }

    fn offer_output(&mut self, info: &Info) {
        let output = WlOutput::new(
            info.name.clone(),
            PhysicalProperties {
                size: info.size_mm.into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: info.name.clone(),
            },
        );
        let (w, h) = info.mode.size();
        let mode = OutputMode {
            size: (i32::from(w), i32::from(h)).into(),
            refresh: i32::try_from(gpu::millihertz(&info.mode)).unwrap_or(i32::MAX),
        };
        output.create_global::<State>(&self.display);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        self.space.map_output(&output, (0, 0));
        self.output = Some(output);
    }

    /// linux-dmabuf with the formats the renderer imports, the GPU as the
    /// main device (feedback, version 4).
    fn offer_dmabuf(&mut self, node: DrmNode) {
        let Some(screen) = &mut self.screen else {
            return;
        };
        let formats = screen.renderer().dmabuf_formats();
        match DmabufFeedbackBuilder::new(node.dev_id(), formats).build() {
            Ok(feedback) => {
                self.dmabuf
                    .create_global_with_default_feedback::<State>(&self.display, &feedback);
            }
            Err(e) => warn!("no linux-dmabuf for clients: {e}"),
        }
    }

    /// The screen's area.
    pub fn screen_area(&self) -> Rectangle<i32, Logical> {
        self.output
            .as_ref()
            .and_then(|o| self.space.output_geometry(o))
            .unwrap_or_default()
    }

    /// What the panels leave of the screen for windows.
    pub fn window_area(&self) -> Rectangle<i32, Logical> {
        let screen = self.screen_area();
        let Some(output) = &self.output else {
            return screen;
        };
        let mut area = layer_map_for_output(output).non_exclusive_zone();
        area.loc += screen.loc;
        area
    }

    /// Fits every window to the window area, which the panels changed.
    /// Until the shell arranges windows (ADR 0028 §2), each fills it.
    pub fn arrange_windows(&mut self) {
        let area = self.window_area();
        // Mapping again puts a window on top: bottom to top, the order stays,
        // and so does which is active.
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| state.size = Some(area.size));
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            self.space.map_element(window, area.loc, false);
        }
        self.changed();
    }

    /// Something on the screen changed; it is drawn at the deadline of the
    /// next vblank.
    pub fn changed(&mut self) {
        self.redraw = true;
        if self.changed_at.is_none() {
            self.changed_at = Some(self.clock.now().into());
        }
        self.schedule_render();
    }

    /// Sets the timer that draws, unless one is set or a commit waits for
    /// its vblank (whose event calls again). Not while another session
    /// has the screen: `activate` calls again.
    fn schedule_render(&mut self) {
        if self.draw_timer.is_some() || !self.session.is_active() {
            return;
        }
        let Some(at) = self.screen.as_mut().and_then(Screen::draw_at) else {
            return;
        };
        let now: Duration = self.clock.now().into();
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                state.draw_timer = None;
                if state.redraw {
                    state.render();
                }
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.draw_timer = Some(token),
            Err(e) => self.fail(error(format!("draw timer: {}", e.error))),
        }
    }

    /// Draws what changed and places the cursor.
    fn render(&mut self) {
        if !self.session.is_active() {
            return;
        }
        let (Some(screen), Some(output)) = (&mut self.screen, &self.output) else {
            return;
        };
        self.redraw = false;
        let began: Duration = self.clock.now().into();
        let requested = self.changed_at.unwrap_or(began);
        let Some(area) = self.space.output_geometry(output) else {
            return;
        };
        let at = self.pointer.to_physical(1.0).to_i32_round();
        let on_plane = self.cursor.image().is_some_and(|i| screen.cursor_fits(i));
        let mut elements: Vec<Element> = Vec::new();
        if !on_plane && let Some((buffer, hotspot)) = self.cursor.drawn() {
            let location = (at - Point::from(hotspot)).to_f64();
            match MemoryRenderBufferRenderElement::from_buffer(
                screen.renderer(),
                location,
                &buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => elements.push(Element::Cursor(element)),
                Err(e) => warn!("drawing the pointer: {e}"),
            }
        }
        let (upper, lower) = panels(screen.renderer(), output);
        elements.extend(upper.into_iter().map(Element::Surface));
        let windows: Vec<WaylandSurfaceRenderElement<_>> =
            self.space
                .render_elements_for_region(screen.renderer(), &area, 1.0, 1.0);
        elements.extend(windows.into_iter().map(Element::Surface));
        elements.extend(lower.into_iter().map(Element::Surface));
        let cursor = self
            .cursor
            .image()
            .filter(|_| on_plane)
            .map(|image| (image, at));
        let frame = screen.render(&elements, cursor, requested, began);
        if !matches!(frame, Ok(Frame::Deferred)) {
            self.changed_at = None;
        }
        match frame {
            Ok(Frame::Queued) => {}
            Ok(Frame::Deferred) => self.redraw = true,
            Ok(Frame::Unchanged) => self.frame_done_later(),
            Err(e) => self.fail(e),
        }
    }

    /// While frames come in a row, checks twice a second whether they
    /// stopped, so their summary reaches the journal; idle, no timer runs.
    fn watch_pacing(&mut self) {
        if self.pacing_timer.is_some() {
            return;
        }
        let token = self
            .handle
            .insert_source(Timer::from_duration(PACING_CHECK), |_, _, state| {
                let running = state.screen.as_mut().is_some_and(Screen::pacing_idle);
                if running {
                    TimeoutAction::ToDuration(PACING_CHECK)
                } else {
                    state.pacing_timer = None;
                    TimeoutAction::Drop
                }
            });
        match token {
            Ok(token) => self.pacing_timer = Some(token),
            Err(e) => warn!("pacing timer: {}", e.error),
        }
    }

    /// "Frame done" one refresh from now, for clients that committed
    /// without changing what the screen shows.
    fn frame_done_later(&mut self) {
        if self.frame_timer.is_some() {
            return;
        }
        let refresh = self
            .screen
            .as_ref()
            .map_or(Duration::from_millis(16), Screen::refresh);
        let token = self
            .handle
            .insert_source(Timer::from_duration(refresh), |_, _, state| {
                state.frame_timer = None;
                state.frame_done(None);
                TimeoutAction::Drop
            });
        match token {
            Ok(token) => self.frame_timer = Some(token),
            Err(e) => warn!("frame timer: {}", e.error),
        }
    }

    /// Tells the clients on the screen their frame is shown, at `time`
    /// (the vblank) or now.
    fn frame_done(&mut self, time: Option<Duration>) {
        let Some(output) = self.output.clone() else {
            return;
        };
        let time = time.unwrap_or_else(|| self.clock.now().into());
        for window in self.space.elements() {
            window.send_frame(&output, time, Some(HIDDEN_FRAMES), |_, _| {
                Some(output.clone())
            });
        }
        for panel in layer_map_for_output(&output).layers() {
            panel.send_frame(&output, time, Some(HIDDEN_FRAMES), |_, _| {
                Some(output.clone())
            });
        }
        if let Some(surface) = self.cursor.surface() {
            send_frames_surface_tree(surface, &output, time, Some(HIDDEN_FRAMES), |_, _| {
                Some(output.clone())
            });
        }
        self.space.refresh();
        self.popups.cleanup();
    }

    /// The topmost surface under `at` – a panel above the windows, a
    /// window, a panel below them – and that surface's position.
    pub fn surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let panels = |layers: &[Layer]| {
            self.output
                .as_ref()
                .and_then(|output| crate::layers::surface_under(output, layers, at))
        };
        panels(&[Layer::Overlay, Layer::Top])
            .or_else(|| self.window_surface_under(at))
            .or_else(|| panels(&[Layer::Bottom, Layer::Background]))
    }

    /// The topmost window under `at`, its surface there and that surface's
    /// position.
    fn window_surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.space.element_under(at)?;
        window
            .surface_under(
                at - location.to_f64(),
                smithay::desktop::WindowSurfaceType::ALL,
            )
            .map(|(surface, offset)| (surface, (offset + location).to_f64()))
    }

    /// Raises `window`, makes it the active one and gives it the keyboard.
    /// A window still waiting for its first configure gets the state with
    /// it (`shell::committed`): the protocol sends none before the
    /// client's initial commit.
    pub fn focus(&mut self, window: Option<Window>) {
        let configure = |toplevel: &smithay::wayland::shell::xdg::ToplevelSurface| {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        };
        for other in self.space.elements() {
            if Some(other) != window.as_ref()
                && other.set_activated(false)
                && let Some(toplevel) = other.toplevel()
            {
                configure(toplevel);
            }
        }
        let surface = window.as_ref().and_then(|window| {
            self.space.raise_element(window, true);
            let toplevel = window.toplevel()?;
            if window.set_activated(true) {
                configure(toplevel);
            }
            Some(toplevel.wl_surface().clone())
        });
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        self.changed();
    }

    /// localed changed the layout.
    pub fn reload_layout(&mut self) {
        let layout = Layout::current();
        let keyboard = self.keyboard.clone();
        match keyboard.set_xkb_config(self, layout.xkb()) {
            Ok(()) => info!("keyboard layout {layout}"),
            Err(e) => warn!("keyboard layout {layout}: {e:?}; keeping the previous one"),
        }
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is inserted with ClientState (`insert_client`); the
        // fallback only keeps a bug from being a crash.
        static NONE: LazyLock<CompositorClientState> = LazyLock::new(Default::default);
        client
            .get_data::<ClientState>()
            .map_or(&*NONE, |state| &state.compositor)
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<State>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            {
                window.on_commit();
            }
        }
        self.popups.commit(surface);
        crate::shell::committed(self, surface);
        crate::layers::committed(self, surface);
        self.cursor.committed(surface);
        self.changed();
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let imported = self
            .screen
            .as_mut()
            .is_some_and(|screen| screen.renderer().import_dmabuf(&dmabuf, None).is_ok());
        if imported {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<State>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display.get_client(surface.id()).ok());
        set_data_device_focus(&self.display, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<State>, image: CursorImageStatus) {
        self.cursor.set(image);
        self.changed();
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_xdg_shell!(State);

/// The panels' surfaces to draw: those above the windows and those below,
/// each front to back.
fn panels(
    renderer: &mut VkRenderer,
    output: &WlOutput,
) -> (
    Vec<WaylandSurfaceRenderElement<VkRenderer>>,
    Vec<WaylandSurfaceRenderElement<VkRenderer>>,
) {
    let map = layer_map_for_output(output);
    let (mut upper, mut lower) = (Vec::new(), Vec::new());
    // The map lists them back to front.
    for panel in map.layers().rev() {
        let Some(place) = map.layer_geometry(panel) else {
            continue;
        };
        let elements = AsRenderElements::<VkRenderer>::render_elements(
            panel,
            renderer,
            place.loc.to_physical(1),
            Scale::from(1.0),
            1.0,
        );
        match panel.layer() {
            Layer::Overlay | Layer::Top => upper.extend(elements),
            Layer::Bottom | Layer::Background => lower.extend(elements),
        }
    }
    (upper, lower)
}

/// Inserts a client that connected to the socket, knowing whether it is
/// the shell (`trust`).
pub fn insert_client(state: &mut State, stream: std::os::unix::net::UnixStream) {
    let shell = crate::trust::is_shell(&stream).unwrap_or_else(|e| {
        warn!("whether a client is the shell: {e}");
        false
    });
    if shell {
        info!("the shell connected");
    }
    let data = ClientState {
        shell,
        ..ClientState::default()
    };
    if let Err(e) = state.display.insert_client(stream, Arc::new(data)) {
        warn!("a client could not connect: {e}");
    }
}
