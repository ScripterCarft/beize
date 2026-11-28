//! An app's connection to the compositor (ADR 0027 §9): one wl_display, one
//! GPU device with its glyph atlas, one text system and one seat for all of
//! its windows, as NSApplication holds AppKit's connection to the
//! WindowServer for every NSWindow – the shell's menu bar and dock are two
//! windows of one app. Input goes to the window that has the pointer or the
//! keyboard; `dispatch` hands each window its own events.

use std::cell::Cell;
use std::ffi::c_void;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use ash::vk;
use beize_vulkan::error::{Context, Result, error};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::warn;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape as CursorShape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use beize_protocols::app::client::beize_app_menu_v1::BeizeAppMenuV1;
use beize_protocols::app::client::beize_menu_manager_v1::BeizeMenuManagerV1;

use crate::clipboard::Clipboard;
use crate::desktop::{
    Desktop, DesktopPlace, DesktopWindow, DesktopWindowId, Glass, Glasses, SwitcherNews,
};
use crate::gpu::Gpu;
use crate::input::{Input, Keyboard, Modifiers, PointerState};
use crate::renderer::Renderer;
use crate::window::{PopupParent, Role, Window};
use beize_draw::titlebar::{Appearance, TitleBar};
use beize_draw::{Scene, Text};

mod desktop;
mod surfaces;
mod wayland;

/// Sends what is queued; false if the socket is full, which is no failure: the
/// compositor is busy, and what is queued goes once it has read (poll's OUT in
/// `App::dispatch`, as calloop-wayland-source does). Treated as an error it
/// ended the app with its windows.
fn send_queued<T>(queue: &EventQueue<T>) -> Result<bool> {
    match queue.flush() {
        Ok(()) => Ok(true),
        Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(error(format!("sending to the compositor: {e}"))),
    }
}

/// One of the app's windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(u32);

/// What happened to a window while waiting.
#[derive(Debug, Default, PartialEq)]
pub struct Events {
    /// The compositor asked for a size (the first, or a new one): draw.
    pub configured: bool,
    /// The user closed the window (a panel: the compositor took it away).
    pub closed: bool,
    pub input: Vec<Input>,
    /// The commands of its menus the user chose in the menu bar, by their ids
    /// (`App::set_menus`).
    pub commands: Vec<u32>,
    /// Its last frame is shown: what changed since can be drawn
    /// (`App::ready`).
    pub ready: bool,
}

impl Events {
    fn any(&self) -> bool {
        self.configured
            || self.closed
            || self.ready
            || !self.input.is_empty()
            || !self.commands.is_empty()
    }
}

/// One of the app's own fds that `dispatch` also waits for.
#[derive(Clone, Copy, Debug)]
pub enum Watch<'a> {
    /// Until it can be read (or closed).
    Read(BorrowedFd<'a>),
    /// Until it takes more (a full PTY), as poll(2) says.
    Write(BorrowedFd<'a>),
}

/// What one `dispatch` brought.
#[derive(Debug, Default)]
pub struct Dispatched {
    /// The windows something happened to, with what.
    pub windows: Vec<(WindowId, Events)>,
    /// Which of the app's own fds (`dispatch`'s `also`) are ready or
    /// closed, by their index there.
    pub ready: Vec<usize>,
    /// The desktop's windows changed (`watch_desktop`).
    pub desktop: bool,
}

impl Dispatched {
    /// What happened to `window`; nothing if it is not in the list.
    pub fn take(&mut self, window: WindowId) -> Events {
        match self.windows.iter().position(|(id, _)| *id == window) {
            Some(i) => self.windows.swap_remove(i).1,
            None => Events::default(),
        }
    }
}

/// The edge of the screen a panel lies along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Top,
    Bottom,
}

