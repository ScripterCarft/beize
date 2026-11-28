//! What is under the pointer, keyboard focus, and popup grabs.

use super::*;

impl State {
    /// The topmost surface under `at` – a panel above the windows, a
    /// window, a panel below them – and that surface's position.
    pub fn surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let panels = |layers: &[Layer]| {
            self.output
                .as_ref()
                .and_then(|output| crate::layers::surface_under(output, layers, at, |_, _| true))
        };
        self.panel_surface_under(at)
            .or_else(|| self.window_surface_under(at))
            .or_else(|| panels(&[Layer::Bottom, Layer::Background]))
    }

    /// The surface of a panel above the windows under `at`, if it shows.
    pub(super) fn panel_surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.output.as_ref()?;
        crate::layers::surface_under(output, &[Layer::Overlay, Layer::Top], at, |layer, place| {
            layer == Layer::Overlay || self.panel_shown(place)
        })
    }

    /// Whether a panel above the windows (the menu bar, the dock) is under
    /// `at`: a click there is the shell's, not the window's behind it.
    pub fn panel_under(&self, at: Point<f64, Logical>) -> bool {
        self.panel_surface_under(at).is_some()
    }

    /// The topmost window under `at`, its surface there and that surface's
    /// position; none on its title bar, which is the compositor's.
    pub(super) fn window_surface_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, Hit::Content) = self.window_under(at)? else {
            return None;
        };
        let location = self.space.element_location(&window)? - window.geometry().loc;
        window
            .surface_under(
                at - location.to_f64(),
                smithay::desktop::WindowSurfaceType::ALL,
            )
            .map(|(surface, offset)| (surface, (offset + location).to_f64()))
    }

    /// After events: once a menu's grab ended – a click elsewhere, a choice,
    /// Escape – the keyboard goes back to the active window, as niri gives
    /// it back. Smithay hands it to the grab's root, which for the shell's
    /// menus is a panel that takes no keys.
    pub fn popup_grab_ended(&mut self) {
        if self.popup_grab.is_none() {
            return;
        }
        // A grab ends once its popups are known gone (Smithay's `has_ended`),
        // which `cleanup` finds out – cheap, so not left to the next frame.
        self.popups.cleanup();
        if !self
            .popup_grab
            .as_ref()
            .is_some_and(|grab| grab.has_ended())
        {
            return;
        }
        let Some(grab) = self.popup_grab.take() else {
            return;
        };
        self.let_go_of(&grab);
        // An overlay that has the keyboard has it back: opened while the
        // menu's grab held it, or before that menu.
        let active = self.overlay_keyboard.clone().or_else(|| {
            self.space
                .elements()
                .rev()
                .filter_map(Window::toplevel)
                .find(|t| t.with_pending_state(|s| s.states.contains(XdgState::Activated)))
                .map(|t| t.wl_surface().clone())
        });
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, active, SERIAL_COUNTER.next_serial());
    }

    /// Lets go of the pointer and keyboard that a menu's `grab` holds, now:
    /// Smithay's grabs let go only at their next event, and the pointer's
    /// drops it – a motion: the press after it went where the pointer had
    /// been (smoke, 28.10.). As `shell::grab` lets go of a menu closed just
    /// before another opens.
    pub fn let_go_of(&mut self, grab: &PopupGrab<State>) {
        if let Some(pointer) = self.seat.get_pointer()
            && pointer.has_grab(grab.serial())
        {
            let serial = SERIAL_COUNTER.next_serial();
            pointer.unset_grab(self, serial, self.time_msec());
        }
        let keyboard = self.keyboard.clone();
        if keyboard.has_grab(grab.serial()) {
            keyboard.unset_grab(self);
        }
    }

    /// Where a dragged window would snap to (`windows`), shown behind it.
    pub fn set_snap_preview(&mut self, place: Option<Rectangle<i32, Logical>>) {
        if self.snap_preview != place {
            if self.snap_preview.is_none() && place.is_some() {
                let now = self.now();
                self.animations.preview_appears(now);
            }
            self.snap_preview = place;
            self.snap_preview_commit.increment();
            self.changed();
        }
    }

    /// The active window left: the top one takes over. An overlay with the
    /// keyboard keeps it: an app's window ending must not send the next
    /// Return past a dialog to what lies behind it.
    pub fn activate_next(&mut self) {
        let overlay = self.overlay_keyboard.clone();
        let next = self.space.elements().last().cloned();
        self.focus(next);
        if let Some(overlay) = overlay {
            self.give_overlay_keyboard(overlay);
        }
    }

    /// The surface a menu's `grab` started from: a panel of the shell or an
    /// app's window.
    pub(super) fn grab_root(&self, grab: &PopupGrab<State>) -> Option<WlSurface> {
        grab.current_grab()
            .and_then(|menu| self.popups.find_popup(&menu))
            .and_then(|kind| find_popup_root_surface(&kind).ok())
    }

    /// A menu of the shell's holds pointer and keyboard now (`forward`).
    pub fn shell_menu_grabs(&self) -> bool {
        self.popup_grab
            .as_ref()
            .is_some_and(|grab| !grab.has_ended() && is_shells(self.grab_root(grab).as_ref()))
    }

    /// The keyboard moves to `window`, or to nothing: an app's menu grabbing
    /// it ends unless it belongs to that window, or the keys would go to a
    /// menu of a window no longer active, or no longer there, as niri's
    /// `update_keyboard_focus`. A menu of the shell's is no window's: it ends
    /// only when another window becomes active.
    pub(super) fn focus_ends_popup_grab(&mut self, window: Option<&Window>) {
        let Some(mut grab) = self.popup_grab.take() else {
            return;
        };
        let surface = window
            .and_then(Window::toplevel)
            .map(|toplevel| toplevel.wl_surface().clone());
        let root = self.grab_root(&grab);
        let shell = is_shells(root.as_ref());
        let keeps = if shell {
            window.is_none() || self.active_window().as_ref() == window
        } else {
            root.is_some() && root == surface
        };
        if !keeps && !grab.has_ended() {
            grab.ungrab(PopupUngrabStrategy::All);
            self.let_go_of(&grab);
        }
        // It ends once the app has destroyed its menus (`popup_grab_ended`).
        self.popup_grab = Some(grab);
    }

    /// Raises `window`, makes it the active one and gives it the keyboard.
    /// A window still waiting for its first configure gets the state with
    /// it (`shell::committed`): the protocol sends none before the
    /// client's initial commit.
    pub fn focus(&mut self, window: Option<Window>) {
        self.focus_ends_popup_grab(window.as_ref());
        self.overlay_keyboard = None;
        for other in self.space.elements().chain(&self.minimized) {
            if Some(other) != window.as_ref() {
                other.set_activated(false);
            }
        }
        let surface = window.as_ref().and_then(|window| {
            // Raising activates it too (Smithay sets the state itself).
            self.space.raise_element(window, true);
            window.set_activated(true);
            Some(window.toplevel()?.wl_surface().clone())
        });
        // Each window hears what changed for it – which `set_activated`'s
        // answer cannot tell once raising has set it; unchanged ones hear
        // nothing (`send_pending_configure`).
        for toplevel in self
            .space
            .elements()
            .chain(&self.minimized)
            .filter_map(Window::toplevel)
        {
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        }
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        self.refresh_listing();
        self.changed();
    }
}
