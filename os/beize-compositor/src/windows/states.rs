//! A window's states: minimized and back, maximized, fullscreen, snapped
//! to a half, arranged after the screen changed.

use super::*;

impl State {
    /// Takes `window` off the screen; the one below it becomes active.
    pub fn minimize(&mut self, window: &Window) {
        let Some(at) = self.space.element_location(window) else {
            return;
        };
        with_place(window, |place| place.minimized_at = Some(at));
        info!("minimized {}", app_id(window));
        // Into its app's symbol (ADR 0032 §5), or gone at once.
        if !self.minimize_picture(window) {
            self.animations.stop(window);
            self.animations.stop_genie(window);
        }
        let active = self.active_window().as_ref() == Some(window);
        self.space.unmap_elem(window);
        self.minimized.push(window.clone());
        if active {
            window.set_activated(false);
            self.activate_next();
        }
        // The pointer may have been over it.
        crate::input::refresh_pointer(self);
    }

    /// `window`, placed, comes out of its app's symbol in the dock – back
    /// from the dock, or `opening` from it (ADR 0032 §5); at once without a
    /// screen or a symbol, and fullscreen. Whether it does.
    pub fn come_out(&mut self, window: &Window, opening: bool) -> bool {
        let app = app_id(window);
        let place = self
            .space
            .element_geometry(window)
            .map(|g| frame(window, g).to_physical(1));
        let icon = self.dock_icon(&app);
        match (place, icon) {
            (Some(place), Some(icon))
                if self.shows()
                    && !fullscreen(window)
                    && crate::genie::Genie::new(place, icon).is_some() =>
            {
                self.animations.come_back(window, app, place, icon, opening);
                self.changed();
                true
            }
            _ => {
                self.animations.stop_genie(window);
                false
            }
        }
    }

    /// Raises `window` and makes it the active one; back on the screen, in
    /// its place, if it was minimized.
    pub fn bring_back(&mut self, window: Window) {
        if let Some(i) = self.minimized.iter().position(|w| *w == window) {
            self.minimized.remove(i);
            let at = with_place(&window, |place| place.minimized_at.take());
            let area = self.window_area();
            let size = window.geometry().size;
            let half = window.toplevel().and_then(snapped);
            let at = if fullscreen(&window) {
                self.screen_area().loc
            } else if let Some(half) = half {
                half_place(area, half, size, bar_height(&window))
            } else {
                at.map_or(area.loc, |at| constrain_content(area, &window, size, at))
            };
            self.space.map_element(window.clone(), at, false);
            info!("brought back {}", app_id(&window));
            self.come_out(&window, false);
        }
        self.focus(Some(window));
        crate::input::refresh_pointer(self);
    }

