//! Windows (xdg-shell): floating, placed, moved and resized as `windows`
//! has it (ADR 0028 §2), the newest on top and active, with the keyboard.
//! Popups (menus) open where the client asks, kept on the screen.
//!
//! Popup grabs (a menu that closes on a click elsewhere) are declined: the
//! protocol then closes the popup (`popup_done`), which every client
//! handles. They come with Beize UI's menus. A minimized window comes back
//! through the shell (`windows`, `shell_v1`).

use smithay::desktop::{PopupKind, find_popup_root_surface, get_popup_toplevel_coords};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{
    wl_output::WlOutput, wl_seat, wl_surface::WlSurface,
};
use smithay::utils::Serial;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use tracing::warn;

use crate::state::State;

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

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        surface.send_popup_done();
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
/// allows.
fn unconstrain(state: &State, popup: &PopupSurface) {
    let kind = PopupKind::Xdg(popup.clone());
    let Ok(root) = find_popup_root_surface(&kind) else {
        return;
    };
    let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
    else {
        return;
    };
    let Some(window_area) = state.space.element_geometry(window) else {
        return;
    };
    // The positioner works relative to the popup's parent.
    let mut target = state.screen_area();
    target.loc -= get_popup_toplevel_coords(&kind);
    target.loc -= window_area.loc;
    popup.with_pending_state(|s| {
        s.geometry = s.positioner.get_unconstrained_geometry(target);
    });
}
