//! The compositor's state and the Wayland protocols it serves: surfaces
//! (wl_compositor, wl_shm, linux-dmabuf), the seat, the screen as
//! wl_output, copy and paste (wl_data_device), windows (xdg-shell,
//! `shell`) with their frames (xdg-decoration, `decorations`), the
//! shell's panels (wlr-layer-shell, `layers`) and its list of windows
//! (beize-shell-v1, `shell_v1`).
//!
//! Drawing follows the screen, not the clients: any change marks the
//! screen for a redraw at the deadline of the next vblank (`schedule`),
//! which draws all changes until then at once, and clients hear "frame
//! done" when their frame is on the screen (its vblank) – a client draws
//! at most as often as the screen shows. With nothing to draw, they hear
//! it one refresh later.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::{DrmEvent, DrmNode};
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{AsRenderElements, Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::{PopupManager, Space, Window, layer_map_for_output};
use smithay::input::keyboard::{KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutputMode, Output as WlOutput, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::channel::{self, Sender};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, LoopSignal, RegistrationToken};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{
    Clock, Logical, Monotonic, Physical, Point, Rectangle, SERIAL_COUNTER, Scale, Size, Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
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
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_output, delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
};
use tracing::{error, info, warn};

use crate::backdrop::{Appearance, Backdrop, LiveGlass};
use crate::corners::{Rounded, Shadow};
use crate::cursor::Cursor;
use crate::decorations::{Decorations, Hit};
use crate::gpu::{self, Screen};
use crate::keyboard::Layout;
use crate::layers::{Side, side};
use crate::opening::{Opening, SHELL_WAIT, Veil};
use crate::output::{Frame, Info};
use beize_compositor::error::{Context, Error, Result, error};
use beize_compositor::render::VkRenderer;

/// In a module of its own: the macro's code names `Result`, which here is
/// the compositor's. Generic over the renderer, as Smithay's concrete form
/// cannot express a frame with two lifetimes (`VkFrame`).
mod scene {
    use smithay::backend::renderer::ImportAll;
    use smithay::backend::renderer::ImportMem;
    use smithay::backend::renderer::RendererSuper;
    use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
    use smithay::backend::renderer::element::texture::TextureRenderElement;
    use smithay::render_elements;

    use crate::backdrop::Glass;
    use crate::corners::{Effects, Rounded, Shadow};

    render_elements! {
        /// What a frame draws, front to back: the black the desktop fades
        /// in from (`opening`), the pointer where no cursor plane shows
        /// it, then the surfaces – panels above the windows,
        /// each with its glass, windows each with its popups, title bar,
        /// round corners and shadow, panels below them – and the background
        /// behind all.
        pub Element<R> where R: ImportAll + ImportMem + Effects;
        Veil=SolidColorRenderElement,
        Cursor=MemoryRenderBufferRenderElement<R>,
        Surface=WaylandSurfaceRenderElement<R>,
        Window=Rounded<WaylandSurfaceRenderElement<R>>,
        Bar=Rounded<TextureRenderElement<<R as RendererSuper>::TextureId>>,
        Shadow=Shadow,
        Glass=Glass,
        Wallpaper=TextureRenderElement<<R as RendererSuper>::TextureId>,
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
    _xdg_decoration: XdgDecorationState,
    pub layer_shell: WlrLayerShellState,
    _outputs: OutputManagerState,
    seat_state: SeatState<State>,
    data_device: DataDeviceState,
    _cursor_shape: CursorShapeManagerState,

    pub seat: Seat<State>,
    pub keyboard: KeyboardHandle<State>,
    /// Keys pressed for the compositor (`input`): their release stays with
    /// it too, the window never hears of them.
    pub taken_keys: HashSet<Keycode>,
    pub space: Space<Window>,
    /// New windows until their first content places them (`windows`).
    pub unmapped: Vec<Window>,
    /// Minimized windows, off the screen until the shell brings them back
    /// (`windows`).
    pub minimized: Vec<Window>,
    /// The shell's view of the windows (`shell_v1`).
    pub shell_windows: crate::shell_v1::ShellWindows,
    pub popups: PopupManager,
    /// What the title bars of other apps' windows share (`decorations`).
    decorations: Decorations,
    /// Light or dark (`backdrop`).
    pub appearance: Appearance,
    /// The background and the glass, drawn for the screen's size and the
    /// appearance (`backdrop_for`, numbered by `backdrops`).
    backdrop: Option<Backdrop>,
    backdrop_for: Option<(Size<i32, Physical>, Appearance)>,
    backdrops: u64,
    /// What lies behind live glass, per panel that has it.
    live_glass: Vec<LiveGlass>,
    /// The desktop's start (`opening`): when the screen was taken, the
    /// wait for the shell, the black it fades in from.
    opening: Opening,
    screen_at: Option<Duration>,
    wait_timer: Option<RegistrationToken>,
    veil_id: Id,
    veil_commit: CommitCounter,
    veil_alpha: f32,
    /// The panels as the last frame drew them, and those of a shell that
    /// failed, kept until a new one is ready (`keep_panels`).
    panels_last: Vec<PanelFrame>,
    kept: Option<Kept>,
    /// The shell's connections tell their end here (`shell_ended`).
    shell_ended: Sender<bool>,
    /// The screen: None until the session is first active, as only then
    /// does logind hand out the GPU (a session started on a background VT
    /// waits).
    screen: Option<Screen>,
    pub output: Option<WlOutput>,
    /// Where the pointer is, on the screen.
    pub pointer: Point<f64, Logical>,
    /// The panels shown over a fullscreen window in front, and whether one
    /// was in front when last looked (`layers`, `reveal`).
    pub revealed: Option<Side>,
    pub fullscreen_front: bool,
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
    /// Draws an animated pointer's next frame.
    cursor_timer: Option<RegistrationToken>,
    failure: Option<Error>,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor: CompositorClientState,
    /// The client is Beize's shell (`trust`): it may draw panels.
    pub shell: bool,
    /// The shell said its panels are drawn (`beize_shell_v1.ready`).
    pub ready: AtomicBool,
    /// Where the shell's end is told, with whether it was ready
    /// (`State::shell_ended`).
    ended: Option<Sender<bool>>,
}

impl ClientData for ClientState {
    fn initialized(&self, _client: ClientId) {}
    fn disconnected(&self, _client: ClientId, _reason: DisconnectReason) {
        if let Some(ended) = &self.ended {
            // The event loop is gone only with the compositor.
            let _ = ended.send(self.ready.load(Ordering::Relaxed));
        }
    }
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
        let (shell_ended, ends) = channel::channel();
        handle
            .insert_source(ends, |event, _, state| {
                if let channel::Event::Msg(ready) = event {
                    state.shell_ended(ready);
                }
            })
            .map_err(|e| error(format!("the shell's end: {}", e.error)))?;
        Ok(State {
            compositor: CompositorState::new::<State>(&display),
            shm: ShmState::new::<State>(&display, Vec::new()),
            dmabuf: DmabufState::new(),
            xdg_shell: XdgShellState::new::<State>(&display),
            _xdg_decoration: XdgDecorationState::new::<State>(&display),
            // Other clients do not even see the global.
            layer_shell: WlrLayerShellState::new_with_filter::<State, _>(&display, |client| {
                client.get_data::<ClientState>().is_some_and(|c| c.shell)
            }),
            _outputs: OutputManagerState::new_with_xdg_output::<State>(&display),
            data_device: DataDeviceState::new::<State>(&display),
            // Apps name their pointer's image (`cursor`).
            _cursor_shape: CursorShapeManagerState::new::<State>(&display),
            seat_state,
            seat,
            keyboard,
            taken_keys: HashSet::new(),
            space: Space::default(),
            unmapped: Vec::new(),
            minimized: Vec::new(),
            shell_windows: crate::shell_v1::ShellWindows::new(&display),
            popups: PopupManager::default(),
            decorations: Decorations::default(),
            appearance: Appearance::from_environment(),
            backdrop: None,
            backdrop_for: None,
            backdrops: 0,
            live_glass: Vec::new(),
            opening: Opening::Waiting { shell: false },
            screen_at: None,
            wait_timer: None,
            veil_id: Id::new(),
            veil_commit: CommitCounter::default(),
            veil_alpha: 1.0,
            panels_last: Vec::new(),
            kept: None,
            shell_ended,
            screen: None,
            output: None,
            pointer: Point::default(),
            revealed: None,
            fullscreen_front: false,
            cursor: Cursor::new(),
            clock: Clock::new(),
            redraw: false,
            changed_at: None,
            draw_timer: None,
            pacing_timer: None,
            frame_timer: None,
            cursor_timer: None,
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
        // The desktop shows once the shell is ready, at most SHELL_WAIT
        // from now (`opening`). Black at once sets the mode: the monitor
        // locks on to it while the shell starts and the next frames draw
        // the background behind the black.
        let now: Duration = self.clock.now().into();
        self.screen_at = Some(now);
        if self.opening == (Opening::Waiting { shell: true }) {
            self.show_desktop(now, true);
        } else {
            let token = self
                .handle
                .insert_source(Timer::from_duration(SHELL_WAIT), |_, _, state| {
                    state.wait_timer = None;
                    if matches!(state.opening, Opening::Waiting { .. }) {
                        warn!(
                            "desktop shown without the shell: none was ready within {SHELL_WAIT:?}"
                        );
                        let now = state.clock.now().into();
                        state.show_desktop(now, false);
                    }
                    TimeoutAction::Drop
                })
                .map_err(|e| error(format!("waiting for the shell: {}", e.error)))?;
            self.wait_timer = Some(token);
        }
        let black = veil_over(&self.veil_id, self.veil_commit, self.screen_area(), 1.0);
        if let Some(screen) = &mut self.screen {
            screen.render(&[black], None, now, now)?;
        }
        Ok(())
    }

    /// The shell's panels are drawn (`beize_shell_v1.ready`): the desktop
    /// shows, or a new shell's panels replace those kept of the old.
    pub fn shell_ready(&mut self) {
        let now: Duration = self.clock.now().into();
        if let Some(kept) = self.kept.take() {
            self.handle.remove(kept.timer);
            info!(
                "a new shell is ready {} ms after the old one failed: its panels replace the kept ones",
                now.saturating_sub(kept.since).as_millis()
            );
            self.arrange_windows();
            self.changed();
        }
        if matches!(self.opening, Opening::Waiting { .. }) {
            if self.screen_at.is_some() {
                self.show_desktop(now, true);
            } else {
                self.opening = Opening::Waiting { shell: true };
            }
        }
    }

    /// The desktop fades in from black, with the shell's panels or – it
    /// failed or was not ready in time – without them.
    fn show_desktop(&mut self, now: Duration, with_shell: bool) {
        if let Some(timer) = self.wait_timer.take() {
            self.handle.remove(timer);
        }
        if with_shell {
            let after = now.saturating_sub(self.screen_at.unwrap_or(now));
            info!(
                "desktop shown with the shell, {} ms after the screen",
                after.as_millis()
            );
        }
        self.opening = Opening::Fading { since: now };
        self.changed();
    }

    /// The shell's connection ended, after it was `ready` or before. A
    /// shell that ends before it is ready failed to start: the desktop
    /// shows without it, and the panels kept of the shell before go – a
    /// new one is not coming to replace them. Waiting out SHELL_WAIT is
    /// left to a shell that hangs.
    fn shell_ended(&mut self, ready: bool) {
        if ready {
            return;
        }
        if let Some(kept) = self.kept.take() {
            self.handle.remove(kept.timer);
            warn!("a new shell ended before it was ready: the kept panels go");
            self.arrange_windows();
            self.changed();
        }
        if matches!(self.opening, Opening::Waiting { .. }) && self.screen_at.is_some() {
            warn!("desktop shown without the shell: it ended before it was ready");
            let now = self.clock.now().into();
            self.show_desktop(now, false);
        }
    }

    /// The shell failed (its connection is gone): its panels stay as the
    /// last frame drew them, with the room they took from the windows,
    /// until a new shell is ready – at most SHELL_WAIT –, as macOS keeps
    /// the desktop while its Dock starts again. Kept, they no longer
    /// change: their glass stays as it was.
    pub fn keep_panels(&mut self) {
        if self.kept.is_some() || self.panels_last.is_empty() {
            return;
        }
        let area = self.window_area();
        let mut panels = std::mem::take(&mut self.panels_last);
        for element in panels.iter_mut().flat_map(|p| p.elements.iter_mut()) {
            if let Element::Glass(glass) = element {
                glass.freeze();
            }
        }
        let timer = self
            .handle
            .insert_source(Timer::from_duration(SHELL_WAIT), |_, _, state| {
                if state.kept.take().is_some() {
                    warn!("no new shell was ready within {SHELL_WAIT:?}: its panels go");
                    state.arrange_windows();
                    state.changed();
                }
                TimeoutAction::Drop
            });
        let timer = match timer {
            Ok(timer) => timer,
            Err(e) => {
                warn!("kept panels: {}", e.error);
                return;
            }
        };
        info!(
            "the shell failed: its {} panels stay until a new one is ready",
            panels.len()
        );
        self.kept = Some(Kept {
            panels,
            area,
            since: self.clock.now().into(),
            timer,
        });
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
        // A failed shell's panels keep their room (`keep_panels`).
        if let Some(kept) = &self.kept {
            return kept.area;
        }
        let screen = self.screen_area();
        let Some(output) = &self.output else {
            return screen;
        };
        let mut area = layer_map_for_output(output).non_exclusive_zone();
        area.loc += screen.loc;
        area
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
        // A window came to the front or left it: the panels over fullscreen,
        // and what the pointer is over then.
        if self.reveal() {
            crate::input::refresh_pointer(self);
        }
        let hide_upper = self.fullscreen_in_front();
        let revealed = self.revealed;
        // Over a fullscreen window, in front or behind, the menu bar lies on
        // live glass (`backdrop`), in place of any glass it asked for.
        let plate = self
            .space
            .elements()
            .any(crate::windows::fullscreen)
            .then(|| self.band(Side::Top))
            .flatten()
            .filter(|_| !hide_upper || revealed == Some(Side::Top));
        let (Some(screen), Some(output)) = (&mut self.screen, &self.output) else {
            return;
        };
        self.redraw = false;
        let began: Duration = self.clock.now().into();
        let requested = self.changed_at.unwrap_or(began);
        let Some(area) = self.space.output_geometry(output) else {
            return;
        };
        // The background and the glass, once per size and appearance: drawn
        // with this frame, ahead of it.
        let want = (area.size.to_physical(1), self.appearance);
        if self.backdrop_for != Some(want) {
            self.backdrop_for = Some(want);
            self.backdrops += 1;
            self.backdrop = match Backdrop::new(screen.renderer(), want.0, want.1, self.backdrops) {
                Ok(backdrop) => Some(backdrop),
                Err(e) => {
                    warn!("drawing the background: {e}; a plain colour instead");
                    None
                }
            };
        }
        let at = self.pointer.to_physical(1.0).to_i32_round();
        let on_plane = self
            .cursor
            .image(began)
            .is_some_and(|i| screen.cursor_fits(i));
        // In front of all, the black the desktop fades in from.
        let veil = self.opening.veil(began);
        let mut elements: Vec<Element> = Vec::new();
        if veil != Veil::None {
            let alpha = match veil {
                Veil::Fading(alpha) => alpha,
                Veil::Black | Veil::None => 1.0,
            };
            if alpha != self.veil_alpha {
                self.veil_alpha = alpha;
                self.veil_commit.increment();
            }
            elements.push(veil_over(&self.veil_id, self.veil_commit, area, alpha));
        }
        if !on_plane && let Some((buffer, hotspot)) = self.cursor.drawn(began) {
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
        // The panels as the shell drew them, or those kept of a shell that
        // failed; lent to this frame and taken back after it.
        if self.kept.is_none() {
            self.panels_last = panels(screen.renderer(), output, self.backdrop.as_mut(), plate);
        }
        let source = match &mut self.kept {
            Some(kept) => &mut kept.panels,
            None => &mut self.panels_last,
        };
        let shown = |panel: &PanelFrame| {
            !panel.upper || !hide_upper || revealed == Some(side(area, panel.place))
        };
        let upper = lend(source, true, shown, &mut elements);
        let mut plated = false;
        if let Some(backdrop) = &mut self.backdrop {
            let glass = plate.map(|band| backdrop.plate(band));
            plated = glass.is_some();
            self.shell_windows.set_tone(backdrop.bar_tone(plated));
            elements.extend(glass.map(Element::Glass));
        }
        // Each window with its bar above it, front to back.
        let windows: Vec<_> = self
            .space
            .elements()
            .rev()
            .filter_map(|w| Some((w.clone(), self.space.element_geometry(w)?)))
            .filter(|(_, geometry)| geometry.overlaps(area))
            .collect();
        let mut bars = self.decorations.elements(screen.renderer(), &windows);
        for (window, geometry) in &windows {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let frame = crate::decorations::frame(window, *geometry);
            let rounded = crate::corners::rounded(window, frame).then_some(frame);
            let origin = geometry.loc - window.geometry().loc;
            // Popups in front, never cut by the corners (as Smithay's
            // `Window` places them).
            for (popup, offset) in PopupManager::popups_for_surface(toplevel.wl_surface()) {
                let at = origin + window.geometry().loc + offset - popup.geometry().loc;
                let surfaces = render_elements_from_surface_tree(
                    screen.renderer(),
                    popup.wl_surface(),
                    at.to_physical(1),
                    Scale::from(1.0),
                    1.0,
                    Kind::Unspecified,
                );
                elements.extend(surfaces.into_iter().map(Element::Surface));
            }
            let surfaces: Vec<WaylandSurfaceRenderElement<_>> = render_elements_from_surface_tree(
                screen.renderer(),
                toplevel.wl_surface(),
                origin.to_physical(1),
                Scale::from(1.0),
                1.0,
                Kind::Unspecified,
            );
            elements.extend(
                surfaces
                    .into_iter()
                    .map(|s| Element::Window(Rounded::new(s, rounded))),
            );
            if let Some(i) = bars.iter().position(|(w, _)| w == window) {
                let bar = bars.swap_remove(i).1;
                elements.push(Element::Bar(Rounded::new(bar, rounded)));
            }
            if let Some(frame) = rounded {
                elements.push(Element::Shadow(Shadow::new(window, frame)));
            }
        }
        let lower = lend(source, false, shown, &mut elements);
        if let Some(backdrop) = &self.backdrop {
            elements.push(Element::Wallpaper(backdrop.scrim()));
            elements.push(Element::Wallpaper(backdrop.wallpaper()));
        }
        // Live glass shows what lies behind it: back to front, so glass in
        // front sees glass behind it as it is now.
        self.live_glass.retain(|l| l.wanted(plated));
        let screen_size = area.size.to_physical(1);
        for i in (0..elements.len()).rev() {
            let (front, behind) = elements.split_at_mut(i + 1);
            let Element::Glass(glass) = &mut front[i] else {
                continue;
            };
            let Some(below) = glass.live().cloned() else {
                continue;
            };
            let live = match self.live_glass.iter().position(|l| l.is_for(&below)) {
                Some(j) => &mut self.live_glass[j],
                None => match LiveGlass::new(screen.renderer(), below) {
                    Ok(live) => {
                        self.live_glass.push(live);
                        let last = self.live_glass.len() - 1;
                        &mut self.live_glass[last]
                    }
                    Err(e) => {
                        warn!("live glass: {e}");
                        continue;
                    }
                },
            };
            if let Err(e) = live.update(
                screen.renderer(),
                behind,
                glass,
                screen_size,
                self.appearance,
            ) {
                warn!("live glass: {e}");
            }
        }
        // The pointer shows with the desktop.
        let cursor = self
            .cursor
            .image(began)
            .filter(|_| on_plane && veil == Veil::None)
            .map(|image| (image, at));
        // Until the desktop shows, the black alone: it sets the mode.
        let drawn = if veil == Veil::Black {
            &elements[..1]
        } else {
            &elements[..]
        };
        let frame = screen.render(drawn, cursor, requested, began);
        take_back(source, &mut elements, lower);
        take_back(source, &mut elements, upper);
        if !matches!(frame, Ok(Frame::Deferred)) {
            self.changed_at = None;
        }
        match frame {
            Ok(Frame::Queued) => {}
            Ok(Frame::Deferred) => self.redraw = true,
            Ok(Frame::Unchanged) => self.frame_done_later(),
            Err(e) => self.fail(e),
        }
        self.animate_cursor(began);
        // The fade draws each frame until it ends.
        if matches!(self.opening, Opening::Fading { .. }) {
            self.changed();
        }
    }

    /// An animated pointer (`cursor`): draws again when its next frame is
    /// due; a still one sets no timer.
    fn animate_cursor(&mut self, now: Duration) {
        if self.cursor_timer.is_some() {
            return;
        }
        let Some(next) = self.cursor.next_frame(now) else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(next.saturating_sub(now)),
            |_, _, state| {
                state.cursor_timer = None;
                state.changed();
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.cursor_timer = Some(token),
            Err(e) => warn!("cursor timer: {}", e.error),
        }
    }

    /// The pointer is on no client's surface: the compositor names its
    /// image – a resize arrow on a window's edge, as macOS shows it, else
    /// the arrow. Not during a grab: a resize keeps its arrow.
    pub fn own_cursor(&mut self) {
        if self.seat.get_pointer().is_some_and(|p| p.is_grabbed()) {
            return;
        }
        let icon = match self.edge_under(self.pointer).map(|(_, edges)| edges) {
            Some(ResizeEdge::Top | ResizeEdge::Bottom) => CursorIcon::NsResize,
            Some(ResizeEdge::Left | ResizeEdge::Right) => CursorIcon::EwResize,
            Some(ResizeEdge::TopLeft | ResizeEdge::BottomRight) => CursorIcon::NwseResize,
            Some(ResizeEdge::TopRight | ResizeEdge::BottomLeft) => CursorIcon::NeswResize,
            _ => CursorIcon::Default,
        };
        let now = self.clock.now().into();
        self.cursor.set(CursorImageStatus::Named(icon), now);
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
        // Not on the screen: "frame done" at most every HIDDEN_FRAMES.
        for window in &self.minimized {
            window.send_frame(&output, time, Some(HIDDEN_FRAMES), |_, _| None);
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
                .and_then(|output| crate::layers::surface_under(output, layers, at, |_| true))
        };
        self.panel_surface_under(at)
            .or_else(|| self.window_surface_under(at))
            .or_else(|| panels(&[Layer::Bottom, Layer::Background]))
    }

    /// The surface of a panel above the windows under `at`, if it shows.
    fn panel_surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.output.as_ref()?;
        crate::layers::surface_under(output, &[Layer::Overlay, Layer::Top], at, |place| {
            self.panel_shown(place)
        })
    }

    /// Whether a panel above the windows (the menu bar, the dock) is under
    /// `at`: a click there is the shell's, not the window's behind it.
    pub fn panel_under(&self, at: Point<f64, Logical>) -> bool {
        self.panel_surface_under(at).is_some()
    }

    /// The input clock now, in milliseconds as input events carry it.
    pub fn time_msec(&self) -> u32 {
        Duration::from(self.clock.now()).as_millis() as u32
    }

    /// The topmost window under `at`, its surface there and that surface's
    /// position; none on its title bar, which is the compositor's.
    fn window_surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, Hit::Content) = self.window_under(at)? else {
            return None;
        };
        let location = self.space.element_location(&window)? - window.geometry().loc;
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
        for other in self.space.elements().chain(&self.minimized) {
            if Some(other) != window.as_ref() {
                other.set_activated(false);
            }
        }
        let surface = window.as_ref().and_then(|window| {
            // Raising activates it too (Smithay sets the state itself).
            self.space.raise_element(window, true);
            window.set_activated(true);
            Some(window.toplevel()?.wl_surface().clone())
        });
        // Each window hears what changed for it – which `set_activated`'s
        // answer cannot tell once raising has set it; unchanged ones hear
        // nothing (`send_pending_configure`).
        for toplevel in self
            .space
            .elements()
            .chain(&self.minimized)
            .filter_map(Window::toplevel)
        {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        self.refresh_listing();
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
            if let Some(window) = self.window_for(&root) {
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
        let now = self.clock.now().into();
        self.cursor.set(image, now);
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
delegate_xdg_decoration!(State);
delegate_cursor_shape!(State);

/// cursor-shape-v1 also names tablet tools' images; Beize has no tablets
/// yet, so nothing is needed of it.
impl TabletSeatHandler for State {}

/// The black over all of `area` the desktop fades in from (`opening`), of
/// opacity `alpha`.
fn veil_over(id: &Id, commit: CommitCounter, area: Rectangle<i32, Logical>, alpha: f32) -> Element {
    Element::Veil(SolidColorRenderElement::new(
        id.clone(),
        Rectangle::from_size(area.size.to_physical(1)),
        commit,
        [0.0, 0.0, 0.0, alpha],
        Kind::Unspecified,
    ))
}

/// A panel as a frame draws it: its place, whether it lies above the
/// windows, its surfaces front to back and the glass below them.
struct PanelFrame {
    place: Rectangle<i32, Logical>,
    upper: bool,
    elements: Vec<Element>,
}

/// The panels of a shell that failed, as the last frame drew them, and the
/// room they left for windows (`State::keep_panels`).
struct Kept {
    panels: Vec<PanelFrame>,
    area: Rectangle<i32, Logical>,
    since: Duration,
    timer: RegistrationToken,
}

/// The panels, front to back, each with the glass it asked for below it
/// (with a `backdrop`; the menu bar's gives way to the `plate`).
fn panels(
    renderer: &mut VkRenderer,
    output: &WlOutput,
    mut backdrop: Option<&mut Backdrop>,
    plate: Option<Rectangle<i32, Logical>>,
) -> Vec<PanelFrame> {
    let map = layer_map_for_output(output);
    let mut drawn = Vec::new();
    // The map lists them back to front.
    for panel in map.layers().rev() {
        let Some(place) = map.layer_geometry(panel) else {
            continue;
        };
        let surfaces: Vec<WaylandSurfaceRenderElement<VkRenderer>> =
            AsRenderElements::<VkRenderer>::render_elements(
                panel,
                renderer,
                place.loc.to_physical(1),
                Scale::from(1.0),
                1.0,
            );
        // The menu bar's own glass gives way to the plate over a fullscreen
        // window.
        let glass = backdrop
            .as_deref_mut()
            .filter(|_| !plate.is_some_and(|band| band.contains_rect(place)))
            .and_then(|b| b.glass(renderer, panel.wl_surface(), place.loc));
        let mut elements: Vec<Element> = surfaces.into_iter().map(Element::Surface).collect();
        elements.extend(glass.map(Element::Glass));
        drawn.push(PanelFrame {
            place,
            upper: matches!(panel.layer(), Layer::Overlay | Layer::Top),
            elements,
        });
    }
    drawn
}

/// Moves the elements of the panels above the windows (`upper`) or below
/// them that are `shown` to the end of `list`; where each went, for
/// `take_back`.
fn lend(
    panels: &mut [PanelFrame],
    upper: bool,
    shown: impl Fn(&PanelFrame) -> bool,
    list: &mut Vec<Element>,
) -> Vec<(usize, Range<usize>)> {
    let mut lent = Vec::new();
    for (i, panel) in panels.iter_mut().enumerate() {
        if panel.upper != upper || !shown(panel) {
            continue;
        }
        let start = list.len();
        list.append(&mut panel.elements);
        lent.push((i, start..list.len()));
    }
    lent
}

/// Gives the panels back what `lend` moved to `list`, the last lent first
/// (so the places of the others stay); what lies after is left.
fn take_back(panels: &mut [PanelFrame], list: &mut Vec<Element>, lent: Vec<(usize, Range<usize>)>) {
    for (i, range) in lent.into_iter().rev() {
        panels[i].elements = list.drain(range).collect();
    }
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
        ended: shell.then(|| state.shell_ended.clone()),
        ..ClientState::default()
    };
    if let Err(e) = state.display.insert_client(stream, Arc::new(data)) {
        warn!("a client could not connect: {e}");
    }
}