/// A panel of Beize's shell (wlr-layer-shell, ADR 0028 §1): along an edge
/// of the screen, above the windows, which the compositor keeps out of its
/// strip.
#[derive(Clone, Copy, Debug)]
pub struct Panel<'a> {
    /// Names it in the compositor's log.
    pub namespace: &'a str,
    pub edge: Edge,
    /// 0: across the whole edge; otherwise centred on it.
    pub width: u32,
    pub height: u32,
    /// The space between it and its edge, kept from windows too.
    pub margin: u32,
    /// What the scene leaves transparent (`Scene::clear`) shows what is
    /// below: a panel with round corners.
    pub translucent: bool,
}

/// A window of the shell centred above all (wlr-layer-shell's overlay): a
/// dialog, the window switcher. It takes no room from the windows and shows
/// over a fullscreen one too.
#[derive(Clone, Copy, Debug)]
pub struct Overlay<'a> {
    /// Names it in the compositor's log.
    pub namespace: &'a str,
    pub size: (u32, u32),
    /// It has the keyboard when it opens, and again after a click on it; a
    /// click elsewhere takes it, as with macOS' alerts.
    pub keyboard: bool,
}

/// A popup of one of the app's windows (`App::popup`): a menu, the control
/// center. It opens by `anchor` – a rectangle in its parent's coordinates, the
/// button or row that opens it – below or above it with the edges `align`
/// says, or beside it (`Align::Beside`), in `size`; the compositor keeps it on
/// the screen. It holds pointer and keyboard until a click elsewhere closes it
/// (`Events::closed`) or the app does (`App::close`).
#[derive(Clone, Copy, Debug)]
pub struct Popup {
    pub anchor: beize_draw::Rect,
    pub size: (u32, u32),
    pub align: Align,
    /// Above the anchor rather than below it (the dock's menus).
    pub above: bool,
}

/// How far a menu `Align::Beside` lies over its parent, and above its row: by a
/// menu's padding, so its first row is level with that row.
const BESIDE_OVERLAP: i32 = 8;
const BESIDE_RISE: i32 = 6;

/// Which edges of a popup and its anchor line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    /// Their centres: a dock tile's menu.
    Centre,
    /// Beside the anchor, its first row on the anchor's, a little over the
    /// parent: a menu of a menu's row, as macOS'.
    Beside,
}

/// What the compositor tells of one window.
pub(crate) struct Surface {
    pub(crate) id: WindowId,
    pub(crate) wl: WlSurface,
    /// From the last configure; 0 means "choose".
    pub(crate) asked: (i32, i32),
    /// The window is the active one, maximized, fullscreen (the last
    /// configure).
    pub(crate) activated: bool,
    pub(crate) maximized: bool,
    pub(crate) fullscreen: bool,
    pub(crate) events: Events,
    /// The compositor's latest configure, held while a frame is on its way
    /// (`State::apply_configures`).
    next: Next,
    /// The frame on its way: presented, its wl_surface.frame callback not
    /// come yet. Numbered, so a callback of a frame given up (a present
    /// that failed) does not free the next one.
    pub(crate) on_its_way: Cell<Option<u32>>,
    /// The number the next frame gets: how many were drawn.
    pub(crate) frames: Cell<u32>,
    /// How many the compositor showed (their callbacks came).
    shown: Cell<u32>,
}

impl Surface {
    /// The window takes a frame now: none is on its way (`App::ready`).
    pub(crate) fn ready(&self) -> bool {
        self.on_its_way.get().is_none()
    }
}

/// A configure not yet handed to the app: its size and states, and the
/// serial its acknowledgement names.
#[derive(Default)]
struct Next {
    asked: (i32, i32),
    activated: bool,
    maximized: bool,
    fullscreen: bool,
    ack: Option<Ack>,
}

enum Ack {
    Toplevel(XdgSurface, u32),
    Panel(ZwlrLayerSurfaceV1, u32),
}

