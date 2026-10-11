//! `beize-accounts`: the one way Beize changes people's accounts (ADR 0019),
//! like Open Directory on macOS – the setup, later the settings, and the
//! recovery call [`Accounts`], never a backend directly. Reading accounts and
//! signing in stay with NSS/userdb and PAM, which know every backend already.
//!
//! The module behind it is [`Homed`] (systemd-homed). Another one – classic
//! /etc/shadow – would implement [`Accounts`] as well and has to pass the same
//! contract: the smoke test's account scenario.
//!
//! One recovery key for everything: every account gets the computer's
//! recovery key (the one of the encrypted root partition) as its own, so the
//! key that opens the data and the recovery also sets a new password.

mod bus;
mod crypt;
mod homed;
mod json;
mod secret;

use std::ffi::{CString, c_char, c_void};

pub use homed::Homed;
pub use secret::Secret;

/// What a password needs at least, counted in characters.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// A person's account to create.
pub struct NewAccount<'a> {
    /// The login name (see [`validate_name`]).
    pub name: &'a str,
    /// The name shown for the person; may be empty.
    pub real_name: &'a str,
    pub password: &'a str,
    /// The computer's recovery key, as systemd-cryptenroll printed it.
    pub recovery_key: &'a str,
    /// Member of `wheel`: run0 with their own password (ADR 0018).
    pub administrator: bool,
}

/// The changes to people's accounts. Errors are sentences for the person.
pub trait Accounts {
    fn create(&self, account: &NewAccount) -> Result<(), String>;
    /// Removes the account and its home with everything in it.
    fn remove(&self, name: &str) -> Result<(), String>;
    /// A new password, proven with the current one.
    fn change_password(&self, name: &str, current: &str, new: &str) -> Result<(), String>;
    /// A new password, proven with the computer's recovery key – for a
    /// forgotten password, in the recovery.
    fn reset_password(&self, name: &str, recovery_key: &str, new: &str) -> Result<(), String>;
}

/// Whether `name` is acceptable as a login name: portable characters only,
/// starting with a letter, at most 32 characters, and not a system name.
pub fn validate_name(name: &str) -> Result<(), String> {
    const RESERVED: [&str; 8] = [
        "root", "admin", "daemon", "nobody", "bin", "sys", "wheel", "beize",
    ];
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !first_ok || !rest_ok || name.len() > 32 {
        return Err(
            "use lowercase letters, digits, - and _, starting with a letter (at most 32)".into(),
        );
    }
    if RESERVED.contains(&name) {
        return Err(format!("\"{name}\" is reserved for the system"));
    }
    Ok(())
}

/// Whether `password` is long enough.
pub fn validate_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!("at least {MIN_PASSWORD_CHARS} characters, please"));
    }
    Ok(())
}

unsafe extern "C" {
    fn getpwnam(name: *const c_char) -> *mut c_void;
    fn getgrnam(name: *const c_char) -> *mut c_void;
}

/// Whether a user or a group of this name exists, wherever it is kept
/// (NSS: /etc, homed, systemd's dynamic users).
pub fn exists(name: &str) -> bool {
    let Ok(name) = CString::new(name) else {
        return false;
    };
    // SAFETY: NUL-terminated name; only the null-ness of the static result
    // is looked at, nothing is kept.
    unsafe { !getpwnam(name.as_ptr()).is_null() || !getgrnam(name.as_ptr()).is_null() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_login_names() {
        assert!(validate_name("joe").is_ok());
        assert!(validate_name("joe-2_b").is_ok());
        for bad in [
            "",
            "Joe",
            "1joe",
            "jo e",
            "jo:e",
            "root",
            "wheel",
            &"a".repeat(33),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn passwords_need_eight_characters() {
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("12345678").is_ok());
        // Characters, not bytes.
        assert!(validate_password("äöüäöüä").is_err());
    }

    #[test]
    fn root_exists_and_nonsense_does_not() {
        assert!(exists("root"));
        assert!(!exists("beize-no-such-account"));
        assert!(!exists("nul\0byte"));
    }
}
