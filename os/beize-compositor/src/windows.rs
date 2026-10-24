//! Floating windows (ADR 0028 §2): a new window opens in the size its app
//! chooses – within what the panels leave of the screen, which it hears as
//! its bounds – centred there, and a little below and to the right of a
//! window already in that place (macOS' cascading, by the height of a title
//! bar). The newest is on top and active.
//!
//! The app moves and resizes it through the compositor
//! (`xdg_toplevel.move/resize`, from its title bar and edges); a pointer
//! grab then follows the pointer until the button is released. Beize UI
//! draws no frame around its windows, so the compositor takes a narrow
//! strip outside every window as its edges, as macOS' WindowServer does.
//! Windows stay below the menu bar, and enough of each stays on the screen
//! to take hold of it again.
//!
//! Maximized, a window fills the window area; it comes back to where it
//! was. A new place goes with the commit that has the new size, never ahead
//! of it, so no frame shows a window's old content in its new place.
//!
//! Minimized, a window leaves the screen and keeps its place; the shell
//! brings it back there (beize-shell-v1, `shell_v1`; `bring_back`), raised
//! and active.
//! The window below takes over the keyboard.
//!
//! A window with the compositor's title bar (`decorations`) is placed by
//! its frame, the bar and its content: the frame is centred, stays below
//! the menu bar and fills the area maximized; the content keeps its place
//! in the space below the bar.

use std::cell::RefCell;

use smithay::desktop::Window;
use smithay::input::SeatHandler;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::{
    ResizeEdge, State as XdgState,
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Serial, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use tracing::info;

use crate::decorations::{bar_height, frame};
use crate::state::State;

/// How far a new window moves from one in its place: the height of the
/// title bar (os/beize-draw, `titlebar`), so the one below shows its title.
const CASCADE: i32 = crate::decorations::HEIGHT;

/// The strip outside a window that resizes it, as wide as macOS' (about
/// 4 px past the frame).
pub const EDGE: i32 = 5;

/// How much of a window stays on the screen, at least, across and down:
/// enough to take hold of its title bar.
const KEEP_VISIBLE: i32 = 64;

/// No window gets smaller than this by dragging, whatever it allows.
const SMALLEST: (i32, i32) = (64, 32);

/// What the compositor remembers of a window.
#[derive(Debug, Default)]
struct Place {
    /// Where it was before it was maximized.
    restore: Option<Rectangle<i32, Logical>>,
    /// Where it goes once it commits its size without being maximized.
    unmaximized_at: Option<Point<i32, Logical>>,
    /// Being resized: which edges move, and the window before.
    resize: Option<Resize>,
    /// Where it was when it was minimized.
    minimized_at: Option<Point<i32, Logical>>,
}

#[derive(Debug, Clone, Copy)]
struct Resize {
    edges: ResizeEdge,
    initial: Rectangle<i32, Logical>,
    /// The button is up: the last commit for the drag ends it.
    released: bool,
}

fn with_place<T>(window: &Window, f: impl FnOnce(&mut Place) -> T) -> T {
    let data = window.user_data();
    data.insert_if_missing(|| RefCell::new(Place::default()));
    // Inserted just above, so always there; the map is only touched from
    // the event loop, so never borrowed twice.
    match data.get::<RefCell<Place>>() {
        Some(place) => f(&mut place.borrow_mut()),
        None => f(&mut Place::default()),
    }
}

/// Where a new window of `size` opens in `area`: centred, then moved by
/// `CASCADE` while another window has its top left corner there; from the
/// area's top left again where the window would not fit.
pub fn initial_place(
    area: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    taken: &[Point<i32, Logical>],
) -> Point<i32, Logical> {
    let centred = Point::from((
        area.loc.x + ((area.size.w - size.w) / 2).max(0),
        area.loc.y + ((area.size.h - size.h) / 2).max(0),
    ));
    let fits = |at: Point<i32, Logical>| {
        at.x + size.w <= area.loc.x + area.size.w && at.y + size.h <= area.loc.y + area.size.h
    };
    let mut at = centred;
    for _ in 0..=taken.len() {
        if !taken.contains(&at) {
            return at;
        }
        at += Point::from((CASCADE, CASCADE));
        if !fits(at) {
            at = area.loc;
        }
    }
    at
}

/// `at` for a window of `size`, kept where it can be taken hold of: its top
/// in `area` (below the menu bar), at least `KEEP_VISIBLE` of it across.
pub fn constrain(
    area: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    at: Point<i32, Logical>,
) -> Point<i32, Logical> {
    let keep = KEEP_VISIBLE.min(size.w);
    let right = area.loc.x + area.size.w;
    let bottom = area.loc.y + area.size.h;
    Point::from((
        at.x.clamp(area.loc.x + keep - size.w, (right - keep).max(area.loc.x)),
        at.y.clamp(area.loc.y, (bottom - KEEP_VISIBLE).max(area.loc.y)),
    ))
}

/// `constrain` for the content of `window` (`size` at `at`): by its frame,
/// so a title bar above it stays below the menu bar too.
fn constrain_content(
    area: Rectangle<i32, Logical>,
    window: &Window,
    size: Size<i32, Logical>,
    at: Point<i32, Logical>,
) -> Point<i32, Logical> {
    let bar = bar_height(window);
    let lifted = Point::from((at.x, at.y - bar));
    constrain(area, (size.w, size.h + bar).into(), lifted) + Point::from((0, bar))
}

/// The window's app, for the log.
fn app_id(window: &Window) -> String {
    let id = window.toplevel().and_then(|toplevel| {
        with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
        })
    });
    id.unwrap_or_else(|| "(no app id)".into())
}

