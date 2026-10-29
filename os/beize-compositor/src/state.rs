//! The compositor's state and the Wayland protocols it serves: surfaces
//! (wl_compositor, wl_shm, linux-dmabuf), the seat, the screen as
//! wl_output, copy and paste (wl_data_device), windows (xdg-shell,
//! `shell`) with their frames (xdg-decoration, `decorations`), the
//! shell's panels (wlr-layer-shell, `layers`) and its list of windows
//! (beize-shell-v1, `shell_v1`), cropped and scaled surfaces (viewporter),
//! windows that ask to become active (xdg-activation, `activation`) and
//! surfaces that keep the screen on (idle-inhibit, `idle`) and explicit
//! synchronisation (linux-drm-syncobj, `syncobj`).
//!
//! Drawing follows the screen, not the clients: any change marks the
//! screen for a redraw at the deadline of the next vblank (`schedule`),
//! which draws all changes until then at once, and clients hear "frame
//! done" when their frame is on the screen (its vblank) – a client draws
//! at most as often as the screen shows. With nothing to draw, they hear
//! it one refresh later.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::{DrmEvent, DrmNode};
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev::{UdevBackend, UdevEvent};
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states,
};
use smithay::desktop::{
    LayerSurface, PopupGrab, PopupManager, Space, Window, layer_map_for_output,
};
use smithay::input::keyboard::{KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutputMode, Output as WlOutput, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::channel::{self, Sender};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, LoopSignal, RegistrationToken};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_callback::WlCallback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{
    Clock, ClockSource, Logical, Monotonic, Physical, Point, Rectangle, SERIAL_COUNTER, Size,
    Transform,
};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::commit_timing::CommitTimingManagerState;
use smithay::wayland::compositor::{
    Barrier, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    SurfaceData, TraversalAction, get_parent, is_sync_subsurface, with_surface_tree_downward,
};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::drm_syncobj::{DrmSyncobjHandler, DrmSyncobjState, supports_syncobj_eventfd};
use smithay::wayland::fifo::{FifoBarrierCachedState, FifoManagerState};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::presentation::{PresentationState, Refresh};
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
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::{
    delegate_commit_timing, delegate_compositor, delegate_cursor_shape, delegate_data_device,
    delegate_dmabuf, delegate_drm_syncobj, delegate_fifo, delegate_idle_inhibit, delegate_output,
    delegate_presentation, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_decoration, delegate_xdg_shell,
};
use tracing::{error, info, warn};

use crate::backdrop::{Appearance, Backdrop, LiveGlass};
use crate::cursor::Cursor;
use crate::decorations::{Decorations, Hit};
use crate::error::{Context, Error, Result, error};
use crate::gpu::{self, Change, Gpu};
use crate::idle::{Idle, Woken};
use crate::keyboard::Layout;
use crate::layers::{Side, side};
use crate::opening::{Opening, SHELL_WAIT, Veil};
use crate::output::{FLIP_TIMEOUT, Frame, Info};
use crate::pacing::Parts;
use crate::scene::{self, Element};
use crate::syncobj::{self, Holds};

/// Key repeat: GNOME's defaults (500 ms, then every 30 ms) until Beize's
/// settings offer a choice.
const REPEAT_DELAY: i32 = 500;
const REPEAT_RATE: i32 = 33;

/// Surfaces not on the screen hear "frame done" at most this often.
const HIDDEN_FRAMES: Duration = Duration::from_secs(1);
/// How often a run of frames is checked for its end (`Pacing`).
const PACING_CHECK: Duration = Duration::from_millis(500);
/// How long the global of a replaced screen stays, hidden, before it goes:
/// a client may bind it in that moment and must not meet a protocol error
/// (wl_global_remove, as Mutter, wlroots and niri wait).
const GLOBAL_GRACE: Duration = Duration::from_secs(10);

pub struct State {
    pub display: DisplayHandle,
    pub handle: LoopHandle<'static, State>,
    signal: LoopSignal,
    pub session: LibSeatSession,
    pub libinput: Libinput,

    compositor: CompositorState,
    shm: ShmState,
    dmabuf: DmabufState,
    /// linux-drm-syncobj, if the GPU's driver signals eventfds from
    /// syncobjs (Smithay's condition, as Mutter's).
    syncobj: Option<DrmSyncobjState>,
    pub xdg_shell: XdgShellState,
    _xdg_decoration: XdgDecorationState,
    pub layer_shell: WlrLayerShellState,
    _outputs: OutputManagerState,
    seat_state: SeatState<State>,
    data_device: DataDeviceState,
    _cursor_shape: CursorShapeManagerState,
    _presentation: PresentationState,
    _fifo: FifoManagerState,
    _commit_timing: CommitTimingManagerState,
    _viewporter: ViewporterState,
    pub activation: XdgActivationState,
    _idle_inhibit: IdleInhibitManagerState,

    pub seat: Seat<State>,
    pub keyboard: KeyboardHandle<State>,
    /// What localed last set (`reload_layout`).
    layout: Layout,
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
    /// The menu holding pointer and keyboard (`shell`), until it ends.
    pub popup_grab: Option<PopupGrab<State>>,
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
    panels_last: Vec<scene::PanelFrame>,
    kept: Option<Kept>,
    /// The shell's connections tell their end here (`shell_ended`).
    shell_ended: Sender<bool>,
    /// The GPU and its screen: None until the session is first active, as
    /// only then does logind hand out the GPU (a session started on a
    /// background VT waits).
    gpu: Option<Gpu>,
    /// The screen for clients; it stays while none is connected, until
    /// another replaces it (ADR 0029 §3).
    pub output: Option<WlOutput>,
    output_global: Option<GlobalId>,
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
    /// Watches a commit for its vblank (`watch_flip`), and when it fires.
    flip_watch: Option<(RegistrationToken, Duration)>,
    /// Draws an animated pointer's next frame.
    cursor_timer: Option<RegistrationToken>,
    /// What clients hear once their content shows (`Shown`): that of the
    /// frame waiting for its vblank, and that of commits that changed
    /// nothing on the screen, told at the next vblank or a refresh later.
    presenting: Option<Shown>,
    unchanged: Vec<Shown>,
    /// The buffers synchronised explicitly that frames on the GPU read
    /// (`syncobj`): let go once it finished them.
    holds: Holds,
    /// What the last frame showed, changed or not (the damage tracker's
    /// states); the other surfaces are hidden.
    drawn: RenderElementStates,
    /// When hidden surfaces last heard "frame done".
    hidden_done: Option<Duration>,
    /// The wake of the frame loop for a commit that waits for its time
    /// (`timing`): its number and when.
    pub timed_wake: Option<(u64, Duration)>,
    /// Counts the wakes set, to know the current one by.
    pub timed_wakes: u64,
    /// When the screen goes off without input (`idle`), and the timer for
    /// it, set while it may.
    idle: Idle,
    idle_timer: Option<RegistrationToken>,
    /// The fade's next step (`Idle::next_step`).
    fade_timer: Option<RegistrationToken>,
    /// Surfaces with an idle inhibitor, once per inhibitor.
    inhibitors: Vec<WlSurface>,
    failure: Option<Error>,
}

