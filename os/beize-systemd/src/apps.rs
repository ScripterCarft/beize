//! Apps start in the user's service manager, each in its own unit, never as
//! children of whoever asks – the compositor's sandbox or the shell's
//! (ADR 0027 §9, ADR 0028 §4). The unit is an instance of the app's
//! template, named as systemd's desktop integration asks
//! (`app-<id>@<random>.service`, the id escaped as systemd-escape does), so
//! tools see which app a process belongs to.
//!
//! Asked on a short thread of its own with its own connection: the caller's
//! loop never waits for the manager, and what goes wrong lands in the log.

use std::thread;

use tracing::{info, warn};

use crate::Manager;

/// Starts a new instance of the app `app_id` (its desktop file's name, as
/// in `xdg_toplevel.app_id`).
pub fn start(app_id: &str) {
    let unit = match instance() {
        Ok(random) => format!("app-{}@{random}.service", escape(app_id)),
        Err(e) => {
            warn!("starting {app_id}: no random instance name: {e}");
            return;
        }
    };
    let spawned = thread::Builder::new()
        .name("app start".into())
        .spawn(
            move || match Manager::connect().and_then(|m| m.start(&unit)) {
                Ok(_) => info!("started {unit}"),
                Err(e) => warn!("starting {unit}: {e}"),
            },
        );
    if let Err(e) = spawned {
        warn!("starting {app_id}: {e}");
    }
}

/// `name` as part of a unit name, as `systemd-escape` writes it: ASCII
/// letters, digits, `:`, `_` and `.` (not first) stay, `/` becomes `-`,
/// every other byte `\xNN`.
pub fn escape(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for (i, byte) in name.bytes().enumerate() {
        match byte {
            b'/' => escaped.push('-'),
            b'.' if i == 0 => escaped.push_str("\\x2e"),
            b if b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.') => {
                escaped.push(char::from(b));
            }
            b => escaped.push_str(&format!("\\x{b:02x}")),
        }
    }
    escaped
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

    #[test]
    fn escapes_as_systemd_escape() {
        // `systemd-escape beize-terminal` and the like.
        assert_eq!(escape("beize-terminal"), "beize\\x2dterminal");
        assert_eq!(escape("org.gnome.Nautilus"), "org.gnome.Nautilus");
        assert_eq!(escape(".hidden"), "\\x2ehidden");
        assert_eq!(escape("a b/c"), "a\\x20b-c");
    }
}
