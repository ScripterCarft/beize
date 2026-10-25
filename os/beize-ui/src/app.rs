//! An app's connection to the compositor (ADR 0027 §9): one wl_display, one
//! GPU device with its glyph atlas, one text system and one seat for all of
//! its windows, as NSApplication holds AppKit's connection to the
//! WindowServer for every NSWindow – the shell's menu bar and dock are two
//! windows of one app. Input goes to the window that has the pointer or the
//! keyboard; `dispatch` hands each window its own events.

use std::ffi::c_void;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use ash::vk;
use beize_vulkan::error::{Context, Result};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::warn;
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
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape as CursorShape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
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
use crate::desktop::{Desktop, DesktopWindow, DesktopWindowId, Glass};
use crate::gpu::Gpu;
use crate::input::{Input, Keyboard, Modifiers, PointerState};
use crate::renderer::Renderer;
use crate::window::{Role, Window};
use beize_draw::titlebar::{Appearance, TitleBar};
use beize_draw::{Scene, Text};

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
}

impl Events {
    fn any(&self) -> bool {
        self.configured || self.closed || !self.input.is_empty()
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
                serial: 0,
                cursor_shapes: globals.bind(&qh, 1..=1, ()).ok(),
                cursor: None,
                entered: 0,
                cursor_shown: None,
            },
            globals,
            seat,
            connection,
            next_id: 0,
        })
    }

    /// Opens a window with Beize UI's title bar; it shows once its first
    /// `configured` is drawn.
    pub fn open(&mut self, title: &str, app_id: &str) -> Result<WindowId> {
        self.add(
            Some(TitleBar::new(title)),
            false,
            |globals, qh, surface, id| {
                let wm_base: XdgWmBase = globals
                    .bind(qh, 1..=6, ())
                    .context("the compositor has no xdg_wm_base")?;
                let xdg_surface = wm_base.get_xdg_surface(surface, qh, id);
                let toplevel = xdg_surface.get_toplevel(qh, id);
                toplevel.set_title(title.into());
                toplevel.set_app_id(app_id.into());
                Ok(Role::Toplevel {
                    _wm_base: wm_base,
                    _xdg_surface: xdg_surface,
                    toplevel,
                })
            },
        )
    }

    /// Opens a panel of Beize's shell. Only the shell may: the compositor
    /// offers the protocol to no other client.
    pub fn panel(&mut self, panel: &Panel<'_>) -> Result<WindowId> {
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        self.add(None, panel.translucent, |globals, qh, surface, id| {
            let layer_shell: ZwlrLayerShellV1 = globals
                .bind(qh, 1..=4, ())
                .context("the compositor offers no panels (wlr-layer-shell) to this program")?;
            let layer = layer_shell.get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Top,
                panel.namespace.into(),
                qh,
                id,
            );
            let along = match panel.edge {
                Edge::Top => Anchor::Top,
                Edge::Bottom => Anchor::Bottom,
            };
            // Anchored left and right with width 0, it is as wide as the
            // screen; otherwise centred.
            let across = if panel.width == 0 {
                Anchor::Left | Anchor::Right
            } else {
                Anchor::empty()
            };
            layer.set_anchor(across | along);
            layer.set_size(panel.width, panel.height);
            let margin = i32::try_from(panel.margin).unwrap_or(i32::MAX);
            match panel.edge {
                Edge::Top => layer.set_margin(margin, 0, 0, 0),
                Edge::Bottom => layer.set_margin(0, 0, margin, 0),
            }
            // The margin comes on top of the zone (the protocol's rule).
            layer.set_exclusive_zone(i32::try_from(panel.height).unwrap_or(i32::MAX));
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            Ok(Role::Panel {
                _layer_shell: layer_shell,
                layer,
            })
        })
    }

    /// A new surface with its `role`, its Vulkan surface, and the first
    /// commit, which asks the compositor for a configure.
    fn add(
        &mut self,
        bar: Option<TitleBar>,
        translucent: bool,
        role: impl FnOnce(&GlobalList, &QueueHandle<State>, &WlSurface, WindowId) -> Result<Role>,
    ) -> Result<WindowId> {
        let id = WindowId(self.next_id);
        self.next_id += 1;
        let qh = self.queue.handle();
        let wl = self.state.compositor.create_surface(&qh, ());
        let role = role(&self.globals, &qh, &wl, id)?;
        wl.commit();
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(self.connection.backend().display_ptr().cast())
            .surface(wl.id().as_ptr().cast());
        // SAFETY: display and surface are libwayland's and live as long as
        // the connection, which outlives the Vulkan surface (`Drop`).
        let vk_surface = unsafe {
            self.renderer
                .gpu
                .instance
                .wayland
                .create_wayland_surface(&info, None)
        }
        .context("a Vulkan surface for the window")?;
        self.windows
            .push(Window::new(id, role, vk_surface, bar, translucent));
        self.state.surfaces.push(Surface {
            id,
            wl,
            asked: (0, 0),
            activated: false,
            maximized: false,
            fullscreen: false,
            events: Events::default(),
        });
        Ok(id)
    }

    /// The window `id`; settings for a window that is not (any more) do
    /// nothing.
    fn window(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// A panel's new size: the compositor configures it, then it is drawn
    /// in it.
    pub fn set_panel_size(&mut self, id: WindowId, size: (u32, u32)) {
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(window) = self.window(id) {
            window.set_panel_size(size);
            wl.commit();
        }
    }

    /// Hears of the desktop's windows from now on: only the shell may
    /// (beize-shell-v1; the compositor offers it to no other client).
    pub fn watch_desktop(&mut self) -> Result<()> {
        if self.state.desktop.is_none() {
            let shell = self
                .globals
                .bind(&self.queue.handle(), 1..=1, ())
                .context("the compositor offers this program no list of windows")?;
            self.state.desktop = Some(Desktop::new(shell));
        }
        Ok(())
    }

    /// The desktop's windows, in the order they opened (`watch_desktop`).
    pub fn desktop_windows(&self) -> Vec<DesktopWindow> {
        self.state
            .desktop
            .as_ref()
            .map(|d| d.windows().cloned().collect())
            .unwrap_or_default()
    }

    /// The system's appearance, as the compositor tells the shell
    /// (`watch_desktop`); light until it has.
    pub fn appearance(&self) -> Appearance {
        self.state
            .desktop
            .as_ref()
            .map_or(Appearance::Light, |d| d.appearance)
    }

    /// How light it is behind the menu bar (`watch_desktop`): light wants
    /// dark ink. The appearance's until the compositor has measured it.
    pub fn menu_bar_backdrop(&self) -> Appearance {
        self.state
            .desktop
            .as_ref()
            .and_then(|d| d.menu_bar_backdrop)
            .unwrap_or_else(|| self.appearance())
    }

    /// Glass below part of the panel `id` (or none), drawn by the
    /// compositor (ADR 0028 §6): what the scene leaves transparent there
    /// shows it. Only the shell (`watch_desktop`); applies with the panel's
    /// next draw.
    pub fn set_glass(&mut self, id: WindowId, glass: Option<Glass>) {
        let qh = self.queue.handle();
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(desktop) = &mut self.state.desktop {
            desktop.set_glass(id, &wl, glass, &qh);
        }
    }

    /// Brings a window of the desktop to the front, back on the screen if
    /// it was minimized.
    pub fn activate_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.activate(id);
        }
    }

    /// Tells the compositor the shell's panels are drawn, so the desktop
    /// appears in one piece (ADR 0028 §6): once, after the first draw of
    /// each (`watch_desktop`).
    pub fn desktop_ready(&mut self) {
        if let Some(desktop) = &self.state.desktop {
            desktop.ready();
        }
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

    /// The window's title, in its title bar and the shell.
    /// The pointer's image over `id`'s content (over the title bar it is
    /// the arrow): the text cursor over text, as AppKit's views name theirs.
    pub fn set_cursor(&mut self, id: WindowId, shape: CursorShape) {
        if let Some(window) = self.windows.iter_mut().find(|w| w.id == id) {
            window.content_cursor = shape;
        }
    }

    pub fn set_title(&mut self, id: WindowId, title: &str) {
        if let Some(window) = self.window(id) {
            window.set_title(title);
        }
    }

    /// The title bar light or dark, to go with the content.
    pub fn set_appearance(&mut self, id: WindowId, appearance: Appearance) {
        if let Some(window) = self.window(id) {
            window.set_appearance(appearance);
        }
    }

    /// The content's size where the compositor leaves the choice to the
    /// app: when the window opens, and when it comes back from being
    /// maximized without a size from before (AppKit's content size).
    pub fn prefer_size(&mut self, id: WindowId, size: (u32, u32)) {
        if let Some(window) = self.window(id) {
            window.preferred = size;
        }
    }

    /// The size the window's next frame is drawn in: the content's, below
    /// the title bar.
    pub fn size(&self, id: WindowId) -> (u32, u32) {
        match (
            self.windows.iter().find(|w| w.id == id),
            self.state.surface(id),
        ) {
            (Some(window), Some(surface)) => window.size(surface),
            _ => (1, 1),
        }
    }

    /// Draws `scene` and shows it with the compositor's next frame. The
    /// scene covers the whole content (`size`): what it leaves is
    /// undefined.
    pub fn draw(&mut self, id: WindowId, scene: &Scene) -> Result<()> {
        let App {
            windows,
            renderer,
            text,
            queue,
            state,
            ..
        } = self;
        let window = windows
            .iter_mut()
            .find(|w| w.id == id)
            .context("no such window")?;
        let surface = state.surface(id).context("no such window")?;
        window.draw(
            scene,
            renderer,
            text,
            surface,
            &state.compositor,
            &queue.handle(),
        )
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
        queue.flush().context("sending to the compositor")?;
        // Mesa's WSI reads the same socket and may have queued events
        // already: then only a look, no waiting.
        let pending = self.state.surfaces.iter().any(|s| s.events.any())
            || self.state.desktop.as_ref().is_some_and(|d| d.changed);
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
            .map(|fd| PollFd::new(fd, PollFlags::IN))
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
            if window.title_bar(events, surface, self.seat.as_ref(), state.serial, alt)
                && !events.configured
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

impl Drop for App {
    fn drop(&mut self) {
        let gpu = &self.renderer.gpu;
        // SAFETY: nothing records any more; after the wait the GPU uses
        // nothing of the windows.
        if let Err(e) = unsafe { gpu.shared.device.device_wait_idle() } {
            warn!("waiting for the GPU: {e}");
        }
        for window in self.windows.drain(..) {
            window.destroy(gpu);
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

impl Dispatch<XdgSurface, WindowId> for State {
    fn event(
        state: &mut State,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            // Acknowledged before the frame for it is presented (the
            // present commits).
            xdg_surface.ack_configure(serial);
            if let Some(surface) = state.surface_mut(*id) {
                surface.events.configured = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, WindowId> for State {
    fn event(
        state: &mut State,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                surface.asked = (width, height);
                let has = |wanted: xdg_toplevel::State| {
                    states
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|s| u32::from_ne_bytes(*s) == u32::from(wanted))
                };
                surface.activated = has(xdg_toplevel::State::Activated);
                surface.maximized = has(xdg_toplevel::State::Maximized);
                surface.fullscreen = has(xdg_toplevel::State::Fullscreen);
            }
            xdg_toplevel::Event::Close => surface.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, WindowId> for State {
    fn event(
        state: &mut State,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
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
                surface.asked = (size(width), size(height));
                surface.events.configured = true;
            }
            // The compositor took the panel away (its screen went).
            zwlr_layer_surface_v1::Event::Closed => surface.events.closed = true,
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
                let on = state.keyboard_on.take();
                if let Some(surface) = on.and_then(|id| state.surface(id)) {
                    let leave = wl_keyboard::Event::Leave {
                        serial: 0,
                        surface: surface.wl.clone(),
                    };
                    let mut input = Vec::new();
                    state.keyboard.event(leave, &mut input);
                    state.deliver(on, input);
                }
            }
            (kept, _) => state.wl_keyboard = kept,
        }
        let pointer = caps.contains(wl_seat::Capability::Pointer);
        match (state.wl_pointer.take(), pointer) {
            (None, true) => {
                let pointer = seat.get_pointer(qh, ());
                state.cursor = state
                    .cursor_shapes
                    .as_ref()
                    .map(|shapes| shapes.get_pointer(&pointer, qh, ()));
                state.wl_pointer = Some(pointer);
            }
            (Some(gone), false) => {
                if let Some(device) = state.cursor.take() {
                    device.destroy();
                }
                gone.release();
            }
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
        if let wl_keyboard::Event::Enter { surface, .. } = &event {
            state.keyboard_on = state.id_of(surface);
        }
        let leave = matches!(event, wl_keyboard::Event::Leave { .. });
        let mut input = Vec::new();
        state.keyboard.event(event, &mut input);
        state.deliver(state.keyboard_on, input);
        if leave {
            state.keyboard_on = None;
        }
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
        if let wl_pointer::Event::Enter {
            surface, serial, ..
        } = &event
        {
            state.pointer_on = state.id_of(surface);
            // The image is the app's to name again (`dispatch`).
            state.entered = *serial;
            state.cursor_shown = None;
        }
        let leave = matches!(event, wl_pointer::Event::Leave { .. });
        let mut input = Vec::new();
        state.pointer.event(event, &mut input);
        state.deliver(state.pointer_on, input);
        if leave {
            state.pointer_on = None;
        }
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlRegion);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: WpCursorShapeManagerV1);
delegate_noop!(State: WpCursorShapeDeviceV1);