/// The size a drag from `initial` by `delta` on `edges` asks for, within
/// the window's `min` and `max` (0: no limit) and `SMALLEST`.
pub fn resized(
    initial: Size<i32, Logical>,
    edges: ResizeEdge,
    delta: Point<f64, Logical>,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Size<i32, Logical> {
    let (dx, dy) = (delta.x.round() as i32, delta.y.round() as i32);
    let mut w = initial.w;
    let mut h = initial.h;
    if has(edges, ResizeEdge::Left) {
        w -= dx;
    } else if has(edges, ResizeEdge::Right) {
        w += dx;
    }
    if has(edges, ResizeEdge::Top) {
        h -= dy;
    } else if has(edges, ResizeEdge::Bottom) {
        h += dy;
    }
    let limit = |n: i32, min: i32, max: i32, smallest: i32| {
        let n = n.max(min.max(smallest));
        if max > 0 { n.min(max.max(smallest)) } else { n }
    };
    Size::from((
        limit(w, min.w, max.w, SMALLEST.0),
        limit(h, min.h, max.h, SMALLEST.1),
    ))
}

/// Whether `edges` includes `edge` (the protocol's values are bits: top 1,
/// bottom 2, left 4, right 8).
fn has(edges: ResizeEdge, edge: ResizeEdge) -> bool {
    u32::from(edges) & u32::from(edge) != 0
}

/// The edge strip of a window `geometry` that `at` lies in, outside it by
/// at most `EDGE`; None inside it or farther away.
pub fn edge_at(geometry: Rectangle<i32, Logical>, at: Point<f64, Logical>) -> Option<ResizeEdge> {
    let g = geometry.to_f64();
    let e = f64::from(EDGE);
    let (left, top) = (g.loc.x, g.loc.y);
    let (right, bottom) = (left + g.size.w, top + g.size.h);
    let inside = at.x >= left && at.x < right && at.y >= top && at.y < bottom;
    let near = at.x >= left - e && at.x < right + e && at.y >= top - e && at.y < bottom + e;
    if inside || !near {
        return None;
    }
    let mut bits = 0;
    if at.y < top {
        bits |= u32::from(ResizeEdge::Top);
    } else if at.y >= bottom {
        bits |= u32::from(ResizeEdge::Bottom);
    }
    if at.x < left {
        bits |= u32::from(ResizeEdge::Left);
    } else if at.x >= right {
        bits |= u32::from(ResizeEdge::Right);
    }
    ResizeEdge::try_from(bits).ok()
}

impl State {
    /// A new window, before its first commit: it hears its bounds with the
    /// first configure and is placed once it has content (`committed`).
    pub fn new_window(&mut self, toplevel: ToplevelSurface) {
        let area = self.window_area();
        toplevel.with_pending_state(|state| state.bounds = Some(area.size));
        self.unmapped.push(Window::new_wayland_window(toplevel));
    }

    /// The window with `surface` as its toplevel, placed or not yet.
    pub fn window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .chain(&self.unmapped)
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }

    /// After a window's commit: places a new one that has content, and
    /// moves one whose new size calls for a new place.
    pub fn window_committed(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if let Some(i) = self.unmapped.iter().position(|w| w == window) {
            let size = window.geometry().size;
            if size.w <= 0 || size.h <= 0 {
                return;
            }
            let window = self.unmapped.remove(i);
            let area = self.window_area();
            let taken: Vec<_> = self
                .space
                .elements()
                .filter_map(|w| Some(frame(w, self.space.element_geometry(w)?).loc))
                .collect();
            let bar = bar_height(&window);
            let at =
                initial_place(area, (size.w, size.h + bar).into(), &taken) + Point::from((0, bar));
            info!(
                "window {} {}x{} at {},{}",
                app_id(&window),
                size.w,
                size.h,
                at.x,
                at.y
            );
            self.space.map_element(window.clone(), at, false);
            self.list_window(&window);
            self.focus(Some(window));
            return;
        }
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let maximized = toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized);
        let resizing = toplevel.current_state().states.contains(XdgState::Resizing);
        let size = window.geometry().size;
        let bar = bar_height(window);
        let to = with_place(window, |place| {
            if maximized {
                return Some(self.window_area().loc + Point::from((0, bar)));
            }
            if let Some(at) = place.unmaximized_at.take() {
                return Some(at);
            }
            let resize = place.resize?;
            if resize.released && !resizing {
                place.resize = None;
            }
            // The edges opposite those dragged stay where they were.
            let mut at = location;
            if has(resize.edges, ResizeEdge::Left) {
                at.x = resize.initial.loc.x + resize.initial.size.w - size.w;
            }
            if has(resize.edges, ResizeEdge::Top) {
                at.y = resize.initial.loc.y + resize.initial.size.h - size.h;
            }
            Some(at)
        });
        if let Some(to) = to
            && to != location
        {
            self.space.map_element(window.clone(), to, false);
        }
    }

    /// A window that is gone, placed, minimized or not yet placed.
    pub fn window_destroyed(&mut self, surface: &WlSurface) {
        if let Some(window) = self.window_for(surface) {
            self.unlist_window(&window);
        }
        let other = |w: &Window| w.toplevel().is_some_and(|t| t.wl_surface() != surface);
        self.unmapped.retain(other);
        self.minimized.retain(other);
    }

    /// Takes `window` off the screen; the one below it becomes active.
    pub fn minimize(&mut self, window: &Window) {
        let Some(at) = self.space.element_location(window) else {
            return;
        };
        with_place(window, |place| place.minimized_at = Some(at));
        info!("minimized {}", app_id(window));
        self.space.unmap_elem(window);
        self.minimized.push(window.clone());
        let next = self.space.elements().last().cloned();
        self.focus(next);
        // The pointer may have been over it.
        crate::input::refresh_pointer(self);
    }

    /// Raises `window` and makes it the active one; back on the screen, in
    /// its place, if it was minimized.
    pub fn bring_back(&mut self, window: Window) {
        if let Some(i) = self.minimized.iter().position(|w| *w == window) {
            self.minimized.remove(i);
            let at = with_place(&window, |place| place.minimized_at.take());
            let area = self.window_area();
            let at = at.map_or(area.loc, |at| {
                constrain_content(area, &window, window.geometry().size, at)
            });
            self.space.map_element(window.clone(), at, false);
            info!("brought back {}", app_id(&window));
        }
        self.focus(Some(window));
        crate::input::refresh_pointer(self);
    }

    pub fn maximize(&mut self, toplevel: &ToplevelSurface) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        let area = self.window_area();
        if !toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized)
        {
            let geometry = self.space.element_geometry(&window);
            with_place(&window, |place| {
                place.restore = geometry;
                place.unmaximized_at = None;
            });
        }
        let bar = bar_height(&window);
        toplevel.with_pending_state(|state| {
            state.states.set(XdgState::Maximized);
            state.size = Some((area.size.w, area.size.h - bar).into());
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    /// Back to where the window was before it was maximized, or `at` – the
    /// pointer's place when a drag takes it off the maximized place.
    pub fn unmaximize(&mut self, toplevel: &ToplevelSurface, at: Option<Point<i32, Logical>>) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        if !toplevel.with_pending_state(|state| state.states.contains(XdgState::Maximized)) {
            return;
        }
        let restore = with_place(&window, |place| {
            let restore = place.restore.take();
            place.unmaximized_at = at.or(restore.map(|r| r.loc));
            restore
        });
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgState::Maximized);
            // Without a size from before, the app chooses.
            state.size = restore.map(|r| r.size);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    /// The panels changed the window area: maximized windows fill it again,
    /// the others keep their place where it is still in reach.
    pub fn arrange_windows(&mut self) {
        let area = self.window_area();
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let bar = bar_height(&window);
            let content = Size::from((area.size.w, area.size.h - bar));
            toplevel.with_pending_state(|state| state.bounds = Some(content));
            if toplevel.with_pending_state(|state| state.states.contains(XdgState::Maximized)) {
                toplevel.with_pending_state(|state| state.size = Some(content));
            } else if let Some(geometry) = self.space.element_geometry(&window) {
                let at = constrain_content(area, &window, geometry.size, geometry.loc);
                if at != geometry.loc {
                    self.space.map_element(window.clone(), at, false);
                }
            }
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        for window in &self.unmapped {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| state.bounds = Some(area.size));
            }
        }
        // Minimized ones fit the area when they come back (`bring_back`).
        for window in &self.minimized {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let content = Size::from((area.size.w, area.size.h - bar_height(window)));
            toplevel.with_pending_state(|state| {
                state.bounds = Some(content);
                if state.states.contains(XdgState::Maximized) {
                    state.size = Some(content);
                }
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        self.changed();
    }

    /// The app asks to move its window with the pointer button it holds
    /// (`serial`, its press).
    pub fn start_move(&mut self, toplevel: &ToplevelSurface, serial: Serial) {
        let Some((window, start)) = self.grab_for(toplevel, serial) else {
            return;
        };
        let Some(geometry) = self.space.element_geometry(&window) else {
            return;
        };
        let maximized = toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized);
        let grab = MoveGrab {
            start,
            window,
            initial: geometry,
            maximized,
        };
        self.set_grab(grab, serial);
    }

    /// The app asks to resize its window from `edges`, with the pointer
    /// button it holds (`serial`).
    pub fn start_resize(&mut self, toplevel: &ToplevelSurface, serial: Serial, edges: ResizeEdge) {
        let Some((window, start)) = self.grab_for(toplevel, serial) else {
            return;
        };
        self.begin_resize(window, start, edges, serial);
    }

    /// A press on the compositor's title bar of `window` (`decorations`):
    /// it moves with the pointer, as an app's own bar moves it.
    pub fn move_from_bar(&mut self, window: Window, start: GrabStartData<State>, serial: Serial) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let Some(geometry) = self.space.element_geometry(&window) else {
            return;
        };
        let maximized = toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized);
        let grab = MoveGrab {
            start,
            window,
            initial: geometry,
            maximized,
        };
        self.set_grab(grab, serial);
    }

    /// A press on a window's edge strip (`edge_at`): the compositor's own
    /// resize, for windows without a frame of their own.
    pub fn resize_from_edge(&mut self, button: u32, serial: Serial) -> bool {
        let at = self.pointer;
        let Some((window, edges)) = self.edge_under(at) else {
            return false;
        };
        self.focus(Some(window.clone()));
        let start = GrabStartData {
            focus: None,
            button,
            location: at,
        };
        self.begin_resize(window, start, edges, serial);
        true
    }

    /// The window whose edge strip lies under `at`, and which edges. Front
    /// to back, as macOS decides: the strip of a window in front wins over
    /// the windows behind it, a window's content or bar in front over the
    /// strip, a panel over all. None for a maximized window, which does
    /// not resize.
    pub fn edge_under(&self, at: Point<f64, Logical>) -> Option<(Window, ResizeEdge)> {
        if self.panel_under(at) {
            return None;
        }
        for window in self.space.elements().rev() {
            if self.hit(window, at).is_some() {
                return None;
            }
            let Some(geometry) = self.space.element_geometry(window) else {
                continue;
            };
            if let Some(edges) = edge_at(frame(window, geometry), at) {
                let maximized = window
                    .toplevel()
                    .is_some_and(|t| t.current_state().states.contains(XdgState::Maximized));
                return (!maximized).then(|| (window.clone(), edges));
            }
        }
        None
    }

    fn begin_resize(
        &mut self,
        window: Window,
        start: GrabStartData<State>,
        edges: ResizeEdge,
        serial: Serial,
    ) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized)
        {
            return;
        }
        let Some(initial) = self.space.element_geometry(&window) else {
            return;
        };
        with_place(&window, |place| {
            place.resize = Some(Resize {
                edges,
                initial,
                released: false,
            });
        });
        toplevel.with_pending_state(|state| state.states.set(XdgState::Resizing));
        toplevel.send_pending_configure();
        let grab = ResizeGrab {
            start,
            window,
            edges,
            initial,
        };
        self.set_grab(grab, serial);
    }

    /// The window and the press that a move or resize asked for, if the
    /// press is the one still held on that window – an app cannot start a
    /// drag for a button it does not hold, nor on another's window.
    fn grab_for(
        &self,
        toplevel: &ToplevelSurface,
        serial: Serial,
    ) -> Option<(Window, GrabStartData<State>)> {
        let pointer = self.seat.get_pointer()?;
        if !pointer.has_grab(serial) {
            return None;
        }
        let start = pointer.grab_start_data()?;
        let (focus, _) = start.focus.as_ref()?;
        if focus.id().same_client_as(&toplevel.wl_surface().id()) {
            let window = self.window_for(toplevel.wl_surface())?;
            self.space.element_location(&window)?;
            Some((window, start))
        } else {
            None
        }
    }

    fn set_grab(&mut self, grab: impl PointerGrab<State> + 'static, serial: Serial) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }
}

