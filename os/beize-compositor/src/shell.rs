//! Windows (xdg-shell): floating, placed, moved and resized as `windows`
//! has it (ADR 0028 §2), the newest on top and active, with the keyboard.
//! Popups (menus) open where the client asks, kept on the screen.
//!
//! A menu may grab the pointer and keyboard (`xdg_popup.grab`): it closes
//! on a click elsewhere, which still goes where it was aimed – a window's
//! content (Smithay's `PopupPointerGrab`), its title bar or edge
//! (`State::press_ends_popup_grab`); then the keyboard goes back to the
//! active window (`State::popup_grab_ended`). A minimized window comes back through the
//! shell (`windows`, `shell_v1`).

use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, find_popup_root_surface,
    get_popup_toplevel_coords,
};
use smithay::input::Seat;
use smithay::input::pointer::Focus;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::{
    wl_output::WlOutput, wl_seat, wl_surface::WlSurface,
};
use smithay::utils::Serial;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use tracing::warn;

use crate::state::{ClientState, State};

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.new_window(surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.window_destroyed(surface.wl_surface());
        // The window leaves the space with its surface (`Space::refresh`);
        // the one below takes over.
        self.space.refresh();
        let next = self.space.elements().last().cloned();
        self.focus(next);
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        self.start_move(&surface, serial);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        self.start_resize(&surface, serial, edges);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for(surface.wl_surface()) {
            self.minimize(&window);
        }
    }

    fn app_id_changed(&mut self, _surface: ToplevelSurface) {
        self.refresh_listing();
    }

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        self.refresh_listing();
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.maximize(&surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.unmaximize(&surface, None);
    }

    /// One screen: the window gets it, whichever output it named.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        self.fullscreen(&surface);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.unfullscreen(&surface);
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        unconstrain(self, &surface);
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            warn!("a popup: {e}");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        unconstrain(self, &surface);
        surface.send_repositioned(token);
    }

    /// A menu that closes on a click elsewhere: the pointer and keyboard
    /// stay with it until then, as Smithay's anvil and niri grab. Declined
    /// – the popup closes at once – unless the click that opened it still
    /// holds the pointer (`serial`), or the keyboard's grab is the parent
    /// menu's (a submenu).
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        // A grab declined dismisses the popup (xdg-shell).
        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            surface.send_popup_done();
            return;
        };
        let kind = PopupKind::Xdg(surface.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            surface.send_popup_done();
            return;
        };
        // An app's menu answers its own input: the button it holds, or the
        // keyboard it has. The shell's may follow a press long past (moving
        // from menu to menu in the bar).
        let shell = root
            .client()
            .and_then(|c| c.get_data::<ClientState>().map(|d| d.shell))
            .unwrap_or(false);
        let holds = seat.get_pointer().is_some_and(|p| p.has_grab(serial))
            || seat
                .get_keyboard()
                .and_then(|k| k.current_focus())
                .is_some_and(|focus| focus.client() == root.client());
        if !shell && !holds {
            surface.send_popup_done();
            return;
        }
        // A menu closed by its app just before – the Beize menu, when the
        // same click opens the control center – still holds the pointer
        // until its next event: let it go first.
        self.popups.cleanup();
        if self.popup_grab.as_ref().is_some_and(|g| g.has_ended()) {
            self.popup_grab = None;
            if let Some(pointer) = seat.get_pointer() {
                pointer.unset_grab(self, serial, self.time_msec());
            }
            if let Some(keyboard) = seat.get_keyboard() {
                keyboard.unset_grab(self);
            }
        }
        let mut grab = match self.popups.grab_popup(root, kind, &seat, serial) {
            Ok(grab) => grab,
            Err(e) => {
                warn!("a popup's grab: {e}");
                surface.send_popup_done();
                return;
            }
        };
        let earlier = grab.previous_serial().unwrap_or(serial);
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed() && !(keyboard.has_grab(serial) || keyboard.has_grab(earlier)) {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
        self.popup_grab = Some(grab);
    }
}

impl State {
    /// A press outside the menu's app closes the menu and lets go of the
    /// pointer before the press is looked at, so it does what it does
    /// without a menu: a title bar drags, an edge resizes, the window comes
    /// forward (owner, 28.10.). Smithay's `PopupPointerGrab::button` closes
    /// the menu too and hands the press on, but past the pointer's own
    /// handling: no click grab, so the compositor's title bar never saw it
    /// and an app's own was refused its move (`windows::grab_for`).
    pub fn press_ends_popup_grab(&mut self) {
        let Some(mut grab) = self.popup_grab.take() else {
            return;
        };
        // What the menu's app takes itself: the shell all its surfaces, each
        // of which closes its menus (a press on the bar); an app the menus
        // of this grab – its own window's content or title bar closes them
        // and does what it does without a menu.
        let under = self
            .seat
            .get_pointer()
            .and_then(|pointer| pointer.current_focus());
        let ours = under
            .as_ref()
            .zip(grab.current_grab())
            .is_some_and(|(under, menu)| {
                let shell = under
                    .client()
                    .and_then(|c| c.get_data::<ClientState>().map(|d| d.shell))
                    .unwrap_or(false);
                under.client() == menu.client()
                    && (shell || self.popups.find_popup(under).is_some())
            });
        if !ours || grab.has_ended() {
            grab.ungrab(PopupUngrabStrategy::All);
            // Back to the surface under the pointer, which the grab hid.
            self.let_go_of(&grab);
        }
        // It ends once the app has destroyed its menus
        // (`popup_grab_ended`).
        self.popup_grab = Some(grab);
    }
}

/// After a commit: the first configure of a new window or popup, which the
/// protocol wants only after the client's initial commit.
pub fn committed(state: &mut State, surface: &WlSurface) {
    if let Some(window) = state.window_for(surface) {
        if let Some(toplevel) = window.toplevel()
            && !toplevel.is_initial_configure_sent()
        {
            toplevel.send_configure();
        }
        state.window_committed(&window);
        return;
    }
    if let Some(PopupKind::Xdg(popup)) = state.popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
        && let Err(e) = popup.send_configure()
    {
        warn!("configuring a popup: {e}");
    }
}

/// Moves the popup so it stays on the screen, as far as its positioner
/// allows; its root is a window or a panel (the shell's menus, whose
/// parent comes later: `layers`).
pub fn unconstrain(state: &State, popup: &PopupSurface) {
    let kind = PopupKind::Xdg(popup.clone());
    let Ok(root) = find_popup_root_surface(&kind) else {
        return;
    };
    let window_area = state
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        .and_then(|window| state.space.element_geometry(window));
    let Some(root_at) = window_area
        .map(|area| area.loc)
        .or_else(|| crate::layers::place_of(state, &root).map(|place| place.loc))
    else {
        return;
    };
    // On the windows' area, clear of the panels, which lie in front of a
    // window's popups; a fullscreen window and the shell's menus have the
    // screen. The positioner works relative to the popup's parent.
    let fills_screen = window_area.is_none()
        || state
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            .is_some_and(crate::windows::fullscreen);
    let mut target = if fills_screen {
        state.screen_area()
    } else {
        state.window_area()
    };
    target.loc -= get_popup_toplevel_coords(&kind);
    target.loc -= root_at;
    popup.with_pending_state(|s| {
        s.geometry = s.positioner.get_unconstrained_geometry(target);
    });
}
