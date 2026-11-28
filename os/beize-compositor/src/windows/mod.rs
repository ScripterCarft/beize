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
//! Snapped (ADR 0028 §2), a window takes the left or right half of the window
//! area, `SNAP_MARGIN` from its edges and from the other half, as macOS 15's
//! tiled windows have margins by default: dragged with the pointer on the
//! screen's edge, with Super+arrows or from the Window menu. Its only record
//! is the xdg state tiled on three sides, so the place goes with the commit
//! that acknowledges it, as maximized. It keeps corners and shadow, and the
//! place from before as a maximized window does. The pointer in the menu bar's
//! strip during a drag maximizes (macOS 15); a preview behind the window
//! (`Backdrop::snap_preview`, live glass) shows where it will go.
//!
//! Minimized, a window leaves the screen and keeps its place; the shell
//! brings it back there (beize-shell-v1, `shell_v1`; `bring_back`), raised
//! and active.
//! The window below takes over the keyboard.
//!
//! Fullscreen (ADR 0028 §2), a window fills the whole screen, over the
//! panels' strips, without a frame; it comes back to where it was, or to
//! maximized if it was so before. In front it hides the panels
//! (`State::panel_shown`); another window raised over it lets it step back,
//! still fullscreen, until it is raised again.
//!
//! A window with the compositor's title bar (`decorations`) is placed by
//! its frame, the bar and its content: the frame is centred, stays below
//! the menu bar and fills the area maximized; the content keeps its place
//! in the space below the bar.

use std::cell::RefCell;
use std::time::Instant;

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
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Serial, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use tracing::info;

use crate::decorations::{bar_height, frame};
use crate::state::State;

mod grabs;
mod lifecycle;
mod placement;
mod states;

pub use placement::*;

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
    /// Where it was before it went fullscreen.
    before_fullscreen: Option<Rectangle<i32, Logical>>,
    /// Where it goes once it commits its size without being maximized or
    /// fullscreen.
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
    /// When the button went up: the last commit for the drag ends it.
    released: Option<Instant>,
}

/// A half of the window area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    Left,
    Right,
}

/// Where a drag with the pointer at a place would leave the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Half(Half),
    Maximized,
}

/// The arrow key of Super+arrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// The space around a window on a half and between the halves. macOS' margins
/// ("Tiled windows have margins", on by default); Apple publishes no number,
/// so estimated, for now (the owner, 31.10.).
const SNAP_MARGIN: i32 = 10;

/// How near a side of the screen the pointer snaps a dragged window to its
/// half: mutter's, its drag threshold (8) six times
/// (`DRAG_THRESHOLD_TO_SHAKE_THRESHOLD_FACTOR`, meta-window-drag.c). On the
/// outermost pixel only (until 0.5.131), a hand that drifted one pixel back
/// lost the snap (the owner on hardware).
const SNAP_REACH: f64 = 48.0;

/// Moves a window with the pointer until its button is released.
struct MoveGrab {
    start: GrabStartData<State>,
    window: Window,
    initial: Rectangle<i32, Logical>,
    /// The window is maximized or on a half and stays so until the pointer
    /// has moved `DRAG_THRESHOLD`: a click (the first of a double click)
    /// moves nothing.
    holds: bool,
    /// Where it goes when the button is released now.
    zone: Option<Zone>,
}

/// How far the pointer moves before a press on a maximized window's title
/// bar becomes a drag (Windows' SM_CXDRAG).
const DRAG_THRESHOLD: f64 = 4.0;

/// Resizes a window with the pointer until its button is released.
struct ResizeGrab {
    start: GrabStartData<State>,
    window: Window,
    edges: ResizeEdge,
    initial: Rectangle<i32, Logical>,
}

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
    fn halves_fill_the_area_between_them() {
        let area = Rectangle::new((0, 32).into(), (1281, 700).into());
        let left = half_frame(area, Half::Left);
        let right = half_frame(area, Half::Right);
        assert_eq!(left, Rectangle::new((10, 42).into(), (625, 680).into()));
        assert_eq!(right, Rectangle::new((645, 42).into(), (626, 680).into()));
        // The margin to the right edge, as to the left and between them.
        assert_eq!(area.size.w - (right.loc.x + right.size.w), SNAP_MARGIN);
        assert_eq!(right.loc.x - (left.loc.x + left.size.w), SNAP_MARGIN);
        assert_eq!(
            half_place(area, Half::Right, (626, 648).into(), 32),
            Point::from((645, 74))
        );
        // An app that stays wider than the half keeps its right edge there.
        assert_eq!(
            half_place(area, Half::Right, (800, 648).into(), 32),
            Point::from((471, 74))
        );
        assert_eq!(
            half_place(area, Half::Left, (800, 648).into(), 32),
            Point::from((10, 74))
        );
    }

    #[test]
    fn the_zones_lie_on_the_screens_edges_and_in_the_bars_strip() {
        let screen = Rectangle::new((0, 0).into(), (1280, 800).into());
        let area = Rectangle::new((0, 32).into(), (1280, 678).into());
        assert_eq!(
            zone(screen, area, (0.0, 400.0).into()),
            Some(Zone::Half(Half::Left))
        );
        assert_eq!(
            zone(screen, area, (1279.0, 400.0).into()),
            Some(Zone::Half(Half::Right))
        );
        assert_eq!(
            zone(screen, area, (640.0, 31.0).into()),
            Some(Zone::Maximized)
        );
        // A hand that drifted back a little still snaps; beyond the reach not.
        assert_eq!(
            zone(screen, area, (47.0, 400.0).into()),
            Some(Zone::Half(Half::Left))
        );
        assert_eq!(
            zone(screen, area, (1232.0, 400.0).into()),
            Some(Zone::Half(Half::Right))
        );
        assert_eq!(zone(screen, area, (48.0, 400.0).into()), None);
        assert_eq!(zone(screen, area, (1231.0, 400.0).into()), None);
        assert_eq!(zone(screen, area, (640.0, 32.0).into()), None);
        // Without a bar, the top row.
        assert_eq!(
            zone(screen, screen, (640.0, 0.0).into()),
            Some(Zone::Maximized)
        );
        assert_eq!(zone(screen, screen, (640.0, 1.0).into()), None);
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
