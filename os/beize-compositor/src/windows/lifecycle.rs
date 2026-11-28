//! A window's life: mapped with its first commit, its commits, unmapped
//! and destroyed.

use super::*;

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

    /// The window with `surface` as its toplevel, wherever it is: minimized
    /// too.
    pub fn any_window(&self, surface: &WlSurface) -> Option<Window> {
        self.window_for(surface).or_else(|| {
            self.minimized
                .iter()
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                .cloned()
        })
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
            // A game or player may ask for fullscreen before its first frame.
            let at = if fullscreen(&window) {
                self.screen_area().loc
            } else {
                initial_place(area, (size.w, size.h + bar).into(), &taken) + Point::from((0, bar))
            };
            info!(
                "window {} {}x{} at {},{}",
                app_id(&window),
                size.w,
                size.h,
                at.x,
                at.y
            );
            self.space.map_element(window.clone(), at, false);
            // It grows in, out of its app's symbol if the dock started it,
            // but for fullscreen (ADR 0032 §5).
            let grows_out =
                crate::activation::take_from_dock(&window) && self.come_out(&window, true);
            if !grows_out && !fullscreen(&window) && self.shows() {
                self.animations.open(&window, app_id(&window));
            }
            self.list_window(&window);
            self.switcher_window_new(&window);
            let active = self.active_window();
            if self.takes_forward(&window) {
                self.focus(Some(window));
            } else if let Some(active) = active {
                // It went on top by being mapped: the active one stays in front.
                self.space.raise_element(&active, true);
                self.changed();
            } else {
                // Nothing to stay in front: it is the active one, and has the
                // keyboard when Beize lets go of it (`forward`).
                window.set_activated(true);
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_pending_configure();
                }
                self.changed();
            }
            // It may have opened under the pointer.
            crate::input::refresh_pointer(self);
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
            if fullscreen(window) {
                return Some(self.screen_area().loc);
            }
            if maximized {
                return Some(self.window_area().loc + Point::from((0, bar)));
            }
            if let Some(half) = snapped(toplevel) {
                return Some(half_place(self.window_area(), half, size, bar));
            }
            if let Some(at) = place.unmaximized_at.take() {
                return Some(at);
            }
            let resize = place.resize?;
            if let Some(released) = resize.released
                && !resizing
            {
                place.resize = None;
                // How long the window took to settle: a client that
                // drew each size the pointer asked for lags behind it.
                info!(
                    "resized {} to {}x{}, {} ms after the button was released",
                    app_id(window),
                    size.w,
                    size.h,
                    released.elapsed().as_millis()
                );
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
        // Snapped, maximized or back: from this commit its frame springs
        // to its new place (ADR 0032 §5).
        if self.animations.reshaping(window)
            && let Some(geometry) = self.space.element_geometry(window)
        {
            let acked = with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok()?.current_serial)
            });
            let frame = frame(window, geometry).to_physical(1);
            self.animations.placed(window, frame, acked);
        }
    }

    /// A window that is gone, placed, minimized or not yet placed.
    pub fn window_destroyed(&mut self, surface: &WlSurface) {
        let window = self.any_window(surface);
        if let Some(window) = &window {
            self.animations.stop(window);
            self.switcher_window_gone(window);
            self.unlist_window(window);
            self.space.unmap_elem(window);
        }
        let other = |w: &Window| w.toplevel().is_some_and(|t| t.wl_surface() != surface);
        self.unmapped.retain(other);
        self.minimized.retain(other);
        // The pointer may have been over it: what lies there now has it,
        // with its image, without waiting for the pointer to move.
        if window.is_some() {
            crate::input::refresh_pointer(self);
        }
    }

    /// `window` committed no buffer: off the screen and out of the lists
    /// until it maps again; if it was active, the one below takes over.
    pub fn unmap_window(&mut self, window: &Window) {
        let active = self.active_window().as_ref() == Some(window);
        info!("unmapped {}", app_id(window));
        self.animations.stop(window);
        self.switcher_window_gone(window);
        self.unlist_window(window);
        self.space.unmap_elem(window);
        self.minimized.retain(|w| w != window);
        window.set_activated(false);
        self.unmapped.push(window.clone());
        if active {
            self.activate_next();
        }
        crate::input::refresh_pointer(self);
        self.changed();
    }
}