pub(crate) struct State {
    pub(crate) compositor: WlCompositor,
    pub(crate) surfaces: Vec<Surface>,
    /// The windows that have the keyboard and the pointer.
    keyboard_on: Option<WindowId>,
    pointer_on: Option<WindowId>,
    wl_keyboard: Option<WlKeyboard>,
    wl_pointer: Option<WlPointer>,
    keyboard: Keyboard,
    pointer: PointerState,
    pub(crate) clipboard: Clipboard,
    /// The desktop's windows, for the shell (`watch_desktop`).
    pub(crate) desktop: Option<Desktop>,
    /// The menus of the app's windows, for the menu bar; none where the
    /// compositor lacks them.
    menu_manager: Option<BeizeMenuManagerV1>,
    app_menus: Vec<(WindowId, BeizeAppMenuV1)>,
    /// Glass below the app's windows; none where the compositor lacks it.
    pub(crate) glasses: Option<Glasses>,
    /// Of the last key press or click: copying names the input it
    /// answers, a title bar's move the press.
    pub(crate) serial: u32,
    /// The pointer's image by name (cursor-shape-v1); none where the
    /// compositor lacks the protocol.
    cursor_shapes: Option<WpCursorShapeManagerV1>,
    cursor: Option<WpCursorShapeDeviceV1>,
    /// The pointer's last enter, which naming its image answers, and the
    /// image named since.
    entered: u32,
    cursor_shown: Option<CursorShape>,
    /// xdg-activation; none where the compositor lacks it.
    activation: Option<XdgActivationV1>,
    /// Tokens made for apps to start (`App::ask_activation_token`), by app
    /// id, not yet taken.
    tokens: Vec<(String, String)>,
}

impl State {
    pub(crate) fn surface(&self, id: WindowId) -> Option<&Surface> {
        self.surfaces.iter().find(|s| s.id == id)
    }

    fn surface_mut(&mut self, id: WindowId) -> Option<&mut Surface> {
        self.surfaces.iter_mut().find(|s| s.id == id)
    }

    fn id_of(&self, wl: &WlSurface) -> Option<WindowId> {
        self.surfaces.iter().find(|s| &s.wl == wl).map(|s| s.id)
    }

    /// Hands each window that takes a frame (`App::ready`) the compositor's
    /// latest configure, as GTK's frame clock takes a new size with the
    /// next frame: only the configure the frame is drawn for is
    /// acknowledged, sizes in between are passed over.
    fn apply_configures(&mut self) {
        for surface in &mut self.surfaces {
            if !surface.ready() {
                continue;
            }
            let Some(ack) = surface.next.ack.take() else {
                continue;
            };
            match ack {
                Ack::Toplevel(xdg_surface, serial) => xdg_surface.ack_configure(serial),
                Ack::Panel(layer, serial) => layer.ack_configure(serial),
            }
            let next = &surface.next;
            surface.asked = next.asked;
            surface.activated = next.activated;
            surface.maximized = next.maximized;
            surface.fullscreen = next.fullscreen;
            surface.events.configured = true;
        }
    }

    /// The user chose `command` of `window`'s menus.
    pub(crate) fn command(&mut self, window: WindowId, command: u32) {
        if let Some(surface) = self.surface_mut(window) {
            surface.events.commands.push(command);
        }
    }

    /// Input for `window`; none for no window.
    fn deliver(&mut self, window: Option<WindowId>, input: Vec<Input>) {
        if let Some(surface) = window.and_then(|id| self.surface_mut(id)) {
            surface.events.input.extend(input);
        }
    }
}

pub struct App {
    /// Before the renderer and the connection: `Drop` destroys their
    /// swapchains and Vulkan surfaces first.
    windows: Vec<Window>,
    renderer: Renderer,
    text: Text,
    queue: EventQueue<State>,
    state: State,
    globals: GlobalList,
    /// None: the compositor has no seat, the windows get no input.
    seat: Option<WlSeat>,
    connection: Connection,
    next_id: u32,
    /// The activation token the app was started with
    /// (`XDG_ACTIVATION_TOKEN`): its first window shows with it, as GTK's
    /// does – from the dock, it grows out of the app's symbol (ADR 0032).
    startup_token: Option<String>,
}

