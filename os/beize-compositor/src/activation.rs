//! An app asks for one of its windows to become active (xdg-activation),
//! with a token another app – or itself – made for it: a link clicked in
//! the terminal opens in the browser's window, a second launch shows the
//! first one's. Only the active app makes tokens that count, from an input
//! event or its focus (the serial), and only while that focus lasts: once
//! the keyboard went elsewhere, the user moved on and an older token
//! activates nothing (niri's and anvil's rule, Mutter's user time). No
//! clock: what ends a token is the focus moving, as macOS lets only the
//! active app hand on activation (cooperative activation, macOS 14).
//!
//! A refused request changes nothing and is logged; macOS bounces the
//! app's dock icon then, which comes with the shell's step 4.

use smithay::delegate_xdg_activation;
use smithay::input::Seat;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use tracing::info;

use crate::state::State;

impl State {
    /// Why a token does not count now, if it does not.
    fn refusal(&self, data: &XdgActivationTokenData) -> Option<&'static str> {
        let Some((serial, seat)) = &data.serial else {
            return Some("no event's serial");
        };
        if Seat::<State>::from_resource(seat).as_ref() != Some(&self.seat) {
            return Some("another seat");
        }
        let focus = self.keyboard.current_focus();
        if focus.as_ref().and_then(|s| s.client()).map(|c| c.id()) != data.client_id {
            return Some("not from the active app");
        }
        match self.keyboard.last_enter() {
            Some(entered) if serial.is_no_older_than(&entered) => None,
            _ => Some("the focus moved since"),
        }
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation
    }

    /// Tokens that cannot count are dropped at once; so are those the focus
    /// left behind, which never will again.
    fn token_created(&mut self, _: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let entered = self.keyboard.last_enter();
        self.activation.retain_tokens(|_, d| {
            d.serial
                .as_ref()
                .zip(entered)
                .is_some_and(|((serial, _), entered)| serial.is_no_older_than(&entered))
        });
        self.refusal(&data).is_none()
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // A token is spent once used, as niri does.
        self.activation.remove_token(&token);
        let Some(window) = self.window_for(&surface) else {
            return;
        };
        let app = crate::windows::app_id(&window);
        if let Some(why) = self.refusal(&data) {
            info!("activation of {app} refused: {why}");
            return;
        }
        info!("activated {app}");
        // One not yet placed becomes active when it is.
        if self
            .space
            .elements()
            .chain(&self.minimized)
            .any(|w| *w == window)
        {
            self.bring_back(window);
        }
    }
}

delegate_xdg_activation!(State);
