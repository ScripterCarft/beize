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

use crate::clipboard::Clipboard;
use crate::desktop::{Desktop, DesktopWindow, DesktopWindowId, Glass, Glasses, SwitcherNews};
use crate::gpu::Gpu;
use crate::input::{Input, Keyboard, Modifiers, PointerState};
use crate::renderer::Renderer;
use crate::window::{PopupParent, Role, Window};
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
    /// Its last frame is shown: what changed since can be drawn
    /// (`App::ready`).
    pub ready: bool,
}

impl Events {
    fn any(&self) -> bool {
        self.configured || self.closed || self.ready || !self.input.is_empty()
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
/// center. It opens below `anchor` – a rectangle in its parent's
/// coordinates, the button that opens it – with its left edge at the
/// anchor's left, or its right edge at the anchor's right (`align`), in
/// `size`; the compositor keeps it on the screen. Opened by a click, it
/// holds pointer and keyboard until a click elsewhere closes it
/// (`Events::closed`) or the app does (`App::close`).
#[derive(Clone, Copy, Debug)]
pub struct Popup {
    pub anchor: beize_draw::Rect,
    pub size: (u32, u32),
    pub align: Align,
    /// Above the anchor rather than below it (the dock's menus).
    pub above: bool,
}

/// Which edges of a popup and its anchor line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    /// Their centres: a dock tile's menu.
    Centre,
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
                glasses: globals.bind(&qh, 1..=1, ()).ok().map(Glasses::new),
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

    /// Opens an overlay (`Overlay`). Only the shell may, as a panel.
    pub fn overlay(&mut self, overlay: &Overlay<'_>) -> Result<WindowId> {
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        self.add(None, true, |globals, qh, surface, id| {
            let layer_shell: ZwlrLayerShellV1 = globals
                .bind(qh, 4..=4, ())
                .context("the compositor offers no overlays to this program")?;
            let layer = layer_shell.get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Overlay,
                overlay.namespace.into(),
                qh,
                id,
            );
            // Anchored nowhere it is centred; a zone of 0 keeps clear of the
            // panels' strips and takes none of its own.
            layer.set_anchor(Anchor::empty());
            layer.set_size(overlay.size.0, overlay.size.1);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(if overlay.keyboard {
                KeyboardInteractivity::OnDemand
            } else {
                KeyboardInteractivity::None
            });
            Ok(Role::Panel {
                _layer_shell: layer_shell,
                layer,
            })
        })
    }

    /// Opens a popup of the window `parent` (`Popup`), with the click just
    /// handled; it shows once its first `configured` is drawn. Its content
    /// is translucent: glass may lie below it (`set_glass`).
    pub fn popup(&mut self, parent: WindowId, popup: &Popup) -> Result<WindowId> {
        use xdg_positioner::{Anchor, ConstraintAdjustment, Gravity};
        let parent = self
            .windows
            .iter()
            .find(|w| w.id == parent)
            .context("no such window")?
            .popup_parent();
        let seat = self.seat.clone();
        let serial = self.state.serial;
        self.add(None, true, |globals, qh, surface, id| {
            let wm_base: XdgWmBase = globals
                .bind(qh, 1..=6, ())
                .context("the compositor has no xdg_wm_base")?;
            let positioner = wm_base.create_positioner(qh, ());
            let px = |v: u32| i32::try_from(v.max(1)).unwrap_or(i32::MAX);
            positioner.set_size(px(popup.size.0), px(popup.size.1));
            let a = popup.anchor;
            positioner.set_anchor_rect(
                a.x.round() as i32,
                a.y.round() as i32,
                (a.w.round() as i32).max(1),
                (a.h.round() as i32).max(1),
            );
            let (anchor, gravity) = match (popup.align, popup.above) {
                (Align::Left, false) => (Anchor::BottomLeft, Gravity::BottomRight),
                (Align::Right, false) => (Anchor::BottomRight, Gravity::BottomLeft),
                (Align::Left, true) => (Anchor::TopLeft, Gravity::TopRight),
                (Align::Right, true) => (Anchor::TopRight, Gravity::TopLeft),
                (Align::Centre, false) => (Anchor::Bottom, Gravity::Bottom),
                (Align::Centre, true) => (Anchor::Top, Gravity::Top),
            };
            positioner.set_anchor(anchor);
            positioner.set_gravity(gravity);
            positioner.set_constraint_adjustment(
                ConstraintAdjustment::SlideX
                    | ConstraintAdjustment::SlideY
                    | ConstraintAdjustment::FlipY,
            );
            let xdg_surface = wm_base.get_xdg_surface(surface, qh, id);
            let xdg_parent = match &parent {
                PopupParent::Xdg(xdg) => Some(xdg),
                PopupParent::Panel(_) => None,
            };
            let xdg_popup = xdg_surface.get_popup(xdg_parent, &positioner, qh, id);
            if let PopupParent::Panel(layer) = &parent {
                layer.get_popup(&xdg_popup);
            }
            positioner.destroy();
            // Before the first commit, as the protocol wants.
            if let Some(seat) = &seat {
                xdg_popup.grab(seat, serial);
            }
            Ok(Role::Popup {
                _wm_base: wm_base,
                xdg_surface,
                popup: xdg_popup,
            })
        })
    }