    pub fn maximize(&mut self, toplevel: &ToplevelSurface) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        let area = self.window_area();
        let fullscreen =
            toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen));
        // From a half: the floating place it remembers already.
        let half = snap_pending(toplevel).is_some() || snapped(toplevel).is_some();
        if !toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized)
        {
            let geometry = self.space.element_geometry(&window);
            with_place(&window, |place| {
                // Fullscreen: the place from before that.
                if fullscreen {
                    place.restore = place.before_fullscreen;
                } else if !half {
                    place.restore = geometry;
                }
                place.unmaximized_at = None;
            });
        }
        let bar = bar_height(&window);
        toplevel.with_pending_state(|state| {
            set_tiled(state, None);
            state.states.set(XdgState::Maximized);
            // Fullscreen keeps the screen; maximized shows when it ends.
            if !fullscreen {
                state.size = Some((area.size.w, area.size.h - bar).into());
            }
        });
        self.reshape(&window, toplevel);
    }

    /// Sends `window` what it was asked to become; its frame springs there
    /// from where it is with the commit that has it (ADR 0032 §5).
    pub(super) fn reshape(&mut self, window: &Window, toplevel: &ToplevelSurface) {
        if toplevel.is_initial_configure_sent()
            && let Some(asked) = toplevel.send_pending_configure()
        {
            self.reshape_picture(window, asked);
        }
    }

    /// Fullscreen, or back (Super+F, Alt on the green button).
    pub fn toggle_fullscreen(&mut self, toplevel: &ToplevelSurface) {
        if toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen)) {
            self.unfullscreen(toplevel);
        } else {
            self.fullscreen(toplevel);
        }
    }

    /// The whole screen for the window, over the panels' strips; it comes
    /// to the front.
    pub fn fullscreen(&mut self, toplevel: &ToplevelSurface) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        if toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen)) {
            return;
        }
        // Fullscreen comes at once (ADR 0032 §5).
        self.animations.stop_reshape(&window);
        let geometry = self.space.element_geometry(&window);
        with_place(&window, |place| {
            place.before_fullscreen = geometry;
            place.unmaximized_at = None;
        });
        let screen = self.screen_area();
        toplevel.with_pending_state(|state| {
            state.states.set(XdgState::Fullscreen);
            state.size = Some(screen.size);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        info!("fullscreen {}", app_id(&window));
        // Only the active one comes in front: asking for the screen is no way
        // to the keyboard (and the clipboard, which goes with it), nor past
        // a dialog or menu of Beize's holding it (ADR 0030).
        let active = self.active_window().as_ref() == Some(&window);
        if active && self.space.element_location(&window).is_some() {
            if self.beize_holds_keyboard() {
                self.space.raise_element(&window, true);
                self.changed();
            } else {
                self.focus(Some(window));
            }
        }
    }

    /// Back to where the window was before, or maximized if it was so.
    pub fn unfullscreen(&mut self, toplevel: &ToplevelSurface) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        if !toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen)) {
            return;
        }
        let area = self.window_area();
        let bar = bar_height(&window);
        let maximized =
            toplevel.with_pending_state(|state| state.states.contains(XdgState::Maximized));
        // On a half it goes to the half as it is now (`window_committed`).
        let half = snap_pending(toplevel);
        let before = with_place(&window, |place| {
            let before = place.before_fullscreen.take();
            if !maximized && half.is_none() {
                place.unmaximized_at = before.map(|r| r.loc);
            }
            before
        });
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgState::Fullscreen);
            state.size = if maximized {
                Some((area.size.w, area.size.h - bar).into())
            } else if let Some(half) = half {
                let frame = half_frame(area, half);
                Some((frame.size.w, frame.size.h - bar).into())
            } else {
                // Without a size from before, the app chooses.
                before.map(|r| r.size)
            };
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        info!("left fullscreen {}", app_id(&window));
    }

    /// The window fills `half` of the window area, with a commit of the
    /// half's size (`window_committed`). From floating it remembers `from`,
    /// else where it is; from maximized or the other half what it remembers.
    pub fn snap(
        &mut self,
        toplevel: &ToplevelSurface,
        half: Half,
        from: Option<Rectangle<i32, Logical>>,
    ) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        let (fullscreen, maximized) = toplevel.with_pending_state(|state| {
            (
                state.states.contains(XdgState::Fullscreen),
                state.states.contains(XdgState::Maximized),
            )
        });
        let before = snap_pending(toplevel);
        if fullscreen || before == Some(half) {
            return;
        }
        let floating = from.or_else(|| self.space.element_geometry(&window));
        with_place(&window, |place| {
            if !maximized && before.is_none() {
                place.restore = floating;
            }
            place.unmaximized_at = None;
        });
        let area = self.window_area();
        let bar = bar_height(&window);
        let frame = half_frame(area, half);
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgState::Maximized);
            set_tiled(state, Some(half));
            state.size = Some((frame.size.w, frame.size.h - bar).into());
        });
        self.reshape(&window, toplevel);
        info!("snapped {} {half:?}", app_id(&window));
    }

    /// The window with the keyboard's state (Alt+F4, Super+arrows): the
    /// topmost active one.
    pub fn active_window(&self) -> Option<Window> {
        self.space
            .elements()
            .rev()
            .find(|w| {
                w.toplevel().is_some_and(|t| {
                    t.with_pending_state(|s| s.states.contains(XdgState::Activated))
                })
            })
            .cloned()
    }

    /// Super+arrow on the active window (ADR 0028 §7): left and right take
    /// that half, from anywhere; up maximizes; only down goes back, as the
    /// Window menu names them. Fullscreen stays.
    pub fn arrow_key(&mut self, arrow: Arrow) {
        // A drag decides where the window goes.
        if self.seat.get_pointer().is_some_and(|p| p.is_grabbed()) {
            return;
        }
        let Some(window) = self.active_window() else {
            return;
        };
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        if toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen)) {
            return;
        }
        match arrow {
            Arrow::Left => self.snap(&toplevel, Half::Left, None),
            Arrow::Right => self.snap(&toplevel, Half::Right, None),
            Arrow::Up => self.maximize(&toplevel),
            Arrow::Down => self.unmaximize(&toplevel, None),
        }
    }

    /// Back to where the window was before it was maximized or took a half,
    /// or `at` – the pointer's place when a drag takes it off that place.
    pub fn unmaximize(&mut self, toplevel: &ToplevelSurface, at: Option<Point<i32, Logical>>) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        if !toplevel.with_pending_state(|state| {
            state.states.contains(XdgState::Maximized) || half_in(state).is_some()
        }) {
            return;
        }
        if toplevel.with_pending_state(|state| state.states.contains(XdgState::Fullscreen)) {
            // It leaves fullscreen to its place from before, not maximized.
            with_place(&window, |place| {
                if let Some(restore) = place.restore.take() {
                    place.before_fullscreen = Some(restore);
                }
            });
            toplevel.with_pending_state(|state| {
                state.states.unset(XdgState::Maximized);
                set_tiled(state, None);
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            return;
        }
        let restore = with_place(&window, |place| {
            let restore = place.restore.take();
            place.unmaximized_at = at.or(restore.map(|r| r.loc));
            restore
        });
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgState::Maximized);
            set_tiled(state, None);
            // Without a size from before, the app chooses.
            state.size = restore.map(|r| r.size);
        });
        // Taken off by a drag, it follows the pointer as it is (ADR 0032
        // §2).
        if at.is_none() {
            self.reshape(&window, toplevel);
        } else if toplevel.is_initial_configure_sent() {
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
            let (maximized, fullscreen) = toplevel.with_pending_state(|state| {
                (
                    state.states.contains(XdgState::Maximized),
                    state.states.contains(XdgState::Fullscreen),
                )
            });
            let half = snap_pending(toplevel);
            if let (Some(half), false, false) = (half, fullscreen, maximized) {
                let frame = half_frame(area, half);
                toplevel.with_pending_state(|state| {
                    state.size = Some((frame.size.w, frame.size.h - bar).into());
                });
                // A new size brings its place with the commit; the same size
                // in a new area only moves.
                if let Some(geometry) = self.space.element_geometry(&window)
                    && snapped(toplevel) == Some(half)
                {
                    let at = half_place(area, half, geometry.size, bar);
                    if at != geometry.loc {
                        self.space.map_element(window.clone(), at, false);
                    }
                }
            } else if fullscreen {
                // The whole screen, over the panels' strips: new with a new
                // mode (ADR 0029 §4).
                let screen = self.screen_area().size;
                toplevel.with_pending_state(|state| state.size = Some(screen));
            } else if maximized {
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
            let bar = bar_height(window);
            let content = Size::from((area.size.w, area.size.h - bar));
            toplevel.with_pending_state(|state| {
                state.bounds = Some(content);
                if state.states.contains(XdgState::Fullscreen) {
                    return;
                }
                if state.states.contains(XdgState::Maximized) {
                    state.size = Some(content);
                } else if let Some(half) = half_in(state) {
                    let frame = half_frame(area, half);
                    state.size = Some((frame.size.w, frame.size.h - bar).into());
                }
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        self.changed();
    }
}