impl App {
    /// Connects to the compositor and readies the GPU and the fonts.
    pub fn connect() -> Result<App> {
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
        let display = connection.backend().display_ptr().cast::<c_void>();
        let renderer = Renderer::new(Gpu::new(Some(display))?)?;
        Ok(App {
            windows: Vec::new(),
            renderer,
            text: Text::new()?,
            queue,
            state: State {
                compositor,
                surfaces: Vec::new(),
                keyboard_on: None,
                pointer_on: None,
                wl_keyboard: None,
                wl_pointer: None,
                keyboard: Keyboard::new(),
                pointer: PointerState::default(),
                clipboard,
                desktop: None,
                menu_manager: globals.bind(&qh, 1..=1, ()).ok(),
                app_menus: Vec::new(),
                glasses: globals.bind(&qh, 1..=1, ()).ok().map(Glasses::new),
                serial: 0,
                cursor_shapes: globals.bind(&qh, 1..=1, ()).ok(),
                cursor: None,
                entered: 0,
                cursor_shown: None,
                activation: globals.bind(&qh, 1..=1, ()).ok(),
                tokens: Vec::new(),
            },
            globals,
            seat,
            connection,
            next_id: 0,
            startup_token: std::env::var("XDG_ACTIVATION_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
        })
    }

    /// Asks the compositor for an activation token to start the app
    /// `app_id` with, for the last input on the window `id` (a click in the
    /// dock); it comes with a later dispatch (`take_activation_tokens`).
    /// False where the compositor makes none: start it without.
    pub fn ask_activation_token(&mut self, id: WindowId, app_id: &str) -> bool {
        let qh = self.queue.handle();
        let (Some(activation), Some(surface)) = (&self.state.activation, self.state.surface(id))
        else {
            return false;
        };
        let token = activation.get_activation_token(&qh, app_id.to_owned());
        if let Some(seat) = &self.seat {
            token.set_serial(self.state.serial, seat);
        }
        token.set_surface(&surface.wl);
        token.set_app_id(app_id.to_owned());
        token.commit();
        true
    }

    /// The tokens made since, each with the app it is for.
    pub fn take_activation_tokens(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.state.tokens)
    }

    /// Sends what the app asked of the compositor now, not with the next
    /// `dispatch`: a menu closed before the app blocks on something else.
    pub fn flush(&mut self) -> Result<()> {
        send_queued(&self.queue)?;
        Ok(())
    }

    /// The system's appearance, as the compositor tells every app, what is
    /// drawn on glass follows it; light until it has.
    pub fn appearance(&self) -> Appearance {
        match (&self.state.desktop, &self.state.glasses) {
            (Some(desktop), _) => desktop.appearance,
            (None, Some(glasses)) => glasses.appearance,
            (None, None) => Appearance::Light,
        }
    }