/// Moves a window with the pointer until its button is released.
struct MoveGrab {
    start: GrabStartData<State>,
    window: Window,
    initial: Rectangle<i32, Logical>,
    /// The window is maximized and stays so until the pointer has moved
    /// `DRAG_THRESHOLD`: a click (the first of a double click) moves nothing.
    maximized: bool,
}

/// How far the pointer moves before a press on a maximized window's title
/// bar becomes a drag (Windows' SM_CXDRAG).
const DRAG_THRESHOLD: f64 = 4.0;

impl MoveGrab {
    fn follow(&mut self, state: &mut State, location: Point<f64, Logical>) {
        let delta = location - self.start.location;
        if self.maximized {
            if delta.x.hypot(delta.y) < DRAG_THRESHOLD {
                return;
            }
            self.maximized = false;
            self.leave_maximized(state);
        }
        let at = self.initial.loc.to_f64() + delta;
        let at = constrain_content(
            state.window_area(),
            &self.window,
            self.initial.size,
            (at.x.round() as i32, at.y.round() as i32).into(),
        );
        // Still waiting for the size from before it was maximized: the new
        // place goes with that commit.
        let waiting = with_place(&self.window, |place| {
            place.unmaximized_at.as_mut().map(|to| *to = at).is_some()
        });
        if !waiting {
            state.space.map_element(self.window.clone(), at, false);
        }
        state.changed();
    }