/// What a frame drew of clients, told them once it is on the screen: when
/// (wp_presentation), and that they may draw their next frame ("frame
/// done") – a drawn frame's at once (`take_shown`), only a frame that
/// changed nothing keeps them for then.
struct Shown {
    feedback: Option<OutputPresentationFeedback>,
    callbacks: Vec<WlCallback>,
    /// What the next commits wait for (wp_fifo), with their client.
    fifo: Vec<(Barrier, Option<Client>)>,
}

/// The fifo barrier of a surface's content (`timing`), with its client.
fn take_fifo(surface: &WlSurface, states: &SurfaceData) -> Option<(Barrier, Option<Client>)> {
    let barrier = states
        .cached_state
        .get::<FifoBarrierCachedState>()
        .current()
        .barrier
        .take()?;
    Some((barrier, surface.client()))
}

/// Lifts fifo barriers; the clients whose commits waited for them.
fn lift(fifo: Vec<(Barrier, Option<Client>)>) -> Vec<Client> {
    fifo.into_iter()
        .filter_map(|(barrier, client)| {
            barrier.signal();
            client
        })
        .collect()
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
        let clock = Clock::<Monotonic>::new();
        let idle = Idle::from_environment(clock.now().into());
        match idle.after() {
            Some(after) => info!(
                "the screen goes off after {} s without input",
                after.as_secs()
            ),
            None => info!("the screen stays on without input"),
        }
        Ok(State {
            compositor: CompositorState::new::<State>(&display),
            shm: ShmState::new::<State>(&display, Vec::new()),
            dmabuf: DmabufState::new(),
            syncobj: None,
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
            // When a frame showed, for video and games (Mesa's Vulkan
            // present wait, GTK, mpv), on the clock of the vblank times.
            _presentation: PresentationState::new::<State>(&display, Monotonic::ID as u32),
            // Commits that wait for the last one to show, or for their time
            // (`timing`).
            _fifo: FifoManagerState::new::<State>(&display),
            _commit_timing: CommitTimingManagerState::new::<State>(&display),
            // Video players and browsers crop and scale their buffers;
            // Smithay applies it to what a surface shows (its view).
            _viewporter: ViewporterState::new::<State>(&display),
            activation: XdgActivationState::new::<State>(&display),
            // Video players and presentations keep the screen on (`idle`).
            _idle_inhibit: IdleInhibitManagerState::new::<State>(&display),
            seat_state,
            seat,
            keyboard,
            layout,
            taken_keys: HashSet::new(),
            space: Space::default(),
            unmapped: Vec::new(),
            minimized: Vec::new(),
            shell_windows: crate::shell_v1::ShellWindows::new(&display),
            popups: PopupManager::default(),
            popup_grab: None,
            decorations: Decorations::default(),
            appearance: Appearance::chosen(),
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
            gpu: None,
            output: None,
            output_global: None,
            pointer: Point::default(),
            revealed: None,
            fullscreen_front: false,
            cursor: Cursor::new(),
            clock,
            redraw: false,
            changed_at: None,
            draw_timer: None,
            pacing_timer: None,
            frame_timer: None,
            flip_watch: None,
            cursor_timer: None,
            presenting: None,
            unchanged: Vec::new(),
            holds: Holds::default(),
            drawn: RenderElementStates::default(),
            hidden_done: None,
            timed_wake: None,
            timed_wakes: 0,
            idle,
            idle_timer: None,
            fade_timer: None,
            inhibitors: Vec::new(),
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
        match &mut self.gpu {
            Some(gpu) => {
                gpu.resume()?;
                // The screens may have changed meanwhile.
                self.rescan();
            }
            None => self.start()?,
        }
        // Coming back to the session is use: a screen gone off comes on.
        self.input_came();
        self.redraw = true;
        self.schedule_render();
        Ok(())
    }

    /// Another session took the screen and the input devices.
    pub fn pause(&mut self) {
        self.libinput.suspend();
        if let Some(gpu) = &mut self.gpu {
            gpu.pause();
        }
    }

    /// Takes the GPU and the screen, and offers the screen to clients.
    fn start(&mut self) -> Result<()> {
        let (gpu, info, events) = gpu::open(&mut self.session)?;
        self.handle
            .insert_source(events, |event, metadata, state| match event {
                DrmEvent::VBlank(crtc) => {
                    state.flip_came(crtc);
                    let sequence = metadata.as_ref().map_or(0, |m| m.sequence);
                    let time = state
                        .gpu
                        .as_mut()
                        .and_then(|gpu| gpu.vblank(crtc, metadata.take()));
                    state.presented(time, sequence);
                    // `presented` took the frame that showed.
                    state.release_read();
                    state.frame_done(time);
                    if state.redraw {
                        state.schedule_render();
                    }
                    state.watch_pacing();
                }
                DrmEvent::Error(e) => error!("DRM: {e}"),
            })
            .map_err(|e| error(format!("DRM events: {}", e.error)))?;
        // Screens that come and go, and links that break, are changes of
        // the GPU's udev device (ADR 0029 §2).
        let device = gpu.device_id();
        let udev = UdevBackend::new(self.session.seat()).context("udev")?;
        self.handle
            .insert_source(udev, move |event, _, state| match event {
                UdevEvent::Changed { device_id } if device_id == device => state.rescan(),
                UdevEvent::Removed { device_id } if device_id == device => {
                    warn!("the GPU is gone");
                }
                _ => {}
            })
            .map_err(|e| error(format!("udev events: {}", e.error)))?;
        let node = gpu.node();
        self.gpu = Some(gpu);
        self.offer_dmabuf(node);
        self.offer_syncobj();
        match info {
            Some(info) => self.first_screen(&info),
            None => {
                info!("no screen connected: the desktop starts once one is");
                Ok(())
            }
        }
    }

    /// The first screen: offered to clients, the desktop's start begins
    /// (`opening`) and the compositor tells systemd it is ready – only now,
    /// so that the shell and apps start when there is a screen to show
    /// them on; without one, nothing is drawn or started, as there is
    /// nothing to see.
    fn first_screen(&mut self, info: &Info) -> Result<()> {
        self.offer_output(info);
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
        let black = scene::veil_over(&self.veil_id, self.veil_commit, self.screen_area(), 1.0);
        if let Some(gpu) = &mut self.gpu {
            gpu.render(&[black], None, now, now, Parts::default())?;
        }
        self.watch_flip();
        crate::notify_ready();
        Ok(())
    }

    /// The GPU changed (udev): its screens are looked at anew (ADR 0029).
    /// A screen that went leaves the windows as they are and clients hear
    /// "frame done" as hidden ones do; one that came is drawn in full.
    fn rescan(&mut self) {
        if !self.session.is_active() {
            // `activate` looks.
            return;
        }
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match gpu.scan(true) {
            Ok(Change::None) => {}
            Ok(Change::Gone) => {
                info!("no screen: windows keep their places until one comes");
                // Its last frame will not show.
                if let Some(shown) = self.presenting.take() {
                    self.never_shown(shown);
                }
                self.release_read();
                self.frame_done_later();
            }
            Ok(Change::Retrained) => self.changed(),
            Ok(Change::New(info)) if self.output.is_none() => {
                if let Err(e) = self.first_screen(&info) {
                    self.fail(e);
                }
            }
            Ok(Change::New(info)) => {
                self.offer_output(&info);
                let area = self.screen_area().to_f64();
                self.pointer = Point::from((
                    self.pointer
                        .x
                        .clamp(area.loc.x, area.loc.x + area.size.w - 1.0),
                    self.pointer
                        .y
                        .clamp(area.loc.y, area.loc.y + area.size.h - 1.0),
                ));
                self.arrange_windows();
                self.changed();
            }
            Err(e) => warn!("looking at the screens: {e}"),
        }
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
        for panel in &mut panels {
            panel
                .elements
                .retain(|e| !matches!(e, Element::Glass(glass) if !glass.keeps()));
            for element in &mut panel.elements {
                if let Element::Glass(glass) = element {
                    glass.freeze();
                }
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

    /// The screen for clients: the one they have in its new mode, or – a
    /// different connector – a new wl_output that takes over the panels.
    fn offer_output(&mut self, info: &Info) {
        let (w, h) = info.mode.size();
        let mode = OutputMode {
            size: (i32::from(w), i32::from(h)).into(),
            refresh: i32::try_from(gpu::millihertz(&info.mode)).unwrap_or(i32::MAX),
        };
        if let Some(output) = &self.output
            && output.name() == info.name
        {
            let before = output.current_mode();
            output.change_current_state(Some(mode), None, None, None);
            output.set_preferred(mode);
            if let Some(before) = before.filter(|b| *b != mode) {
                output.delete_mode(before);
            }
            // The panels hear the new size.
            layer_map_for_output(output).arrange();
            return;
        }
        let output = WlOutput::new(
            info.name.clone(),
            PhysicalProperties {
                size: info.size_mm.into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: info.name.clone(),
            },
        );
        let global = output.create_global::<State>(&self.display);
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        self.space.map_output(&output, (0, 0));
        let before = self.output.replace(output.clone());
        let before_global = self.output_global.replace(global);
        let (Some(before), Some(before_global)) = (before, before_global) else {
            return;
        };
        info!("{} replaces {} for clients", info.name, before.name());
        // One screen: the panels go where they went, whichever they named.
        let panels: Vec<LayerSurface> = layer_map_for_output(&before).layers().cloned().collect();
        for panel in panels {
            layer_map_for_output(&before).unmap_layer(&panel);
            if let Err(e) = layer_map_for_output(&output).map_layer(&panel) {
                warn!("a panel: {e}");
            }
        }
        layer_map_for_output(&output).arrange();
        self.space.unmap_output(&before);
        self.display.disable_global::<State>(before_global.clone());
        let removed =
            self.handle
                .insert_source(Timer::from_duration(GLOBAL_GRACE), move |_, _, state| {
                    state.display.remove_global::<State>(before_global.clone());
                    TimeoutAction::Drop
                });
        if let Err(e) = removed {
            warn!("the replaced screen's global stays: {}", e.error);
        }
    }

    /// linux-dmabuf with the formats the renderer imports, the GPU as the
    /// main device (feedback, version 4).
    fn offer_dmabuf(&mut self, node: DrmNode) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let formats = gpu.renderer().dmabuf_formats();
        match DmabufFeedbackBuilder::new(node.dev_id(), formats).build() {
            Ok(feedback) => {
                self.dmabuf
                    .create_global_with_default_feedback::<State>(&self.display, &feedback);
            }
            Err(e) => warn!("no linux-dmabuf for clients: {e}"),
        }
    }

    /// linux-drm-syncobj, with the GPU importing the clients' timelines (as
    /// anvil): only if its driver signals an eventfd from a syncobj point,
    /// or no commit could wait for its acquire point.
    fn offer_syncobj(&mut self) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let device = gpu.device_fd().clone();
        if supports_syncobj_eventfd(&device) {
            self.syncobj = Some(DrmSyncobjState::new::<State>(&self.display, device));
            info!("explicit sync for clients (linux-drm-syncobj)");
        } else {
            info!("no explicit sync: the GPU's driver has no syncobj eventfd");
        }
    }

    /// Lets go of the buffers synchronised explicitly that finished frames
    /// read (`syncobj`): at each vblank – KMS waited for the frame's fence,
    /// which signals after the submission's timeline –, and while no frame
    /// waits for its vblank when clients hear "frame done" without one (no
    /// screen, nothing drawn). So a buffer is let go no earlier than the
    /// vblank of the last frame that showed it, as a buffer on a plane of
    /// its own will need (direct scanout). With some left and no vblank to
    /// come, it looks again a refresh later.
    fn release_read(&mut self) {
        if self.presenting.is_some() {
            return;
        }
        if let Some(gpu) = &mut self.gpu {
            let completed = gpu.renderer().completed();
            self.holds.done(completed);
        }
        if !self.holds.is_empty() {
            self.frame_done_later();
        }
    }

    /// Saves what the screen shows (`screenshot`).
    pub fn screenshot(&mut self) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        match gpu.screenshot() {
            Ok((size, rgb)) => crate::screenshot::save(size, rgb),
            Err(e) => warn!("screenshot: {e}"),
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
    /// has the screen: `activate` calls again. Without a screen, clients
    /// hear "frame done" as hidden ones do.
    fn schedule_render(&mut self) {
        if self.draw_timer.is_some() || !self.session.is_active() {
            return;
        }
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        if !gpu.shows() {
            self.frame_done_later();
            return;
        }
        let Some(at) = gpu.draw_at() else {
            return;
        };
        let now: Duration = self.clock.now().into();
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                // Commits due by the vblank this frame shows at apply first
                // (`timing`); while this timer is set, they set no other.
                if let Some(vblank) = state.gpu.as_ref().and_then(Gpu::aim) {
                    state.release_timed(vblank);
                }
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
        // An inhibitor shown until now takes a fade back before it draws.
        self.look_at_inhibitors();
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
        let (Some(gpu), Some(output)) = (&mut self.gpu, &self.output) else {
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
            self.backdrop = match Backdrop::new(gpu.renderer(), want.0, want.1, self.backdrops) {
                Ok(backdrop) => Some(backdrop),
                Err(e) => {
                    warn!("drawing the background: {e}; a plain colour instead");
                    None
                }
            };
        }
        let at = self.pointer.to_physical(1.0).to_i32_round();
        let on_plane = self.cursor.image(began).is_some_and(|i| gpu.cursor_fits(i));
        // In front of all, the black the desktop fades in from, or the
        // black it fades to before the screen goes off (`idle`), the
        // pointer still over it.
        let veil = self.opening.veil(began);
        let shade = match (veil, self.idle.black(began)) {
            (Veil::None, Some(black)) if black > 0.0 => Veil::Fading(black),
            _ => veil,
        };
        let mut elements: Vec<Element> = Vec::new();
        if shade != Veil::None {
            let alpha = match shade {
                Veil::Fading(alpha) => alpha,
                Veil::Black | Veil::None => 1.0,
            };
            if alpha != self.veil_alpha {
                self.veil_alpha = alpha;
                self.veil_commit.increment();
            }
            elements.push(scene::veil_over(
                &self.veil_id,
                self.veil_commit,
                area,
                alpha,
            ));
        }
        if !on_plane && let Some((buffer, hotspot)) = self.cursor.drawn(began) {
            let location = (at - Point::from(hotspot)).to_f64();
            match MemoryRenderBufferRenderElement::from_buffer(
                gpu.renderer(),
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
            self.panels_last = scene::panels(gpu.renderer(), output, self.backdrop.as_mut(), plate);
        }
        let source = match &mut self.kept {
            Some(kept) => &mut kept.panels,
            None => &mut self.panels_last,
        };
        let shown = |panel: &scene::PanelFrame| {
            !panel.upper || !hide_upper || revealed == Some(side(area, panel.place))
        };
        let upper = scene::lend(source, true, shown, &mut elements);
        let mut plated = false;
        if let Some(backdrop) = &mut self.backdrop {
            let glass = plate.map(|band| backdrop.plate(band));
            plated = glass.is_some();
            self.shell_windows.set_tone(backdrop.bar_tone(plated));
            elements.extend(glass.map(Element::Glass));
        }
        // Each window with its bar above it, front to back.
        scene::windows(
            gpu.renderer(),
            &self.space,
            &mut self.decorations,
            area,
            &mut elements,
        );
        let lower = scene::lend(source, false, shown, &mut elements);
        if let Some(backdrop) = &self.backdrop {
            elements.push(Element::Wallpaper(backdrop.scrim()));
            elements.push(Element::Wallpaper(backdrop.wallpaper()));
        }
        let scene_done: Duration = self.clock.now().into();
        self.live_glass.retain(|l| l.wanted(plated));
        scene::live_glass(
            gpu.renderer(),
            &mut self.live_glass,
            &mut elements,
            area.size.to_physical(1),
            self.appearance,
        );
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
        let now: Duration = self.clock.now().into();
        let parts = Parts {
            scene: scene_done.saturating_sub(began),
            glass: now.saturating_sub(scene_done),
            ..Parts::default()
        };
        let frame = gpu.render(drawn, cursor, requested, began, parts);
        scene::take_back(source, &mut elements, lower);
        scene::take_back(source, &mut elements, upper);
        if !matches!(frame, Ok(Frame::Deferred)) {
            self.changed_at = None;
        }
        match frame {
            Ok(Frame::Queued(shown)) => {
                self.hold_read(Some(&shown));
                let (mut taken, hidden) = self.take_shown(&shown);
                // Drawn: their clients may draw the next frame now, a
                // refresh ahead of the next drawing (`take_shown`).
                let msec = Duration::from(self.clock.now()).as_millis() as u32;
                for callback in taken.callbacks.drain(..) {
                    callback.done(msec);
                }
                if let Some(old) = self.presenting.replace(taken) {
                    self.never_shown(old);
                }
                self.drawn = shown;
                self.unblock(lift(hidden));
            }
            Ok(Frame::Deferred) => self.redraw = true,
            Ok(Frame::Unchanged(shown)) => {
                let (taken, hidden) = self.take_shown(&shown);
                self.unchanged.push(taken);
                self.drawn = shown;
                self.unblock(lift(hidden));
                self.frame_done_later();
            }
            // One frame lost, not the session with its windows: said, and
            // drawn again at the next vblank, as Mutter, KWin and niri go on
            // after a frame that failed. What it would have shown waits
            // for that frame, its clients' callbacks with it.
            Err(e) => {
                warn!("a frame failed: {e:#}");
                // It may have reached the GPU: all it could have read.
                self.hold_read(None);
                self.changed();
            }
        }
        self.watch_flip();
        self.animate_cursor(began);
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

    /// The arrow, until a client names another image.
    pub fn default_cursor(&mut self) {
        let now = self.clock.now().into();
        self.cursor
            .set(CursorImageStatus::Named(CursorIcon::Default), now);
    }

    /// A commit went to KMS: if its vblank has not come after
    /// `FLIP_TIMEOUT`, that is said each time, as KWin's commit thread says
    /// it, and waited for further – nothing is reset; the vblank, when it
    /// comes, says how late (`Output::vblank`). An event that lay unread
    /// meanwhile means the compositor was held up, not the driver (KWin
    /// asks its main thread). Only while a commit waits: idle, no timer.
    fn watch_flip(&mut self) {
        if self.flip_watch.is_some() {
            return;
        }
        // Counted from when it could show (`Output::flip_since`); while the
        // GPU still draws, from now, looked at again then.
        let Some(since) = self.gpu.as_ref().and_then(Gpu::flip_since) else {
            return;
        };
        let now = self.now();
        let at = since.unwrap_or(now) + FLIP_TIMEOUT;
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                let now = state.now();
                // Another session's screen is not ours to watch.
                let waiting = state
                    .gpu
                    .as_ref()
                    .filter(|_| state.session.is_active())
                    .and_then(|gpu| Some((gpu, gpu.flip_since()?)));
                let Some((gpu, since)) = waiting else {
                    state.flip_watch = None;
                    return TimeoutAction::Drop;
                };
                let next = match since {
                    // The GPU still draws the frame (a client's long work it
                    // waits for): not KMS's to show yet.
                    None => FLIP_TIMEOUT,
                    // A later commit than the one this was set for.
                    Some(since) if now.saturating_sub(since) < FLIP_TIMEOUT => {
                        FLIP_TIMEOUT - now.saturating_sub(since)
                    }
                    Some(since) => {
                        let waited = now.saturating_sub(since).as_millis();
                        // Held up (a VM's CPU stopped, the system swapping) is
                        // no fault of the compositor or the driver: said, not
                        // warned of.
                        if gpu.events_waiting() {
                            info!(
                                "the compositor was held up: a vblank waited {waited} ms to be read"
                            );
                        } else {
                            error!(
                                "page flip timed out, no vblank {waited} ms after the frame \
                                 was committed and drawn: a bug in the kernel driver"
                            );
                        }
                        FLIP_TIMEOUT
                    }
                };
                if let Some((_, at)) = &mut state.flip_watch {
                    *at = now + next;
                }
                TimeoutAction::ToDuration(next)
            },
        );
        match token {
            Ok(token) => self.flip_watch = Some((token, at)),
            Err(e) => warn!("page flip timer: {}", e.error),
        }
    }

    /// The vblank of a commit on `crtc` came: the watch goes – not for an
    /// event of a screen gone, nor once its time is reached: then its event
    /// may wait in this same dispatch (calloop warns of an event for a
    /// source gone); it finds no commit then and goes itself.
    fn flip_came(&mut self, crtc: smithay::reexports::drm::control::crtc::Handle) {
        if !self.gpu.as_ref().is_some_and(|gpu| gpu.flipping_on(crtc)) {
            return;
        }
        let now = self.now();
        if let Some((token, at)) = self.flip_watch
            && now < at
        {
            self.flip_watch = None;
            self.handle.remove(token);
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
                let running = state.gpu.as_mut().is_some_and(Gpu::pacing_idle);
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
    /// without changing what the screen shows; without a screen, as for
    /// hidden ones.
    fn frame_done_later(&mut self) {
        if self.frame_timer.is_some() {
            return;
        }
        let refresh = self.gpu.as_ref().map_or(Duration::from_millis(16), |gpu| {
            gpu.refresh().unwrap_or(HIDDEN_FRAMES)
        });
        let token = self
            .handle
            .insert_source(Timer::from_duration(refresh), |_, _, state| {
                state.frame_timer = None;
                // Without a screen no frame is drawn: what is due applies
                // now (`timing`).
                if !state.gpu.as_ref().is_some_and(Gpu::shows) {
                    let now = state.now();
                    state.release_timed(now);
                }
                state.presented_unchanged();
                state.release_read();
                state.frame_done(None);
                TimeoutAction::Drop
            });
        match token {
            Ok(token) => self.frame_timer = Some(token),
            Err(e) => warn!("frame timer: {}", e.error),
        }
    }

    /// Each surface of the windows – with their popups, minimized ones too –
    /// and of the panels.
    pub fn each_surface(&self, mut f: impl FnMut(&WlSurface, &SurfaceData)) {
        let mut tree = |surface: &WlSurface| {
            with_surface_tree_downward(
                surface,
                (),
                |_, _, &()| TraversalAction::DoChildren(()),
                |surface, states, &()| f(surface, states),
                |_, _, &()| true,
            );
        };
        // With windows not shown yet: a first commit held back for its time
        // (wp_commit_timing) has to be let go, or the window never maps.
        for window in self
            .space
            .elements()
            .chain(&self.minimized)
            .chain(&self.unmapped)
        {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let surface = toplevel.wl_surface();
            tree(surface);
            for (popup, _) in PopupManager::popups_for_surface(surface) {
                tree(popup.wl_surface());
            }
        }
        if let Some(output) = &self.output {
            for panel in layer_map_for_output(output).layers() {
                let surface = panel.wl_surface();
                tree(surface);
                // As Smithay draws them with their panel.
                for (popup, _) in PopupManager::popups_for_surface(surface) {
                    tree(popup.wl_surface());
                }
            }
        }
    }

    /// Takes from the surfaces a frame shows (`shown`, the damage tracker's
    /// states, which name each element in view, changed or not) what is
    /// told of them: their "frame done" callbacks, answered once the frame
    /// is drawn, and their presentation feedback, told when it is on the
    /// screen. Only those of commits the frame draws: one that arrives
    /// after drawing began keeps its own for the next frame, as Weston
    /// takes them at repaint and answers them right after it
    /// (`weston_output_repaint`; anvil's `post_repaint` too) – told at the
    /// vblank before (until 0.5.104), its client drew the next frame at
    /// once and the one not shown was discarded; told at the frame's own
    /// vblank (until 0.5.107), a client had only what was left of the
    /// refresh before the next drawing, ~1 ms at 240 Hz (hardware test
    /// 26.10.: 9 of the load test's 12 windows at 165 frames a second,
    /// not 240). The surfaces not
    /// shown keep theirs until their next commit discards the feedback, as
    /// the protocol asks for content never seen. Their fifo barriers too
    /// (`timing`): the next commit applies once this one showed. Those of
    /// the surfaces not shown are returned, to be lifted now: wp_fifo lets
    /// a compositor ignore the barrier of a surface it does not update, its
    /// client throttles itself (Mesa by commit times); held, Mesa's FIFO
    /// blocked a covered app in its present, events and all (25.10.).
    fn take_shown(&self, shown: &RenderElementStates) -> (Shown, Vec<(Barrier, Option<Client>)>) {
        let mut callbacks = Vec::new();
        let mut fifo = Vec::new();
        let mut hidden = Vec::new();
        self.each_surface(|surface, states| {
            if shown.element_was_presented(surface) {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                callbacks.append(&mut attributes.current().frame_callbacks);
                fifo.extend(take_fifo(surface, states));
            } else {
                hidden.extend(take_fifo(surface, states));
            }
        });
        let feedback = self.output.as_ref().map(|output| {
            let mut feedback = OutputPresentationFeedback::new(output);
            let on = |surface: &WlSurface, _: &SurfaceData| {
                shown.element_was_presented(surface).then(|| output.clone())
            };
            let flags = |surface: &WlSurface, _: &SurfaceData| {
                surface_presentation_feedback_flags_from_states(surface, shown)
            };
            for window in self.space.elements() {
                window.take_presentation_feedback(&mut feedback, on, flags);
            }
            for panel in layer_map_for_output(output).layers() {
                panel.take_presentation_feedback(&mut feedback, on, flags);
            }
            feedback
        });
        let shown = Shown {
            feedback,
            callbacks,
            fifo,
        };
        (shown, hidden)
    }

    /// Keeps the buffers synchronised explicitly that the frame just
    /// drawn read – of the surfaces it shows (`shown`), or of all – until
    /// the GPU finished it (`release_read`).
    fn hold_read(&mut self, shown: Option<&RenderElementStates>) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let renderer = gpu.renderer_ref();
        let mut held = Vec::new();
        self.each_surface(|surface, states| {
            if shown.is_none_or(|shown| shown.element_was_presented(surface)) {
                held.extend(syncobj::held(renderer, states));
            }
        });
        let submission = renderer.submitted();
        self.holds.add(submission, held);
    }

    /// Tells clients their content showed: that of the frame KMS just
    /// showed, and of commits that changed nothing since, at the vblank
    /// `time` with its `sequence`, timed by the hardware (as Smithay's
    /// DrmCompositor reports it). Without a vblank time, none is exact.
    fn presented(&mut self, time: Option<Duration>, sequence: u32) {
        let kind = wp_presentation_feedback::Kind::Vsync
            | wp_presentation_feedback::Kind::HwClock
            | wp_presentation_feedback::Kind::HwCompletion;
        let (at, kind) = match time {
            Some(time) => (time, kind),
            None => (
                self.clock.now().into(),
                wp_presentation_feedback::Kind::empty(),
            ),
        };
        let mut shown: Vec<_> = self.presenting.take().into_iter().collect();
        shown.append(&mut self.unchanged);
        self.present(shown, at, u64::from(sequence), kind);
    }

    /// Commits that changed nothing on the screen, when no vblank came
    /// since: shown now, a refresh after them, without the hardware's
    /// flags (as niri reports them).
    fn presented_unchanged(&mut self) {
        let shown = std::mem::take(&mut self.unchanged);
        let now = self.clock.now().into();
        self.present(shown, now, 0, wp_presentation_feedback::Kind::empty());
    }

    fn present(
        &mut self,
        shown: Vec<Shown>,
        at: Duration,
        sequence: u64,
        kind: wp_presentation_feedback::Kind,
    ) {
        let refresh = self
            .gpu
            .as_ref()
            .and_then(Gpu::refresh)
            .map_or(Refresh::Unknown, Refresh::fixed);
        let time = smithay::utils::Time::<Monotonic>::from(at);
        // In milliseconds, as wl_callback.done carries it.
        let msec = at.as_millis() as u32;
        let mut released = Vec::new();
        for shown in shown {
            if let Some(mut feedback) = shown.feedback {
                feedback.presented(time, refresh, sequence, kind);
            }
            for callback in shown.callbacks {
                callback.done(msec);
            }
            released.extend(lift(shown.fifo));
        }
        self.unblock(released);
    }

    /// A frame that will not show (its screen went, or a new frame replaced
    /// it while the session was away): its content counts as discarded, and
    /// its clients hear "frame done" now, so none waits for it.
    fn never_shown(&mut self, shown: Shown) {
        if let Some(mut feedback) = shown.feedback {
            feedback.discarded();
        }
        let now: Duration = self.clock.now().into();
        for callback in shown.callbacks {
            callback.done(now.as_millis() as u32);
        }
        let released = lift(shown.fifo);
        self.unblock(released);
    }

    /// Clients whose held-back commits may apply now, a barrier lifted
    /// (`timing`): Smithay applies them, each through `commit`.
    pub fn unblock(&mut self, released: Vec<Client>) {
        let mut clients: Vec<Client> = Vec::new();
        for client in released {
            if !clients.iter().any(|c| c.id() == client.id()) {
                clients.push(client);
            }
        }
        let display = self.display.clone();
        for client in clients {
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor.blocker_cleared(self, &display);
            }
        }
    }

    pub fn now(&self) -> Duration {
        self.clock.now().into()
    }

    /// The time between two vblanks; without a screen, 16 ms.
    pub fn refresh(&self) -> Duration {
        self.gpu
            .as_ref()
            .and_then(Gpu::refresh)
            .unwrap_or(Duration::from_millis(16))
    }

    /// The first vblank at or after `time`; without a screen, `time`.
    pub fn vblank_from(&self, time: Duration) -> Duration {
        self.gpu
            .as_ref()
            .and_then(|gpu| gpu.vblank_from(time))
            .unwrap_or(time)
    }

    /// "Frame done" for the surfaces the last frame did not show – off the
    /// screen, covered, minimized, or all without a screen – at most every
    /// HIDDEN_FRAMES, at `time` (the vblank) or now; and for the pointer's.
    /// Those shown hear it once their frame is drawn (`render`). Their fifo
    /// barriers are lifted by each draw (`take_shown`), which knows what it
    /// shows; here only without a screen. Lifted here against the last
    /// frame, a window's first commit counted as hidden – not in that frame
    /// yet – and its next replaced it unseen (25.10.).
    fn frame_done(&mut self, time: Option<Duration>) {
        let Some(output) = self.output.clone() else {
            return;
        };
        let time = time.unwrap_or_else(|| self.clock.now().into());
        let shows = self.gpu.as_ref().is_some_and(Gpu::shows);
        let callbacks = self
            .hidden_done
            .is_none_or(|last| time.saturating_sub(last) >= HIDDEN_FRAMES);
        if callbacks {
            self.hidden_done = Some(time);
        }
        let drawn = shows.then_some(&self.drawn);
        let msec = time.as_millis() as u32;
        let mut fifo = Vec::new();
        self.each_surface(|surface, states| {
            if drawn.is_some_and(|d| d.element_was_presented(surface)) {
                return;
            }
            if callbacks {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                for callback in attributes.current().frame_callbacks.drain(..) {
                    callback.done(msec);
                }
            }
            if !shows {
                fifo.extend(take_fifo(surface, states));
            }
        });
        self.unblock(lift(fifo));
        let shown = shows.then(|| output.clone());
        if let Some(surface) = self.cursor.surface() {
            send_frames_surface_tree(surface, &output, time, Some(HIDDEN_FRAMES), |_, _| {
                shown.clone()
            });
        }
        self.space.refresh();
        self.popups.cleanup();
        // The compositor's own animation (the fade) asks for its next frame
        // here, as a client does on "frame done": the change dates from
        // this vblank and is drawn at the next one's deadline. Asked for
        // while drawing (until 0.5.102), its frame was dated before the
        // last one was shown, could only be drawn at that one's vblank, at
        // once – every frame of the fade late (hardware test 24.10.).
        if matches!(self.opening, Opening::Fading { .. }) {
            self.changed();
        }
        // The fade to black (`idle`) asks for its next step once one is
        // shown; once all black is on the screen, it goes off.
        let now = self.now();
        if self.idle.faded(now) {
            self.screen_off();
        } else {
            self.fade_step();
        }
    }

    /// While fading, draws when the black next gets darker
    /// (`Idle::next_step`); a fade taken back meanwhile draws nothing.
    fn fade_step(&mut self) {
        if self.fade_timer.is_some() {
            return;
        }
        let now = self.now();
        let Some(at) = self.idle.next_step(now) else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                state.fade_timer = None;
                if state.idle.black(state.now()).is_some() {
                    state.changed();
                }
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.fade_timer = Some(token),
            Err(e) => warn!("fade timer: {}", e.error),
        }
    }

    /// Input came (`input`), or the session came back: the time without
    /// input starts anew, a fade is taken back, a screen gone off comes on
    /// – drawn in full, which sets its mode.
    pub fn input_came(&mut self) {
        let now = self.now();
        match self.idle.input(now) {
            Woken::Nothing => {}
            Woken::Fade => {
                info!("input: the fade to black is taken back");
                self.changed();
            }
            Woken::Screen => {
                info!("input: the screen is on again");
                if let Some(gpu) = &mut self.gpu {
                    gpu.wake();
                }
                self.changed();
            }
        }
        self.set_idle_timer();
    }

    /// Sets the timer for when the screen fades without input, unless one
    /// is set: it runs out at the time the last use allows, looks whether
    /// there was use since and, if so, runs on for the rest – one wake per
    /// such time, none per input event.
    fn set_idle_timer(&mut self) {
        if self.idle_timer.is_some() {
            return;
        }
        let Some(due) = self.idle.due() else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(due.saturating_sub(self.now())),
            |_, _, state| {
                let now = state.now();
                // Another session's time is not ours to count.
                if !state.session.is_active() {
                    state.idle.inhibited(now);
                }
                state.look_at_inhibitors();
                if state.idle.fade(now) {
                    state.idle_timer = None;
                    state.fade_out();
                    return TimeoutAction::Drop;
                }
                match state.idle.due() {
                    Some(due) => TimeoutAction::ToDuration(due.saturating_sub(now)),
                    None => {
                        state.idle_timer = None;
                        TimeoutAction::Drop
                    }
                }
            },
        );
        match token {
            Ok(token) => self.idle_timer = Some(token),
            Err(e) => warn!("idle timer: {}", e.error),
        }
    }

    /// No input for the time: the screen fades to black, then goes off
    /// (`frame_done`); without a screen to fade, off at once.
    fn fade_out(&mut self) {
        let seconds = self.idle.after().unwrap_or_default().as_secs();
        if self.gpu.as_ref().is_some_and(Gpu::shows) {
            info!("no input for {seconds} s: the screen fades to black");
            self.fade_step();
        } else {
            info!("no input for {seconds} s: no screen to fade, off");
            self.screen_off();
        }
    }

    /// The fade is done: the screen goes off. A frame still waiting for its
    /// vblank defers it to that vblank's `frame_done`.
    fn screen_off(&mut self) {
        let slept = match &mut self.gpu {
            Some(gpu) => gpu.sleep(),
            None => Ok(true),
        };
        match slept {
            Ok(false) => return,
            Ok(true) => info!("the screen is off"),
            // Not drawn on either way until input comes.
            Err(e) => warn!("{e:#}; nothing is drawn until input comes"),
        }
        self.idle.sleep();
    }

    /// The idle inhibitors (`idle`): those of surfaces gone are dropped; one
    /// on a surface the last frame showed – until now, also if it went
    /// since – counts as use, and takes a fade back.
    fn look_at_inhibitors(&mut self) {
        if self.inhibitors.is_empty() {
            return;
        }
        let drawn = &self.drawn;
        let mut seen = false;
        self.inhibitors.retain(|surface| {
            seen |= drawn.element_was_presented(surface);
            surface.is_alive()
        });
        let now = self.now();
        if seen && self.idle.inhibited(now) {
            info!("an idle inhibitor shows: the fade to black is taken back");
            self.changed();
            self.set_idle_timer();
        }
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

    /// After events: once a menu's grab ended – a click elsewhere, a choice,
    /// Escape – the keyboard goes back to the active window, as niri gives
    /// it back. Smithay hands it to the grab's root, which for the shell's
    /// menus is a panel that takes no keys.
    pub fn popup_grab_ended(&mut self) {
        if self.popup_grab.is_none() {
            return;
        }
        // A grab ends once its popups are known gone (Smithay's `has_ended`),
        // which `cleanup` finds out – cheap, so not left to the next frame.
        self.popups.cleanup();
        if !self
            .popup_grab
            .as_ref()
            .is_some_and(|grab| grab.has_ended())
        {
            return;
        }
        let Some(grab) = self.popup_grab.take() else {
            return;
        };
        self.let_go_of(&grab);
        let active = self
            .space
            .elements()
            .rev()
            .filter_map(Window::toplevel)
            .find(|t| t.with_pending_state(|s| s.states.contains(XdgState::Activated)))
            .map(|t| t.wl_surface().clone());
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, active, SERIAL_COUNTER.next_serial());
    }

    /// Lets go of the pointer and keyboard that a menu's `grab` holds, now:
    /// Smithay's grabs let go only at their next event, and the pointer's
    /// drops it – a motion: the press after it went where the pointer had
    /// been (smoke, 28.10.). As `shell::grab` lets go of a menu closed just
    /// before another opens.
    pub fn let_go_of(&mut self, grab: &PopupGrab<State>) {
        if let Some(pointer) = self.seat.get_pointer()
            && pointer.has_grab(grab.serial())
        {
            let serial = SERIAL_COUNTER.next_serial();
            pointer.unset_grab(self, serial, self.time_msec());
        }
        let keyboard = self.keyboard.clone();
        if keyboard.has_grab(grab.serial()) {
            keyboard.unset_grab(self);
        }
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
    /// Only a layout other than the last: localed's one write is two
    /// events (its file linked over the old one, then closed), and it too
    /// writes only what differs.
    pub fn reload_layout(&mut self) {
        let layout = Layout::current();
        if layout == self.layout {
            return;
        }
        let keyboard = self.keyboard.clone();
        match keyboard.set_xkb_config(self, layout.xkb()) {
            Ok(()) => info!("keyboard layout {layout}"),
            Err(e) => warn!("keyboard layout {layout}: {e:?}; keeping the previous one"),
        }
        self.layout = layout;
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        syncobj::watch(surface);
        crate::timing::watch(surface);
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
        if let Some(gpu) = &self.gpu {
            let replaced = syncobj::applied(gpu.renderer_ref(), surface);
            let submission = gpu.renderer_ref().submitted();
            self.holds.add(submission, replaced);
        }
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

impl DrmSyncobjHandler for State {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj.as_mut()
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
        // Imported once, when the client makes it, and kept with it (Smithay's
        // anvil; Weston imports at creation too): its first frame finds it.
        let imported = match self.gpu.as_mut() {
            Some(gpu) => gpu.renderer().import_dmabuf(&dmabuf, None).map(drop),
            None => Err(error("no GPU")),
        };
        match imported {
            Ok(()) => {
                let _ = notifier.successful::<State>();
            }
            // Said, or the client's protocol error is all that shows (0.5.105).
            Err(e) => {
                warn!("a client's dmabuf cannot be shown: {e}");
                notifier.failed();
            }
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

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.inhibitors.push(surface);
    }

    /// Destroyed by its client: shown until now, if it was. Inhibitors of
    /// a client that ends go without this (`look_at_inhibitors`).
    fn uninhibit(&mut self, surface: WlSurface) {
        self.look_at_inhibitors();
        if let Some(i) = self.inhibitors.iter().position(|s| *s == surface) {
            self.inhibitors.swap_remove(i);
        }
    }
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_drm_syncobj!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_cursor_shape!(State);
delegate_presentation!(State);
delegate_fifo!(State);
delegate_idle_inhibit!(State);
delegate_commit_timing!(State);
delegate_viewporter!(State);

/// cursor-shape-v1 also names tablet tools' images; Beize has no tablets
/// yet, so nothing is needed of it.
impl TabletSeatHandler for State {}

/// The panels of a shell that failed, as the last frame drew them, and the
/// room they left for windows (`State::keep_panels`).
struct Kept {
    panels: Vec<scene::PanelFrame>,
    area: Rectangle<i32, Logical>,
    since: Duration,
    timer: RegistrationToken,
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
