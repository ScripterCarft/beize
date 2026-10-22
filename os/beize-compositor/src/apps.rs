//! Apps start in the user's service manager, each in its own unit, never as
//! children of the compositor: they would inherit its sandbox (ADR 0027 §9).
//! The unit is an instance of the app's template, named as systemd's
//! desktop integration asks (`app-<id>@<random>.service`, dashes in the id
//! escaped), so tools see which app a process belongs to.
//!
//! Asked on a short thread of its own with its own connection: the frame
//! loop never waits for the manager, and what goes wrong lands in the log.

use std::thread;

use tracing::{info, warn};

/// The terminal's template (`os/beize-terminal`, app id `beize-terminal`).
const TERMINAL: &str = "app-beize\\x2dterminal";

/// Opens a new terminal window.
pub fn open_terminal() {
    start(TERMINAL);
}

fn start(template: &'static str) {
    let unit = match instance() {
        Ok(random) => format!("{template}@{random}.service"),
        Err(e) => {
            warn!("starting {template}: no random instance name: {e}");
            return;
        }
    };
    let spawned = thread::Builder::new()
        .name("app start".into())
        .spawn(move || {
            match beize_systemd::Manager::connect().and_then(|manager| manager.start(&unit)) {
                Ok(_) => info!("started {unit}"),
                Err(e) => warn!("starting {unit}: {e}"),
            }
        });
    if let Err(e) = spawned {
        warn!("starting {template}: {e}");
    }
}

/// 64 random bits in hex, as the instance of an app's unit.
fn instance() -> std::io::Result<String> {
    let mut bytes = [0u8; 8];
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())?;
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_differ() {
        let (a, b) = (instance().unwrap(), instance().unwrap());
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
    }
}
