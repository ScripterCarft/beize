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
//! The shell's tokens count too: its dock starts an app with one in the
//! app's environment (`XDG_ACTIVATION_TOKEN`, `beize_systemd::apps`), as
//! GNOME's shell makes its own startup sequences (ADR 0032 §4). The app's
//! first window shows with it and grows out of the app's symbol in the
//! dock. They last until used, the newest `SHELL_TOKENS` of them: an app
//! that never uses its token leaves nothing behind for long.
//!
//! A refused request changes nothing and is logged; macOS bounces the
//! app's dock icon then, which comes with the shell's step 4.

use std::cell::Cell;

use smithay::delegate_xdg_activation;
use smithay::desktop::Window;
use smithay::input::Seat;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use tracing::info;

use crate::state::{ClientState, State};

/// The most tokens of the shell's kept unused: more than apps a user starts
/// from the dock at once.
const SHELL_TOKENS: usize = 8;

/// Whether the shell made the token: its dock's (`trust` knows the shell).
fn shells(data: &XdgActivationTokenData) -> bool {
    data.surface
        .as_ref()
        .and_then(|surface| surface.client())
        .is_some_and(|client| {
            Some(client.id()) == data.client_id
                && client.get_data::<ClientState>().is_some_and(|c| c.shell)
        })
}

/// A window not yet shown that asked for activation with the shell's
/// token: it grows out of its app's symbol when it shows
/// (`State::window_committed`), once.
struct FromDock(Cell<bool>);

fn mark_from_dock(window: &Window) {
    let data = window.user_data();
    data.insert_if_missing(|| FromDock(Cell::new(false)));
    if let Some(from) = data.get::<FromDock>() {
        from.0.set(true);
    }
}

/// Whether `window`, showing now, grows out of its app's symbol: once.
pub fn take_from_dock(window: &Window) -> bool {
    window
        .user_data()
        .get::<FromDock>()
        .is_some_and(|from| from.0.replace(false))
}

impl State {
    /// Why a token does not count now, if it does not.
    fn refusal(&self, data: &XdgActivationTokenData) -> Option<&'static str> {
        if shells(data) {
            return None;
        }
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
    /// left behind, which never will again, and the shell's beyond the
    /// newest `SHELL_TOKENS`.
    fn token_created(&mut self, _: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let entered = self.keyboard.last_enter();
        let mut shell_tokens: Vec<_> = self
            .activation
            .tokens()
            .filter(|(_, d)| shells(d))
            .map(|(_, d)| d.timestamp)
            .collect();
        shell_tokens.sort_unstable();
        let oldest_kept = shell_tokens
            .len()
            .checked_sub(SHELL_TOKENS - 1)
            .and_then(|drop| shell_tokens.get(drop).copied());
        self.activation.retain_tokens(|_, d| {
            if shells(d) {
                return oldest_kept.is_none_or(|oldest| d.timestamp >= oldest);
            }
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
        let placed = self.space.elements().any(|w| *w == window);
        if shells(&data) {
            // Not yet shown: it grows out of its symbol when it is. Just
            // shown, its growing in place not yet under way: out of its
            // symbol instead.
            if !placed {
                mark_from_dock(&window);
            } else if self.animations.opens_next(&window) {
                self.come_out(&window, true);
            }
        }
        // One not yet placed comes forward when it is, as any new window
        // (`forward`).
        if placed || self.minimized.contains(&window) {
            self.bring_back(window);
        }
    }
}

delegate_xdg_activation!(State);