    /// Off the maximized place: the restored window under the pointer, as
    /// far across it as the pointer was across the maximized one (Windows,
    /// GNOME); its top where it was.
    fn leave_maximized(&mut self, state: &mut State) {
        let Some(toplevel) = self.window.toplevel().cloned() else {
            return;
        };
        let restored = with_place(&self.window, |place| place.restore.map(|r| r.size));
        if let Some(size) = restored {
            let x = self.start.location.x;
            let across =
                (x - f64::from(self.initial.loc.x)) / f64::from(self.initial.size.w.max(1));
            let left = x - across * f64::from(size.w);
            self.initial = Rectangle::new((left.round() as i32, self.initial.loc.y).into(), size);
        }
        state.unmaximize(&toplevel, Some(self.initial.loc));
    }
}

/// Resizes a window with the pointer until its button is released.
struct ResizeGrab {
    start: GrabStartData<State>,
    window: Window,
    edges: ResizeEdge,
    initial: Rectangle<i32, Logical>,
}

impl ResizeGrab {
    fn follow(&self, state: &State, location: Point<f64, Logical>) {
        let Some(toplevel) = self.window.toplevel() else {
            return;
        };
        let (min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let mut size = resized(
            self.initial.size,
            self.edges,
            location - self.start.location,
            min,
            max,
        );
        // The top edge (or the bar above it) stops at the menu bar.
        if has(self.edges, ResizeEdge::Top) {
            let area = state.window_area();
            let bottom = self.initial.loc.y + self.initial.size.h;
            size.h = size.h.min(bottom - area.loc.y - bar_height(&self.window));
        }
        toplevel.with_pending_state(|state| state.size = Some(size));
        toplevel.send_pending_configure();
    }

    fn release(&self) {
        let Some(toplevel) = self.window.toplevel() else {
            return;
        };
        with_place(&self.window, |place| {
            if let Some(resize) = &mut place.resize {
                resize.released = true;
            }
        });
        toplevel.with_pending_state(|state| state.states.unset(XdgState::Resizing));
        toplevel.send_pending_configure();
    }
}

/// What both grabs share: the pointer's events go to no window meanwhile,
/// and the grab ends with the last button released.
macro_rules! drag_grab {
    ($grab:ty, $follow:expr, $release:expr) => {
        impl PointerGrab<State> for $grab {
            fn motion(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
                event: &MotionEvent,
            ) {
                handle.motion(data, None, event);
                ($follow)(self, data, event.location);
            }

            fn relative_motion(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
                event: &RelativeMotionEvent,
            ) {
                handle.relative_motion(data, None, event);
            }

            fn button(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &ButtonEvent,
            ) {
                handle.button(data, event);
                if handle.current_pressed().is_empty() {
                    ($release)(&*self);
                    handle.unset_grab(self, data, event.serial, event.time, true);
                }
            }

            fn axis(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                details: AxisFrame,
            ) {
                handle.axis(data, details);
            }

            fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
                handle.frame(data);
            }

            fn gesture_swipe_begin(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GestureSwipeBeginEvent,
            ) {
                handle.gesture_swipe_begin(data, event);
            }

            fn gesture_swipe_update(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GestureSwipeUpdateEvent,
            ) {
                handle.gesture_swipe_update(data, event);
            }

            fn gesture_swipe_end(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GestureSwipeEndEvent,
            ) {
                handle.gesture_swipe_end(data, event);
            }

            fn gesture_pinch_begin(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GesturePinchBeginEvent,
            ) {
                handle.gesture_pinch_begin(data, event);
            }

            fn gesture_pinch_update(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GesturePinchUpdateEvent,
            ) {
                handle.gesture_pinch_update(data, event);
            }

            fn gesture_pinch_end(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GesturePinchEndEvent,
            ) {
                handle.gesture_pinch_end(data, event);
            }

            fn gesture_hold_begin(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GestureHoldBeginEvent,
            ) {
                handle.gesture_hold_begin(data, event);
            }

            fn gesture_hold_end(
                &mut self,
                data: &mut State,
                handle: &mut PointerInnerHandle<'_, State>,
                event: &GestureHoldEndEvent,
            ) {
                handle.gesture_hold_end(data, event);
            }

            fn start_data(&self) -> &GrabStartData<State> {
                &self.start
            }

            fn unset(&mut self, _data: &mut State) {}
        }
    };
}

