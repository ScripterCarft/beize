//! `beize-account` – account changes on the console, through the one account
//! interface (ADR 0019), until the settings (M3) offer them:
//!
//! - `beize-account passwd`: a new password for yourself, with the current one.
//! - `beize-account reset NAME`: a new password for a forgotten one, in the
//!   recovery (ADR 0018). The recovery key needs no typing: the gate checked
//!   it and left it in /run/beize/recovery-key for this shell.

use std::ffi::{CStr, c_char};
use std::fs;
use std::process::{Command, ExitCode, Stdio};

use beize_accounts::{Accounts, Homed, Secret, validate_password};

/// Where the recovery's gate (beize-tpm emergency) and the first boot
/// (recovery-key in the initrd) leave the checked recovery key; memory only.
const RECOVERY_KEY: &str = "/run/beize/recovery-key";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["passwd"] => passwd(),
        ["reset", name] => reset(name),
        _ => Err("usage: beize-account passwd | beize-account reset NAME".into()),
    };
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn passwd() -> Result<String, String> {
    let name = own_name()?;
    let current = ask(&format!("Current password of {name}:"))?;
    let new = new_password()?;
    Homed::connect()?.change_password(&name, current.expose(), new.expose())?;
    Ok("Password changed.".into())
}

fn reset(name: &str) -> Result<String, String> {
    let key = fs::read_to_string(RECOVERY_KEY)
        .map(|key| Secret::new(key.trim().to_owned()))
        .map_err(|_| {
            "Only in the recovery: choose \"(Recovery)\" in the boot menu (hold Space \
             while the computer starts), type the recovery key, then \"s\"."
                .to_owned()
        })?;
    // The recovery starts rescue.target, without homed; starting it is
    // root's right there.
    let started = Command::new("systemctl")
        .args(["start", "systemd-homed.service"])
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("starting systemctl: {e}"))?;
    if !started.success() {
        return Err("The account service (systemd-homed) did not start.".into());
    }
    let new = new_password()?;
    Homed::connect()?.reset_password(name, key.expose(), new.expose())?;
    Ok(format!("New password set for {name}."))
}

/// Asks for a new password twice until both agree and it is long enough.
fn new_password() -> Result<Secret, String> {
    loop {
        let first = ask("New password:")?;
        if let Err(reason) = validate_password(first.expose()) {
            println!("{reason}");
            continue;
        }
        if ask("New password again:")?.expose() == first.expose() {
            return Ok(first);
        }
        println!("The passwords differ.");
    }
}

/// Asks on the terminal through systemd-ask-password, which hides what is
/// typed; it asks the terminal only if its stdin is one.
fn ask(question: &str) -> Result<Secret, String> {
    let output = Command::new("systemd-ask-password")
        .args(["--timeout=0", "--id=beize-account", question])
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("starting systemd-ask-password: {e}"))?;
    let answer =
        Secret::new(String::from_utf8(output.stdout).map_err(|_| "The answer is not text.")?);
    if !output.status.success() {
        return Err("Cancelled.".into());
    }
    Ok(Secret::new(
        answer.expose().trim_end_matches('\n').to_owned(),
    ))
}

#[repr(C)]
struct Passwd {
    name: *const c_char,
}

unsafe extern "C" {
    fn getuid() -> u32;
    fn getpwuid(uid: u32) -> *const Passwd;
}

/// The login name of whoever runs this.
fn own_name() -> Result<String, String> {
    // SAFETY: getpwuid returns a static record or null; only its first
    // field (pw_name, a NUL-terminated string) is read, at once.
    unsafe {
        let record = getpwuid(getuid());
        if record.is_null() || (*record).name.is_null() {
            return Err("Who are you? Your account was not found.".into());
        }
        Ok(CStr::from_ptr((*record).name)
            .to_string_lossy()
            .into_owned())
    }
}