    /// A context menu's popup at `at` in the content of the window `parent`
    /// (`Popup`), with the click just handled: its top left corner at the
    /// pointer, kept on the screen.
    pub fn context_popup(
        &mut self,
        parent: WindowId,
        at: (f32, f32),
        size: (u32, u32),
    ) -> Result<WindowId> {
        let below_bar = match (
            self.windows.iter().find(|w| w.id == parent),
            self.state.surface(parent),
        ) {
            (Some(window), Some(surface)) => window.bar_height(surface) as f32,
            _ => 0.0,
        };
        self.popup(
            parent,
            &Popup {
                anchor: beize_draw::Rect {
                    x: at.0,
                    y: at.1 + below_bar,
                    w: 1.0,
                    h: 1.0,
                },
                size,
                align: Align::Left,
                above: false,
            },
        )
    }

    /// Sends what the app asked of the compositor now, not with the next
    /// `dispatch`: a menu closed before the app blocks on something else.
    pub fn flush(&mut self) -> Result<()> {
        self.queue.flush().context("sending to the compositor")?;
        Ok(())
    }

    /// Closes the window `id`: a popup the app is done with (a menu's choice
    /// made), or one the compositor closed (`Events::closed`).
    pub fn close(&mut self, id: WindowId) {
        let Some(i) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let qh = self.queue.handle();
        if let Some(wl) = self.state.surface(id).map(|s| s.wl.clone())
            && let Some(glasses) = &mut self.state.glasses
        {
            glasses.set_glass(id, &wl, None, &qh);
        }
        let window = self.windows.swap_remove(i);
        let gpu = &self.renderer.gpu;
        // SAFETY: nothing records now; after the wait the GPU uses nothing
        // of the window.
        if let Err(e) = unsafe { gpu.shared.device.device_wait_idle() } {
            warn!("waiting for the GPU: {e}");
        }
        window.unmap();
        window.destroy(gpu);
        if let Some(j) = self.state.surfaces.iter().position(|s| s.id == id) {
            self.state.surfaces.swap_remove(j).wl.destroy();
        }
        if self.state.keyboard_on == Some(id) {
            self.state.keyboard_on = None;
        }
        if self.state.pointer_on == Some(id) {
            self.state.pointer_on = None;
        }
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
            next: Next::default(),
            on_its_way: Cell::new(None),
            frames: Cell::new(0),
            shown: Cell::new(0),
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

    /// The system's appearance, as the compositor tells every app, what is
    /// drawn on glass follows it; light until it has.
    pub fn appearance(&self) -> Appearance {
        match (&self.state.desktop, &self.state.glasses) {
            (Some(desktop), _) => desktop.appearance,
            (None, Some(glasses)) => glasses.appearance,
            (None, None) => Appearance::Light,
        }
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

    /// Glass below part of the panel or popup `id` (or none), drawn by the
    /// compositor (ADR 0028 §6): what the scene leaves transparent there
    /// shows it. Applies with its next draw.
    pub fn set_glass(&mut self, id: WindowId, glass: Option<Glass>) {
        let qh = self.queue.handle();
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(glasses) = &mut self.state.glasses {
            glasses.set_glass(id, &wl, glass, &qh);
        }
    }

    /// Where the window `id` takes the pointer: these rectangles (a popup's
    /// tiles, a menu's shape), or all of it (`None`). The rest is as if not
    /// there – a click falls through to what lies below, or closes the
    /// popup. Applies with the next draw.
    pub fn set_input(&mut self, id: WindowId, rects: Option<&[beize_draw::Rect]>) {
        let qh = self.queue.handle();
        let Some(surface) = self.state.surface(id) else {
            return;
        };
        let Some(rects) = rects else {
            surface.wl.set_input_region(None);
            return;
        };
        let region = self.state.compositor.create_region(&qh, ());
        for r in rects {
            region.add(
                r.x.round() as i32,
                r.y.round() as i32,
                r.w.round() as i32,
                r.h.round() as i32,
            );
        }
        surface.wl.set_input_region(Some(&region));
        region.destroy();
    }

    /// Brings a window of the desktop to the front, back on the screen if
    /// it was minimized.
    pub fn activate_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.activate(id);
        }
    }

