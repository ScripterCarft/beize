//! Whether a new window comes to the front with the keyboard (ADR 0030).
//!
//! Mutter's rule, which the apps for Linux are made for: a new toplevel
//! takes the focus. Mutter keeps one back only when the user worked in the
//! active window after the new one's time (`intervening_user_event_occurred`,
//! `focus-new-windows=smart`); a Wayland window without a token has no time,
//! so it comes forward - KWin's default (`FocusStealingPreventionLevel`
//! Low) does the same. 1Password's unlock window is built on that: a new
//! window each time, no token of its own, no focus asked for.
//!
//! What still needs a token (`activation`): an existing window coming back
//! to the front, as a link opened in the browser's window, or one asking for
//! the whole screen. One without it stays where it is.
//!
//! The front always has the keyboard (owner, 1.11.): a window in front that
//! the keys did not reach would take a password typed into the terminal
//! behind it. So a new window opens behind the active one only while Beize
//! itself holds the keyboard - a menu of the shell, Alt+Tab, an overlay of
//! the shell - not to take it from there; then it is not active either.
//! With no window active it becomes the active one, and has the keyboard
//! once Beize lets go of it (`popup_grab_ended`, `layer_destroyed`). An
//! app's own menu holds nothing back: the new window's focus ends it, as
//! Mutter's and niri's.

use smithay::desktop::Window;
use tracing::info;

use crate::state::State;

impl State {
    /// Whether `window`, new, comes to the front with the keyboard.
    pub fn takes_forward(&self, window: &Window) -> bool {
        let held = self.beize_holds_keyboard();
        if held {
            info!(
                "{} opens behind: Beize holds the keyboard",
                crate::windows::app_id(window)
            );
        }
        !held
    }

    /// Beize itself has the keys: an overlay of the shell, Alt+Tab, or a
    /// menu of the shell still grabbing.
    pub fn beize_holds_keyboard(&self) -> bool {
        self.overlay_keyboard.is_some() || self.switcher.is_some() || self.shell_menu_grabs()
    }
}
