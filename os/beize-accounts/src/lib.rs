//! `beize-accounts`: how people's accounts change on Beize (ADR 0019). Like
//! Open Directory on macOS, one service is the authority:
//! `beize-accountsd` decides who may do what and keeps the accounts – as
//! systemd user records in /etc/userdb, which NSS and PAM read as they read
//! any account. The setup, `beize-account` and later the settings are its
//! clients ([`Client`]); nothing else writes accounts.
//!
//! The rules both sides share are here: what a name and a password must be.

pub mod crypt;
pub mod protocol;

use std::ffi::{CString, c_char, c_void};
use std::os::unix::net::UnixStream;

pub use beize_varlink::{Secret, json};

use protocol::error;

/// What a password needs at least, counted in characters.
pub const MIN_PASSWORD_CHARS: usize = 8;

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
/// (NSS: /etc/passwd, /etc/userdb, systemd's dynamic users).
pub fn exists(name: &str) -> bool {
    let Ok(name) = CString::new(name) else {
        return false;
    };
    // SAFETY: NUL-terminated name; only the null-ness of the static result
    // is looked at, nothing is kept.
    unsafe { !getpwnam(name.as_ptr()).is_null() || !getgrnam(name.as_ptr()).is_null() }
}

/// A person's account to create.
pub struct NewAccount<'a> {
    /// The login name (see [`validate_name`]).
    pub name: &'a str,
    /// The name shown for the person; may be empty.
    pub real_name: &'a str,
    pub password: &'a str,
    /// Member of `wheel`: run0 with their own password (ADR 0018), and may
    /// create accounts and reset others' passwords.
    pub administrator: bool,
}

/// A connection to the account service. Errors are sentences for the person.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect() -> Result<Self, String> {
        UnixStream::connect(protocol::SOCKET)
            .map(|stream| Self { stream })
            .map_err(|e| format!("The account service is not reachable ({e})."))
    }

    /// Creates the account; `auth` is the caller's own password (an
    /// administrator's), unless the caller is root.
    pub fn create(&mut self, account: &NewAccount, auth: Option<&str>) -> Result<(), String> {
        let mut params = format!(
            "\"name\":{},\"realName\":{},\"password\":{},\"administrator\":{}",
            json::string(account.name),
            json::string(account.real_name),
            json::string(account.password),
            account.administrator
        );
        push_auth(&mut params, auth);
        self.call("Create", Secret::new(params))
    }

    /// A new password, proven with the current one.
    pub fn change_password(&mut self, name: &str, current: &str, new: &str) -> Result<(), String> {
        let params = format!(
            "\"name\":{},\"current\":{},\"new\":{}",
            json::string(name),
            json::string(current),
            json::string(new)
        );
        self.call("ChangePassword", Secret::new(params))
    }

    /// A new password without the current one: by an administrator (`auth`:
    /// their own password) or root – the recovery for a forgotten one.
    pub fn reset_password(
        &mut self,
        name: &str,
        new: &str,
        auth: Option<&str>,
    ) -> Result<(), String> {
        let mut params = format!(
            "\"name\":{},\"new\":{}",
            json::string(name),
            json::string(new)
        );
        push_auth(&mut params, auth);
        self.call("ResetPassword", Secret::new(params))
    }

    /// Removes the account and its home (root only).
    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let params = format!("\"name\":{}", json::string(name));
        self.call("Remove", Secret::new(params))
    }

    /// Calls `method` with the parameters (the inside of a JSON object);
    /// the error as a sentence.
    fn call(&mut self, method: &str, params: Secret) -> Result<(), String> {
        let request = Secret::new(format!(
            "{{\"method\":\"{}.{method}\",\"parameters\":{{{}}}}}",
            protocol::INTERFACE,
            params.expose()
        ));
        let broken = |e: std::io::Error| format!("The account service failed ({e}).");
        protocol::write_message(&mut self.stream, request.expose()).map_err(broken)?;
        let reply = protocol::read_message(&mut self.stream)
            .map_err(broken)?
            .ok_or("The account service ended the connection.")?;
        let reply = json::Json::parse(reply.expose(), false)?;
        let object = reply.object();
        if let Some(error) = object.str("error") {
            let reason = object
                .object("parameters")
                .and_then(|p| p.str("reason"))
                .unwrap_or("");
            return Err(explain(error, reason));
        }
        Ok(())
    }
}

fn push_auth(params: &mut String, auth: Option<&str>) {
    if let Some(auth) = auth {
        params.push_str(&format!(",\"auth\":{}", json::string(auth)));
    }
}

/// The service's error as a sentence.
fn explain(error: &str, reason: &str) -> String {
    match error
        .strip_prefix(protocol::INTERFACE)
        .and_then(|e| e.strip_prefix('.'))
    {
        Some(error::BAD_PASSWORD) => "That password is not right.".into(),
        Some(error::NO_SUCH_ACCOUNT) => "There is no such account.".into(),
        Some(error::ACCOUNT_EXISTS) => "That name exists already.".into(),
        Some(error::NOT_ALLOWED) => "You are not allowed to do that.".into(),
        Some(error::INVALID) => format!("Not possible: {reason}."),
        _ if reason.is_empty() => format!("The account service failed ({error})."),
        _ => format!("The account service failed: {reason}."),
    }
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

    #[test]
    fn explains_the_services_errors() {
        assert_eq!(
            explain("io.beize.Accounts.BadPassword", ""),
            "That password is not right."
        );
        assert_eq!(
            explain("io.beize.Accounts.Invalid", "at least 8 characters, please"),
            "Not possible: at least 8 characters, please."
        );
        assert_eq!(
            explain("org.varlink.service.MethodNotFound", ""),
            "The account service failed (org.varlink.service.MethodNotFound)."
        );
    }
}
