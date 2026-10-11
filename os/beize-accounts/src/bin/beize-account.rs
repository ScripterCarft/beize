//! `beize-account` – accounts on the console, through Beize's account service
//! (ADR 0019), until the settings (M3) offer the same:
//!
//! - `beize-account passwd`: a new password for yourself, with the current one.
//! - `beize-account reset NAME`: a new password for NAME without the current
//!   one – an administrator with their own password, or root: in the
//!   recovery (ADR 0018) for a forgotten password.
//! - `beize-account add [--administrator] NAME`: a new account, by an
//!   administrator with their own password.

use std::ffi::{CStr, c_char};
use std::process::{Command, ExitCode, Stdio};

use beize_accounts::{Client, NewAccount, Secret, validate_name, validate_password};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["passwd"] => passwd(),
        ["reset", name] => reset(name),
        ["add", name] => add(name, false),
        ["add", "--administrator", name] => add(name, true),
        _ => Err("usage: beize-account passwd | reset NAME | add [--administrator] NAME".into()),
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
    let current = ask(&format!("Current password of {name}:"), false)?;
    let new = new_password("New password")?;
    Client::connect()?.change_password(&name, current.expose(), new.expose())?;
    Ok("Password changed.".into())
}

fn reset(name: &str) -> Result<String, String> {
    let auth = own_password()?;
    let new = new_password(&format!("New password of {name}"))?;
    Client::connect()?.reset_password(name, new.expose(), auth.as_ref().map(Secret::expose))?;
    Ok(format!("New password set for {name}."))
}

fn add(name: &str, administrator: bool) -> Result<String, String> {
    validate_name(name).map_err(|reason| format!("Not possible: {reason}."))?;
    let auth = own_password()?;
    let real_name = ask(&format!("Full name of {name} (optional):"), true)?;
    let password = new_password(&format!("Password of {name}"))?;
    Client::connect()?.create(
        &NewAccount {
            name,
            real_name: real_name.expose(),
            password: password.expose(),
            administrator,
        },
        auth.as_ref().map(Secret::expose),
    )?;
    Ok(format!("Account {name} created."))
}

/// An administrator proves who they are with their own password; root
/// needs none.
fn own_password() -> Result<Option<Secret>, String> {
    // SAFETY: no arguments, no side effects.
    if unsafe { getuid() } == 0 {
        return Ok(None);
    }
    ask(&format!("Your password ({}):", own_name()?), false).map(Some)
}

/// Asks for a new password twice until both agree and it is long enough.
fn new_password(question: &str) -> Result<Secret, String> {
    loop {
        let first = ask(&format!("{question}:"), false)?;
        if let Err(reason) = validate_password(first.expose()) {
            println!("{reason}");
            continue;
        }
        if ask(&format!("{question} again:"), false)?.expose() == first.expose() {
            return Ok(first);
        }
        println!("The passwords differ.");
    }
}

/// Asks on the terminal through systemd-ask-password, which hides a password
/// as it is typed; it asks the terminal only if its stdin is one.
fn ask(question: &str, echo: bool) -> Result<Secret, String> {
    let output = Command::new("systemd-ask-password")
        .arg(if echo { "--echo=yes" } else { "--echo=no" })
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
            return Err("Your account was not found.".into());
        }
        Ok(CStr::from_ptr((*record).name)
            .to_string_lossy()
            .into_owned())
    }
}
