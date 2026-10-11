//! The interface of Beize's account service, io.beize.Accounts, over Varlink
//! (`beize-varlink`).
//!
//! Methods (parameters → reply):
//! - `Create` name, realName, password, administrator, [auth] → uid
//! - `ChangePassword` name, current, new
//! - `ResetPassword` name, new, [auth]
//! - `Remove` name (root only)
//!
//! `auth` is the caller's own password, where an administrator acts on
//! another account; root (the setup, the recovery) needs none.

use std::io::{self, Read, Write};

use crate::Secret;

/// Where the service listens (beize-accounts.socket).
pub const SOCKET: &str = "/run/beize-accounts/io.beize.Accounts";
pub const INTERFACE: &str = "io.beize.Accounts";

/// Errors, as `io.beize.Accounts.<name>`.
pub mod error {
    /// The proof – a current password, the caller's own – is wrong.
    pub const BAD_PASSWORD: &str = "BadPassword";
    pub const NO_SUCH_ACCOUNT: &str = "NoSuchAccount";
    pub const ACCOUNT_EXISTS: &str = "AccountExists";
    pub const NOT_ALLOWED: &str = "NotAllowed";
    /// A name or password the rules refuse; the reply's `reason` says why.
    pub const INVALID: &str = "Invalid";
    /// Anything else; `reason` says what.
    pub const FAILED: &str = "Failed";
}

/// Larger messages are refused: a request carries a few names and passwords.
const MAX_MESSAGE: usize = 64 * 1024;

/// Reads one message; `None` at the end of the connection.
pub fn read_message(stream: &mut impl Read) -> io::Result<Option<Secret>> {
    beize_varlink::read_message(stream, MAX_MESSAGE)
}

/// Writes one message.
pub fn write_message(stream: &mut impl Write, text: &str) -> io::Result<()> {
    beize_varlink::write_message(stream, text)
}
