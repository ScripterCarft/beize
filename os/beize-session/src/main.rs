//! `beize-session` – starts Beize's desktop from a login on the console
//! (ADR 0027 §6), until Beize has its own login window. Run after signing
//! in on tty1, it stays in the login session as gnome-session and
//! startplasma do:
//!
//! 1. it tells the user's service manager about the session: its id for
//!    libseat, the session type (libseat passes it on to logind) and the
//!    compositor's socket for the apps the manager starts;
//! 2. it starts `beize-session.target`, which runs
//!    `beize-compositor.service` and binds systemd's
//!    `graphical-session.target`;
//! 3. it waits: when the compositor ends, it takes the session's settings
//!    back and returns to the shell; when the login ends (SIGHUP, SIGTERM),
//!    it stops the desktop first.
//!
//! The compositor runs in the user's service manager, not under this
//! process: its own unit, sandbox and journal, as GNOME runs its shell.

mod systemd;

use std::process::ExitCode;

use systemd::{Manager, UNIT, Watcher};

const TARGET: &str = "beize-session.target";
const COMPOSITOR: &str = "beize-compositor.service";
/// The compositor's socket: a fixed name, as mutter's and KWin's, since
/// each user has one compositor (beize-compositor binds the same).
const WAYLAND_DISPLAY: &str = "wayland-0";

unsafe extern "C" {
    fn getuid() -> u32;
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("beize-session: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    // SAFETY: getuid cannot fail.
    if unsafe { getuid() } == 0 {
        return Err("the desktop runs as a user, never as root: sign in with your account".into());
    }
    systemd::block_login_signals().map_err(at("blocking the login's signals"))?;
    let session = systemd::own_session()
        .map_err(|e| format!("not in a login session ({e}): sign in on the console first"))?;
    systemd::seat(&session).map_err(|e| {
        format!("the session has no seat ({e}): start the desktop at the computer itself")
    })?;
    let manager = Manager::connect().map_err(at("connecting to the user's service manager"))?;
    let target = manager
        .unit_path(TARGET)
        .map_err(at("loading the target"))?;
    let state = manager
        .property(&target, UNIT, "ActiveState")
        .map_err(at("the target's state"))?;
    if !matches!(state.as_str(), "inactive" | "failed") {
        return Err("the desktop already runs, in another login".into());
    }

    let environment = environment(&session);
    manager
        .set_environment(&environment)
        .map_err(at("setting the session's environment"))?;
    let mut desktop = Desktop::default();
    let watched = manager.watch(&target, &mut desktop, |manager| {
        manager.start(TARGET).map_err(at("starting the target"))
    });
    // The settings belong to this login only.
    let names = environment
        .iter()
        .filter_map(|a| a.split_once('=').map(|(name, _)| name.to_owned()))
        .collect::<Vec<_>>();
    let unset = manager
        .unset_environment(&names)
        .map_err(at("taking the session's environment back"));
    watched?;
    unset?;
    if let Some(e) = desktop.error {
        return Err(e);
    }
    let journal = format!("see `journalctl --user -u {COMPOSITOR}`");
    if !desktop.started {
        return Err(format!("the desktop did not start; {journal}"));
    }
    if !desktop.stopping {
        let compositor = manager
            .unit_path(COMPOSITOR)
            .map_err(at("loading the compositor's unit"))?;
        let result = manager
            .property(&compositor, "org.freedesktop.systemd1.Service", "Result")
            .map_err(at("how the compositor ended"))?;
        if result != "success" {
            return Err(format!(
                "the compositor ended with an error ({result}); {journal}"
            ));
        }
    }
    Ok(())
}

/// Names the step that failed.
fn at(what: &str) -> impl Fn(String) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// What the user's service manager learns of this session.
fn environment(session: &str) -> Vec<String> {
    vec![
        format!("XDG_SESSION_ID={session}"),
        "XDG_SESSION_TYPE=wayland".into(),
        format!("WAYLAND_DISPLAY={WAYLAND_DISPLAY}"),
    ]
}

/// The desktop's course: the start job of `beize-session.target`, then
/// its state.
#[derive(Default)]
struct Desktop {
    /// The start job is done: the compositor is ready.
    started: bool,
    /// The login ends, the desktop is being stopped.
    stopping: bool,
    error: Option<String>,
}

impl Watcher for Desktop {
    fn started(&mut self, result: &str) -> bool {
        // canceled (the login ended meanwhile), dependency (the compositor
        // failed), timeout …
        self.started = result == "done";
        self.started
    }

    fn changed(&mut self, state: &str) -> bool {
        // Before the job is done, the target is still inactive with its job
        // queued.
        !(self.started && matches!(state, "inactive" | "failed"))
    }

    fn signal(&mut self, manager: &Manager) -> bool {
        if self.stopping {
            return true;
        }
        self.stopping = true;
        match manager.stop(TARGET) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("stopping the desktop: {e}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_manager_the_session_and_the_socket() {
        assert_eq!(
            environment("3"),
            [
                "XDG_SESSION_ID=3",
                "XDG_SESSION_TYPE=wayland",
                "WAYLAND_DISPLAY=wayland-0"
            ]
        );
    }

    #[test]
    fn ends_when_the_desktop_is_down_and_not_before() {
        let mut desktop = Desktop::default();
        // The target is inactive while its start job waits for the
        // compositor.
        assert!(desktop.changed("inactive"));
        assert!(desktop.started("done"));
        for state in ["active", "deactivating"] {
            assert!(desktop.changed(state), "{state}");
        }
        assert!(!desktop.changed("inactive"));

        let mut failed = Desktop::default();
        assert!(!failed.started("dependency"));
        assert!(!failed.started);
    }
}