drag_grab!(MoveGrab, MoveGrab::follow, |_: &MoveGrab| {});
drag_grab!(
    ResizeGrab,
    |grab: &mut ResizeGrab, state: &mut State, at| grab.follow(state, at),
    ResizeGrab::release
);

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 24).into(), (1280, 776).into())
    }

    #[test]
    fn a_new_window_opens_centred_then_cascades() {
        let size = Size::from((640, 480));
        let first = initial_place(area(), size, &[]);
        assert_eq!(first, Point::from((320, 24 + 148)));
        let second = initial_place(area(), size, &[first]);
        assert_eq!(second, first + Point::from((CASCADE, CASCADE)));
        let third = initial_place(area(), size, &[first, second]);
        assert_eq!(third, second + Point::from((CASCADE, CASCADE)));
        // A window larger than the area starts at its top left.
        assert_eq!(initial_place(area(), (2000, 2000).into(), &[]), area().loc);
        // Where the cascade leaves the area, it starts again at the top left.
        let tall = Size::from((640, 770));
        let centred = initial_place(area(), tall, &[]);
        assert_eq!(initial_place(area(), tall, &[centred]), area().loc);
    }

    #[test]
    fn windows_stay_below_the_menu_bar_and_in_reach() {
        let size = Size::from((640, 480));
        assert_eq!(
            constrain(area(), size, (100, 0).into()),
            Point::from((100, 24))
        );
        assert_eq!(
            constrain(area(), size, (-1000, 300).into()),
            Point::from((KEEP_VISIBLE - 640, 300))
        );
        assert_eq!(
            constrain(area(), size, (5000, 5000).into()),
            Point::from((1280 - KEEP_VISIBLE, 800 - KEEP_VISIBLE))
        );
        assert_eq!(
            constrain(area(), size, (10, 50).into()),
            Point::from((10, 50))
        );
    }

    #[test]
    fn a_drag_resizes_within_the_limits() {
        let initial = Size::from((400, 300));
        let none = Size::from((0, 0));
        let corner = ResizeEdge::BottomRight;
        assert_eq!(
            resized(initial, corner, (50.0, -20.0).into(), none, none),
            Size::from((450, 280))
        );
        assert_eq!(
            resized(
                initial,
                ResizeEdge::TopLeft,
                (50.0, 20.0).into(),
                none,
                none
            ),
            Size::from((350, 280))
        );
        assert_eq!(
            resized(
                initial,
                ResizeEdge::Left,
                (-1000.0, 0.0).into(),
                none,
                (800, 0).into()
            ),
            Size::from((800, 300))
        );
        assert_eq!(
            resized(
                initial,
                corner,
                (-1000.0, -1000.0).into(),
                (200, 0).into(),
                none
            ),
            Size::from((200, SMALLEST.1))
        );
    }

    #[test]
    fn the_edge_strip_lies_just_outside_the_window() {
        let window = Rectangle::new((100, 100).into(), (200, 100).into());
        assert_eq!(edge_at(window, (150.0, 150.0).into()), None);
        assert_eq!(edge_at(window, (150.0, 97.0).into()), Some(ResizeEdge::Top));
        assert_eq!(
            edge_at(window, (302.0, 150.0).into()),
            Some(ResizeEdge::Right)
        );
        assert_eq!(
            edge_at(window, (98.0, 201.0).into()),
            Some(ResizeEdge::BottomLeft)
        );
        assert_eq!(edge_at(window, (150.0, 90.0).into()), None);
    }
}
