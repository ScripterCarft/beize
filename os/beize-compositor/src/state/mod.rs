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
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev::{UdevBackend, UdevEvent};
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states,
};
use smithay::desktop::{
    LayerSurface, PopupGrab, PopupManager, PopupUngrabStrategy, Space, Window,
    find_popup_root_surface, layer_map_for_output,
};
use smithay::input::keyboard::{KeyboardHandle, Keycode, LedState, XkbConfig};
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutputMode, Output as WlOutput, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::channel::{self, Sender};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, LoopSignal, RegistrationToken};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_callback::WlCallback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::utils::{
    Clock, ClockSource, Logical, Monotonic, Physical, Point, Rectangle, SERIAL_COUNTER, Serial,
    Size, Transform,
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

use crate::animation::{Animations, Picture};
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

mod focus;
mod frame;
mod handlers;
mod idle;
mod pictures;
mod session;

/// Key repeat: GNOME's defaults (500 ms, then every 30 ms) until Beize's
/// settings offer a choice.
const REPEAT_DELAY: i32 = 500;
const REPEAT_RATE: i32 = 33;

/// Surfaces not on the screen hear "frame done" at most this often.
const HIDDEN_FRAMES: Duration = Duration::from_secs(1);
/// The longest wait before a frame is tried again (`retry_frame`).
const FAILED_AT_MOST: Duration = Duration::from_secs(1);
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
    /// The keyboards libinput has: their Caps, Num and Scroll Lock lights
    /// follow the keyboard's state (`led_state_changed`), as in niri.
    pub keyboards: Vec<smithay::reexports::input::Device>,
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
    /// Alt+Tab, while it is open (`switcher`).
    pub switcher: Option<crate::switcher::Switcher>,
    /// The overlay with the keyboard (`layers`).
    pub overlay_keyboard: Option<WlSurface>,
    /// The serial of the last key and button event and whose it was: what
    /// entitles a client to a menu that grabs (`shell::grab`).
    pub last_key: Option<(Serial, ClientId)>,
    pub last_button: Option<(Serial, ClientId)>,
    /// Frames that failed in a row (`retry_frame`).
    frames_failed: u32,
    /// Every app's handle on the glass, which hears the appearance (`glass`).
    pub glass_managers:
        Vec<beize_protocols::app::server::beize_glass_manager_v1::BeizeGlassManagerV1>,
    /// Where a dragged window would snap to, shown behind it (`windows`).
    snap_preview: Option<Rectangle<i32, Logical>>,
    snap_preview_id: Id,
    snap_preview_commit: CommitCounter,
    /// What the title bars of other apps' windows share (`decorations`).
    decorations: Decorations,
    /// Light or dark (`backdrop`).
    pub appearance: Appearance,
    /// The background and the glass, drawn for the screen's size and the
    /// appearance (`backdrop_for`, numbered by `backdrops`).
    backdrop: Option<Backdrop>,
    backdrop_for: Option<(Size<i32, Physical>, Appearance)>,
    backdrop_retry: Option<Duration>,
    backdrops: u64,
    /// What lies behind live glass, per panel that has it.
    live_glass: Vec<LiveGlass>,
    /// The desktop's start (`opening`): when the screen was taken, the
    /// wait for the shell, the black it fades in from.
    opening: Opening,
    /// Windows opening and closing (ADR 0032).
    pub animations: Animations,
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
    /// When hidden windows hear "frame done" next (`hidden_tick`).
    hidden_timer: Option<RegistrationToken>,
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
        crate::glass::offer(&display);
        crate::app_menu::offer(&display);
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
            keyboards: Vec::new(),
            layout,
            taken_keys: HashSet::new(),
            space: Space::default(),
            unmapped: Vec::new(),
            minimized: Vec::new(),
            shell_windows: crate::shell_v1::ShellWindows::new(&display),
            popups: PopupManager::default(),
            popup_grab: None,
            switcher: None,
            overlay_keyboard: None,
            last_key: None,
            last_button: None,
            frames_failed: 0,
            glass_managers: Vec::new(),
            snap_preview: None,
            snap_preview_id: Id::new(),
            snap_preview_commit: CommitCounter::default(),
            decorations: Decorations::default(),
            appearance: Appearance::chosen(),
            backdrop: None,
            backdrop_for: None,
            backdrop_retry: None,
            backdrops: 0,
            live_glass: Vec::new(),
            opening: Opening::Waiting { shell: false },
            animations: Animations::from_environment(),
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
            hidden_timer: None,
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

    pub fn now(&self) -> Duration {
        self.clock.now().into()
    }

    /// Whether a screen shows what is drawn.
    pub fn shows(&self) -> bool {
        self.gpu.as_ref().is_some_and(Gpu::shows)
    }

    /// The time between two vblanks; without a screen, 16 ms.
    pub fn refresh(&self) -> Duration {
        self.gpu
            .as_ref()
            .and_then(Gpu::refresh)
            .unwrap_or(Duration::from_millis(16))
    }

    /// The input clock now, in milliseconds as input events carry it.
    pub fn time_msec(&self) -> u32 {
        Duration::from(self.clock.now()).as_millis() as u32
    }
}

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

/// Whether `surface` is the shell's.
fn is_shells(surface: Option<&WlSurface>) -> bool {
    surface
        .and_then(|surface| surface.client())
        .and_then(|c| c.get_data::<ClientState>().map(|d| d.shell))
        .unwrap_or(false)
}

/// A popup that grows out of its button had its first content
/// (`grown_committed`).
struct Grown;

/// A window drawn into a picture of its own as it shows (`window_picture`):
/// its app, the picture, its shadow if apart, its buffers kept until the
/// picture is drawn, the windows behind it.
struct WindowShot {
    app: String,
    picture: Picture,
    shadow: Option<crate::corners::Shadow>,
    kept: Vec<syncobj::Held>,
    behind: Vec<Window>,
}
