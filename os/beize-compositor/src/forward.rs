//! Who may come to the front with the keyboard by itself (the owner's rule,
//! 1.11.): nobody can steal the keys while they are typed, a password least of
//! all, and a window in front always has them, so no key goes to the terminal
//! while it looks as if it went to the window on top.
//!
//! A new window takes the keyboard when
//! - no window is active: there is nothing to take it from;
//! - it asked with a token of the active app (`activation`);
//! - it is of the active app: the same cgroup, so what the terminal started too
//!   (Mutter's and KWin's focus-stealing prevention keep those together);
//! - the user's last input went to Beize itself, the dock, a menu of the bar or
//!   a key of the compositor: that is how apps are started. Any input to an
//!   app ends this, without a clock (`input`), as a token ends with the focus;
//! - the user allows it: its unit named in `come-forward` in the configuration
//!   directory, for a program that must ask at once, as a password manager's
//!   unlock (macOS asks for a permission like that once).
//!
//! Any other opens behind the active window, not active, and the dock has it.
//! A program of the user's own can write that file as it can trace the
//! terminal: no sandbox stands between them yet. The file is the place Beize's
//! settings will show.

use smithay::desktop::Window;
use smithay::reexports::wayland_server::{Client, Resource};
use tracing::info;

use crate::state::{ClientState, State};

const FILE: &str = "come-forward";

impl State {
    /// Input went to `client`: to Beize, the next window is asked for (at most
    /// one more, however often the bar is clicked); to an app, the user moved on.
    pub fn input_to(&mut self, client: Option<&Client>) {
        let shell = client
            .and_then(|client| client.get_data::<ClientState>())
            .is_some_and(|data| data.shell);
        self.launching = if shell { self.launching.max(1) } else { 0 };
    }

    /// Whether `window`, new, comes to the front with the keyboard; it takes
    /// one of the windows expected.
    pub fn takes_forward(&mut self, window: &Window) -> bool {
        let launched = self.launching > 0;
        self.launching = self.launching.saturating_sub(1);
        launched || self.may_come_forward(window)
    }

    /// The same without one expected: asked for later, as for the whole screen.
    pub fn may_come_forward(&self, window: &Window) -> bool {
        let Some(active) = self.active_window() else {
            return true;
        };
        if crate::windows::activation_asked(window) {
            return true;
        }
        let cgroup = cgroup_of(window);
        if cgroup.is_some() && cgroup == cgroup_of(&active) {
            return true;
        }
        let allowed = cgroup
            .and_then(crate::trust::unit_of)
            .is_some_and(|unit| allowed(&unit));
        if !allowed {
            info!(
                "{} opens behind the active window",
                crate::windows::app_id(window)
            );
        }
        allowed
    }
}

fn cgroup_of(window: &Window) -> Option<u64> {
    let client = window.toplevel()?.wl_surface().client()?;
    client.get_data::<ClientState>()?.cgroup
}

fn allowed(unit: &str) -> bool {
    let Some(file) = std::env::var_os("CONFIGURATION_DIRECTORY") else {
        return false;
    };
    let path = std::path::Path::new(&file).join(FILE);
    std::fs::read_to_string(path).is_ok_and(|text| listed(&text, unit))
}

/// Whether the file's lines name `unit`: an app by its id, as the dock starts
/// it (`app-<id>@<random>.service`), or any unit by the start of its name.
fn listed(text: &str, unit: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|line| {
            unit.starts_with(line)
                || unit
                    .strip_prefix("app-")
                    .and_then(|rest| rest.split_once('@'))
                    .is_some_and(|(id, _)| id == line)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_names_apps_and_units() {
        let text = "# who may come forward\n\n1password\nbeize-test\n";
        assert!(listed(text, "app-1password@abc.service"));
        assert!(listed(text, "beize-testclient.service"));
        assert!(!listed(text, "app-1password2@abc.service"));
        assert!(!listed(text, "app-evil@abc.service"));
        assert!(!listed("", "app-1password@abc.service"));
    }
}
