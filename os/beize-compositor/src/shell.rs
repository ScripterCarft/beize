//! Windows (xdg-shell). Until Beize's shell arranges windows (ADR 0028
//! §2), every window fills what the panels leave of the screen, the newest
//! on top and active, with the keyboard. Popups (menus) open where the client asks, kept on
//! the screen.
//!
//! Popup grabs (a menu that closes on a click elsewhere) are declined: the
//! protocol then closes the popup (`popup_done`), which every client
//! handles. They come with Beize UI's menus.

use smithay::desktop::{PopupKind, Window, find_popup_root_surface, get_popup_toplevel_coords};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_seat, wl_surface::WlSurface};
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
        let area = self.window_area();
        surface.with_pending_state(|state| {
            state.size = Some(area.size);
            state.states.set(xdg_toplevel::State::Maximized);
        });
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window.clone(), area.loc, false);
        self.focus(Some(window));
    }

    fn toplevel_destroyed(&mut self, _surface: ToplevelSurface) {
        // The window leaves the space with its surface (`Space::refresh`);
        // the one below takes over.
        self.space.refresh();
        let next = self.space.elements().last().cloned();
        self.focus(next);
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
    let toplevel = state
        .space
        .elements()
        .filter_map(Window::toplevel)
        .find(|t| t.wl_surface() == surface);
    if let Some(toplevel) = toplevel {
        if !toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
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
