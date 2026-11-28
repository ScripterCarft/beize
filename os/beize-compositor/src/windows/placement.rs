//! Where windows go, as pure geometry: the first place, staying in reach
//! below the menu bar, halves and snap zones, edges for resizing (ADR
//! 0028 §2).

use super::*;

pub(super) fn with_place<T>(window: &Window, f: impl FnOnce(&mut Place) -> T) -> T {
    // The map is only touched from the event loop, so never borrowed twice.
    let place = window
        .user_data()
        .get_or_insert(|| RefCell::new(Place::default()));
    f(&mut place.borrow_mut())
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
pub(super) fn constrain_content(
    area: Rectangle<i32, Logical>,
    window: &Window,
    size: Size<i32, Logical>,
    at: Point<i32, Logical>,
) -> Point<i32, Logical> {
    let bar = bar_height(window);
    let lifted = Point::from((at.x, at.y - bar));
    constrain(area, (size.w, size.h + bar).into(), lifted) + Point::from((0, bar))
}

/// Whether `window` is fullscreen, as its last commit acknowledged.
pub fn fullscreen(window: &Window) -> bool {
    window
        .toplevel()
        .is_some_and(|t| t.current_state().states.contains(XdgState::Fullscreen))
}

/// Whether `window` is maximized or fullscreen: it fills its place and does
/// not move or resize by dragging.
pub(super) fn fills(window: &Window) -> bool {
    window.toplevel().is_some_and(|t| {
        let states = t.current_state().states;
        states.contains(XdgState::Maximized) || states.contains(XdgState::Fullscreen)
    })
}

/// The half that xdg `states` say, if any: tiled on three sides.
pub(super) fn half_in(states: &smithay::wayland::shell::xdg::ToplevelState) -> Option<Half> {
    let states = &states.states;
    let tall = states.contains(XdgState::TiledTop) && states.contains(XdgState::TiledBottom);
    match (
        tall,
        states.contains(XdgState::TiledLeft),
        states.contains(XdgState::TiledRight),
    ) {
        (true, true, false) => Some(Half::Left),
        (true, false, true) => Some(Half::Right),
        _ => None,
    }
}

/// The half `toplevel` has acknowledged.
pub(super) fn snapped(toplevel: &ToplevelSurface) -> Option<Half> {
    half_in(&toplevel.current_state())
}

/// The half `toplevel` was last asked to take.
pub(super) fn snap_pending(toplevel: &ToplevelSurface) -> Option<Half> {
    toplevel.with_pending_state(|state| half_in(state))
}

/// Sets or clears the xdg states of a half in `state`.
pub(super) fn set_tiled(
    state: &mut smithay::wayland::shell::xdg::ToplevelState,
    half: Option<Half>,
) {
    for tiled in [
        XdgState::TiledLeft,
        XdgState::TiledRight,
        XdgState::TiledTop,
        XdgState::TiledBottom,
    ] {
        state.states.unset(tiled);
    }
    if let Some(half) = half {
        state.states.set(XdgState::TiledTop);
        state.states.set(XdgState::TiledBottom);
        state.states.set(match half {
            Half::Left => XdgState::TiledLeft,
            Half::Right => XdgState::TiledRight,
        });
    }
}

/// The half of `area` with its title bar and content, `SNAP_MARGIN` from all
/// around: the left gets the smaller part of an odd width.
pub(super) fn half_frame(area: Rectangle<i32, Logical>, half: Half) -> Rectangle<i32, Logical> {
    let (y, h) = (area.loc.y + SNAP_MARGIN, area.size.h - 2 * SNAP_MARGIN);
    let room = area.size.w - 3 * SNAP_MARGIN;
    let left = room / 2;
    match half {
        Half::Left => Rectangle::new((area.loc.x + SNAP_MARGIN, y).into(), (left, h).into()),
        Half::Right => Rectangle::new(
            (area.loc.x + 2 * SNAP_MARGIN + left, y).into(),
            (room - left, h).into(),
        ),
    }
}

/// Where the preview of a maximized window lies: as far from the edges as a
/// half's, though the window will fill the area.
pub(super) fn maximized_preview(area: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (area.loc.x + SNAP_MARGIN, area.loc.y + SNAP_MARGIN).into(),
        (area.size.w - 2 * SNAP_MARGIN, area.size.h - 2 * SNAP_MARGIN).into(),
    )
}

/// Where the content of a window in `half` lies, `size` as committed and
/// below a bar of `bar`: an app that stays wider than the half (its least
/// size) stays on the screen, as the right half keeps its right edge.
pub(super) fn half_place(
    area: Rectangle<i32, Logical>,
    half: Half,
    size: Size<i32, Logical>,
    bar: i32,
) -> Point<i32, Logical> {
    let frame = half_frame(area, half);
    let x = match half {
        Half::Left => frame.loc.x,
        Half::Right => (frame.loc.x + frame.size.w - size.w).max(area.loc.x),
    };
    Point::from((x, frame.loc.y + bar))
}

/// Where a drag lets go of a window with the pointer `at`. Within
/// `SNAP_REACH` of a side, or in the strip the menu bar keeps.
pub(super) fn zone(
    screen: Rectangle<i32, Logical>,
    area: Rectangle<i32, Logical>,
    at: Point<f64, Logical>,
) -> Option<Zone> {
    let screen = screen.to_f64();
    if at.x < screen.loc.x + SNAP_REACH {
        Some(Zone::Half(Half::Left))
    } else if at.x >= screen.loc.x + screen.size.w - SNAP_REACH {
        Some(Zone::Half(Half::Right))
    } else if at.y < f64::from(area.loc.y).max(screen.loc.y + 1.0) {
        Some(Zone::Maximized)
    } else {
        None
    }
}

/// The window's app, for the log.
pub fn app_id(window: &Window) -> String {
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
pub(super) fn has(edges: ResizeEdge, edge: ResizeEdge) -> bool {
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
