//! Moving and resizing by the pointer: the grabs (`MoveGrab`,
//! `ResizeGrab`) and how they start – from a title bar, an edge or the
//! client's request.

use super::*;

impl MoveGrab {
    fn follow(&mut self, state: &mut State, location: Point<f64, Logical>) {
        // Closed or minimized meanwhile, or fullscreen by its app: nothing to
        // move (mapping it again would put a minimized window on the screen).
        if !self.window.alive()
            || state.space.element_location(&self.window).is_none()
            || fullscreen(&self.window)
        {
            self.zone = None;
            state.set_snap_preview(None);
            return;
        }
        let delta = location - self.start.location;
        if self.holds {
            if delta.x.hypot(delta.y) < DRAG_THRESHOLD {
                return;
            }
            self.holds = false;
            self.leave_maximized(state);
        }
        let area = state.window_area();
        self.zone = zone(state.screen_area(), area, location);
        state.set_snap_preview(self.zone.map(|zone| match zone {
            Zone::Half(half) => half_frame(area, half),
            Zone::Maximized => maximized_preview(area),
        }));
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
        state.animations.stop_reshape(&self.window);
        state.unmaximize(&toplevel, Some(self.initial.loc));
    }

    /// The button is up: the window goes where the pointer says, and comes
    /// back from there to where the drag began.
    fn release(&mut self, state: &mut State) {
        let Some(toplevel) = self.window.toplevel().cloned() else {
            return;
        };
        match self.zone.take() {
            Some(Zone::Half(half)) => state.snap(&toplevel, half, Some(self.initial)),
            Some(Zone::Maximized) => {
                state.maximize(&toplevel);
                with_place(&self.window, |place| place.restore = Some(self.initial));
            }
            None => {}
        }
    }
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

    fn release(&mut self, _state: &mut State) {
        let Some(toplevel) = self.window.toplevel() else {
            return;
        };
        with_place(&self.window, |place| {
            if let Some(resize) = &mut place.resize {
                resize.released = Some(Instant::now());
            }
        });
        toplevel.with_pending_state(|state| state.states.unset(XdgState::Resizing));
        toplevel.send_pending_configure();
    }

    /// Whatever ended the grab: the window is not resizing any more.
    fn finish(&mut self) {
        let Some(toplevel) = self.window.toplevel().cloned() else {
            return;
        };
        if toplevel.with_pending_state(|state| state.states.contains(XdgState::Resizing)) {
            with_place(&self.window, |place| {
                if let Some(resize) = &mut place.resize {
                    resize.released = Some(Instant::now());
                }
            });
            toplevel.with_pending_state(|state| state.states.unset(XdgState::Resizing));
            toplevel.send_pending_configure();
        }
    }
}

/// What both grabs share: the pointer's events go to no window meanwhile,
/// and the grab ends with the last button released. `$unset` also when
/// something else ends the grab.
macro_rules! drag_grab {
    ($grab:ty, $follow:expr, $release:expr, $unset:expr) => {
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
                    ($release)(&mut *self, &mut *data);
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

            fn unset(&mut self, data: &mut State) {
                ($unset)(self, data);
            }
        }
    };
}

drag_grab!(
    MoveGrab,
    MoveGrab::follow,
    MoveGrab::release,
    |_: &mut MoveGrab, state: &mut State| state.set_snap_preview(None)
);
drag_grab!(
    ResizeGrab,
    |grab: &mut ResizeGrab, state: &mut State, at| grab.follow(state, at),
    ResizeGrab::release,
    // Released already, it says nothing new; replaced, the window stops
    // resizing with it.
    |grab: &mut ResizeGrab, _: &mut State| grab.finish()
);

/// Whether `toplevel` is maximized or on a half: a drag takes it off.
pub(super) fn holds_its_place(toplevel: &ToplevelSurface) -> bool {
    snapped(toplevel).is_some()
        || toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized)
}

impl State {
    /// The app asks to move its window with the pointer button it holds
    /// (`serial`, its press).
    pub fn start_move(&mut self, toplevel: &ToplevelSurface, serial: Serial) {
        let Some((window, start)) = self.grab_for(toplevel, serial) else {
            return;
        };
        let Some(geometry) = self.space.element_geometry(&window) else {
            return;
        };
        // Fullscreen stays where it is.
        if fullscreen(&window) {
            return;
        }
        // The frame follows the pointer as it is (ADR 0032 §2); on a half
        // or maximized only once a drag takes it off (`leave_maximized`):
        // a press alone, as the first of a double click, ends nothing.
        let holds = holds_its_place(toplevel);
        if !holds {
            self.animations.stop_reshape(&window);
        }
        let grab = MoveGrab {
            start,
            window,
            initial: geometry,
            holds,
            zone: None,
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
        // The frame follows the pointer as it is (ADR 0032 §2); on a half
        // or maximized only once a drag takes it off (`leave_maximized`):
        // a press alone, as the first of a double click, ends nothing.
        let holds = holds_its_place(toplevel);
        if !holds {
            self.animations.stop_reshape(&window);
        }
        let grab = MoveGrab {
            start,
            window,
            initial: geometry,
            holds,
            zone: None,
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
    /// strip, a panel over all. None for a maximized or fullscreen window,
    /// which does not resize.
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
                return (!fills(window)).then(|| (window.clone(), edges));
            }
        }
        None
    }

    pub(super) fn begin_resize(
        &mut self,
        window: Window,
        start: GrabStartData<State>,
        edges: ResizeEdge,
        serial: Serial,
    ) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if fills(&window) {
            return;
        }
        let Some(initial) = self.space.element_geometry(&window) else {
            return;
        };
        self.animations.stop_reshape(&window);
        // Off its half: it floats from here at the size it is dragged to.
        let half = snap_pending(toplevel).is_some();
        with_place(&window, |place| {
            if half {
                place.restore = None;
            }
            place.resize = Some(Resize {
                edges,
                initial,
                released: None,
            });
        });
        toplevel.with_pending_state(|state| {
            set_tiled(state, None);
            state.states.set(XdgState::Resizing);
        });
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
    pub(super) fn grab_for(
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

    pub(super) fn set_grab(&mut self, grab: impl PointerGrab<State> + 'static, serial: Serial) {
        if let Some(pointer) = self.seat.get_pointer() {
            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }
}