    /// The switcher (Alt+Tab) while the compositor shows it (`watch_desktop`).
    pub fn desktop_switcher(&self) -> Option<SwitcherNews> {
        self.state.desktop.as_ref()?.switcher.clone()
    }

    /// The pointer moved onto a window's tile in the switcher.
    pub fn switcher_hover(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.switcher_hover(id);
        }
    }

    /// A click on a window's tile in the switcher: it is chosen.
    pub fn switcher_choose(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.switcher_choose(id);
        }
    }

    /// The compositor draws these windows' live pictures on `id`, each in its
    /// rectangle; applies with its next draw.
    pub fn show_thumbnails(
        &mut self,
        id: WindowId,
        pictures: &[(DesktopWindowId, beize_draw::Rect)],
    ) {
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(desktop) = &self.state.desktop {
            desktop.show_thumbnails(&wl, pictures);
        }
    }

    /// Asks a window of the desktop to close, as its own close button does.
    pub fn close_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.close(id);
        }
    }

    /// Switches the system's appearance (the control center; only the
    /// shell, `watch_desktop`): the compositor keeps it and tells the shell
    /// back (`appearance`).
    pub fn choose_appearance(&mut self, appearance: Appearance) {
        if let Some(desktop) = &self.state.desktop {
            desktop.choose_appearance(appearance);
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

    /// Whether `id` takes a frame now: its last one is shown. The app's
    /// clock, as GTK's frame clock and macOS' display link are: an app
    /// draws what changed once its window is ready – never before, as a
    /// frame drawn ahead waits in line behind the one on its way, and
    /// everything after it later still. `dispatch` returns when a window
    /// becomes ready.
    pub fn ready(&self, id: WindowId) -> bool {
        self.state.surface(id).is_some_and(Surface::ready)
    }

    /// How many frames `id` drew, and how many of them the compositor
    /// showed: the difference is on its way, at most one (`ready`).
    pub fn frames(&self, id: WindowId) -> (u32, u32) {
        self.state
            .surface(id)
            .map_or((0, 0), |s| (s.frames.get(), s.shown.get()))
    }

    /// Draws `scene` and shows it with the compositor's next frame; only
    /// when the window is `ready`. The scene covers the whole content
    /// (`size`): what it leaves is undefined.
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
        if !surface.ready() {
            return Err(error("drawn while the window's last frame is on its way"));
        }
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
        self.state.apply_configures();
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

/// A window's frame is shown (`Window::draw_once` asked): the window is
/// ready for the next, with the latest configure.
impl Dispatch<WlCallback, (WindowId, u32)> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        (id, frame): &(WindowId, u32),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let wl_callback::Event::Done { .. } = event else {
            return;
        };
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        surface.shown.set(surface.shown.get().wrapping_add(1));
        if surface.on_its_way.get() == Some(*frame) {
            surface.on_its_way.set(None);
            surface.events.ready = true;
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
        // Acknowledged when the app takes it, before the frame for it is
        // presented (`State::apply_configures`).
        if let xdg_surface::Event::Configure { serial } = event
            && let Some(surface) = state.surface_mut(*id)
        {
            surface.next.ack = Some(Ack::Toplevel(xdg_surface.clone(), serial));
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
                let has = |wanted: xdg_toplevel::State| {
                    states
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|s| u32::from_ne_bytes(*s) == u32::from(wanted))
                };
                // Taken with the xdg_surface's configure that follows.
                surface.next.asked = (width, height);
                surface.next.activated = has(xdg_toplevel::State::Activated);
                surface.next.maximized = has(xdg_toplevel::State::Maximized);
                surface.next.fullscreen = has(xdg_toplevel::State::Fullscreen);
            }
            xdg_toplevel::Event::Close => surface.events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, WindowId> for State {
    fn event(
        state: &mut State,
        _: &XdgPopup,
        event: xdg_popup::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(surface) = state.surface_mut(*id) else {
            return;
        };
        match event {
            // Its size, taken with the xdg_surface's configure that follows;
            // where it lies is the compositor's.
            xdg_popup::Event::Configure { width, height, .. } => {
                surface.next.asked = (width, height);
            }
            // A click elsewhere, or the grab was declined.
            xdg_popup::Event::PopupDone => surface.events.closed = true,
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
                // As for a toplevel (`XdgSurface`): acknowledged when the
                // app takes it.
                let size = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
                surface.next.asked = (size(width), size(height));
                surface.next.ack = Some(Ack::Panel(layer.clone(), serial));
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
delegate_noop!(State: ignore XdgPositioner);
delegate_noop!(State: WpCursorShapeManagerV1);
delegate_noop!(State: WpCursorShapeDeviceV1);