    /// The menus of the window `id`, in the menu bar while it is active
    /// (macOS' main menu): described anew whole.
    pub fn set_menus(&mut self, id: WindowId, menus: &[crate::menu::AppMenu]) {
        let qh = self.queue.handle();
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        let Some(manager) = self.state.menu_manager.clone() else {
            return;
        };
        let proxy = match self.state.app_menus.iter().find(|(w, _)| *w == id) {
            Some((_, proxy)) => proxy.clone(),
            None => {
                let proxy = manager.get_menu(&wl, &qh, id);
                self.state.app_menus.push((id, proxy.clone()));
                proxy
            }
        };
        crate::desktop::describe_menus(&proxy, menus);
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

    /// The app's text system: the system's fonts, loaded once.
    pub fn text(&mut self) -> &mut Text {
        &mut self.text
    }

    /// The GPU the app draws on.
    pub fn gpu_name(&self) -> &str {
        &self.renderer.gpu.name
    }

    /// Handles what the compositor sent and repeats a held key, waiting
    /// at most `timeout` for either (`None`: until something comes) – or
    /// for one of the app's own fds in `also` (a terminal's PTY): one
    /// thread waits for all.
    pub fn dispatch(
        &mut self,
        timeout: Option<Duration>,
        also: &[Watch<'_>],
    ) -> Result<Dispatched> {
        let queue = &mut self.queue;
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        self.state.apply_configures();
        let sent = send_queued(queue)?;
        // Mesa's WSI reads the same socket and may have queued events
        // already: then only a look, no waiting.
        let pending = self.state.surfaces.iter().any(|s| s.events.any())
            || self.state.desktop.as_ref().is_some_and(|d| d.changed)
            || !self.state.tokens.is_empty();
        let mut timeout = if pending {
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
            .map(|fd| {
                let also_out = if sent {
                    PollFlags::empty()
                } else {
                    PollFlags::OUT
                };
                PollFd::new(fd, PollFlags::IN | also_out)
            })
            .chain(also.iter().map(|watch| match watch {
                Watch::Read(fd) => PollFd::new(fd, PollFlags::IN),
                Watch::Write(fd) => PollFd::new(fd, PollFlags::OUT),
            }))
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
        // Room in the socket alone is nothing to read.
        let incoming = connection.is_some() && !(fds[0].revents() - PollFlags::OUT).is_empty();
        drop(fds);
        if let Some(guard) = guard
            && incoming
        {
            guard.read().context("reading from the compositor")?;
        }
        queue
            .dispatch_pending(&mut self.state)
            .context("dispatching")?;
        self.state.apply_configures();
        let state = &mut self.state;
        if let Some(repeat) = state.keyboard.repeat_due(Instant::now()) {
            state.deliver(state.keyboard_on, vec![repeat]);
        }
        let mut windows: Vec<(WindowId, Events)> = state
            .surfaces
            .iter_mut()
            .map(|s| (s.id, std::mem::take(&mut s.events)))
            .collect();
        // Each window's title bar takes its part.
        let qh = self.queue.handle();
        let alt = state.keyboard.modifiers().alt;
        for (id, events) in &mut windows {
            let (Some(window), Some(surface)) = (
                self.windows.iter_mut().find(|w| w.id == *id),
                state.surfaces.iter().find(|s| s.id == *id),
            ) else {
                continue;
            };
            // A bar that changed while a frame was on its way stays changed
            // until the window is ready.
            if window.title_bar(events, surface, self.seat.as_ref(), state.serial, alt)
                && !events.configured
                && surface.ready()
            {
                window.present(
                    &mut self.renderer,
                    &mut self.text,
                    surface,
                    &state.compositor,
                    &qh,
                )?;
            }
        }
        // The pointer's image for where it is now (`Window::cursor`).
        let shape = state
            .pointer_on
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
            .map(Window::cursor);
        if let (Some(shape), Some(device)) = (shape, &state.cursor)
            && state.cursor_shown != Some(shape)
        {
            device.set_shape(state.entered, shape);
            state.cursor_shown = Some(shape);
        }
        windows.retain(|(_, events)| events.any());
        let desktop = self
            .state
            .desktop
            .as_mut()
            .is_some_and(|d| std::mem::take(&mut d.changed));
        Ok(Dispatched {
            windows,
            ready,
            desktop,
        })
    }
}

/// An app that ends takes its windows down as `close` does, each toplevel
/// before its buffers and its surface, and sends that before the connection
/// goes: the compositor draws a closing window's picture while its buffer is
/// there (ADR 0032 §2). A connection that just drops destroys the surfaces
/// first, and the window goes without its animation, as a crashed app's.
/// smithay-client-toolkit's windows go down in that order too.
impl Drop for App {
    fn drop(&mut self) {
        let ids: Vec<WindowId> = self.windows.iter().map(|w| w.id).collect();
        for id in ids {
            self.close(id);
        }
        if let Err(e) = self.flush() {
            warn!("sending the closed windows: {e}");
        }
    }
}
