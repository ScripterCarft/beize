//! The interface of Beize's account service, io.beize.Accounts: Varlink
//! (https://varlink.org), systemd's own IPC – JSON messages, each ended by a
//! NUL byte, over a Unix socket. One connection, one service process
//! (socket activation with Accept=yes).
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
    let mut message = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return if message.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "message cut off",
                ))
            };
        }
        if byte[0] == 0 {
            break;
        }
        if message.len() == MAX_MESSAGE {
            message.fill(0);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too long",
            ));
        }
        message.push(byte[0]);
    }
    match String::from_utf8(message) {
        Ok(text) => Ok(Some(Secret::new(text))),
        Err(error) => {
            let mut bytes = error.into_bytes();
            bytes.fill(0);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message not UTF-8",
            ))
        }
    }
}

/// Writes one message.
pub fn write_message(stream: &mut impl Write, text: &str) -> io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.write_all(&[0])?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_end_with_a_nul_byte() {
        let mut wire = Vec::new();
        write_message(&mut wire, "{\"a\":1}").unwrap();
        write_message(&mut wire, "{}").unwrap();
        assert_eq!(wire, b"{\"a\":1}\0{}\0");
        let mut reader = &wire[..];
        assert_eq!(
            read_message(&mut reader).unwrap().unwrap().expose(),
            "{\"a\":1}"
        );
        assert_eq!(read_message(&mut reader).unwrap().unwrap().expose(), "{}");
        assert!(read_message(&mut reader).unwrap().is_none());
        assert!(read_message(&mut &b"{\"cut"[..]).is_err());
    }
}
