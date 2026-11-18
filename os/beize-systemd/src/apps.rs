//! Apps start in the user's service manager, each in its own unit, never as
//! children of whoever asks – the compositor's sandbox or the shell's
//! (ADR 0027 §9, ADR 0028 §4). The unit is transient, made from the app's
//! desktop file (`desktop`: its `Exec`, its name), as KDE starts its apps:
//! any app with a desktop file starts so, and the activation token of
//! whoever started it goes into its environment (`XDG_ACTIVATION_TOKEN`,
//! xdg-activation; ADR 0032 §4), which a template's instance could not
//! take. It is named as systemd's desktop integration asks
//! (`app-<id>@<random>.service`, the id escaped as systemd-escape does), so
//! tools see which app a process belongs to; an app of the graphical
//! session (systemd.special(7)), it stops with it, and a window closed with
//! a failing last command leaves nothing behind.
//!
//! Asked on a short thread of its own with its own connection: the caller's
//! loop never waits for the manager, and what goes wrong lands in the log.

use std::thread;

use tracing::{info, warn};

use crate::{Manager, Property, desktop};

/// Starts a new instance of the app `app_id` (its desktop file's name, as
/// in `xdg_toplevel.app_id`), with the activation `token` its first window
/// shows with, if one was made for it.
pub fn start(app_id: &str, token: Option<String>) {
    let unit = match instance() {
        Ok(random) => format!("app-{}@{random}.service", escape(app_id)),
        Err(e) => {
            warn!("starting {app_id}: no random instance name: {e}");
            return;
        }
    };
    let app_id = app_id.to_owned();
    let spawned = thread::Builder::new()
        .name("app start".into())
        .spawn(move || match run(&app_id, &unit, token) {
            Ok(()) => info!("started {unit}"),
            Err(e) => warn!("starting {unit}: {e}"),
        });
    if let Err(e) = spawned {
        warn!("starting an app: {e}");
    }
}

/// Starts `unit` running the app `app_id`, with `token`.
fn run(app_id: &str, unit: &str, token: Option<String>) -> Result<(), String> {
    let (path, entry) = desktop::find(app_id).ok_or("no desktop file")?;
    let exec = entry
        .exec
        .as_deref()
        .ok_or("its desktop file has no Exec")?;
    let argv = desktop::command(exec, &entry, &path).ok_or("its Exec is malformed")?;
    let program = argv
        .first()
        .and_then(|name| desktop::program(name))
        .ok_or_else(|| format!("no program {:?}", argv.first()))?;
    let program = program.to_string_lossy();
    let session = ["graphical-session.target".to_owned()];
    let environment: Vec<String> = token
        .into_iter()
        .map(|t| format!("XDG_ACTIVATION_TOKEN={t}"))
        .collect();
    let name = entry.name.as_deref().unwrap_or(app_id);
    let mut properties = vec![
        Property::Text("Description", name),
        Property::Exec(&program, &argv),
        Property::List("PartOf", &session),
        Property::List("After", &session),
        Property::Text("CollectMode", "inactive-or-failed"),
    ];
    if !environment.is_empty() {
        properties.push(Property::List("Environment", &environment));
    }
    Manager::connect()?.start_transient(unit, &properties)?;
    Ok(())
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
