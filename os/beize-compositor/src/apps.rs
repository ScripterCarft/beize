//! The apps the compositor opens itself: a terminal with the desktop and
//! with Ctrl+Alt+T. Started in the user's service manager (beize-systemd,
//! `apps`), never as the compositor's children: they would inherit its
//! sandbox (ADR 0027 §9).

/// The terminal (`os/beize-terminal`).
const TERMINAL: &str = "beize-terminal";

/// Opens a new terminal window.
pub fn open_terminal() {
    beize_systemd::apps::start(TERMINAL);
}
