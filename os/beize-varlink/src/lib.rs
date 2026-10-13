//! Varlink (https://varlink.org) for Beize's own services: systemd's IPC –
//! JSON messages, each ended by a NUL byte, over a Unix socket. Every service
//! runs one process per connection (socket activation with Accept=yes).
//!
//! Shared by the account service (ADR 0019) and the updater (ADR 0020).

pub mod json;
mod secret;

use std::io::{self, Read, Write};

pub use secret::{Secret, wipe};

/// Reads one message of at most `max` bytes; `None` at the end of the
/// connection. A message may carry secrets: it is a [`Secret`], and what is
/// read of a refused one is erased.
pub fn read_message(stream: &mut impl Read, max: usize) -> io::Result<Option<Secret>> {
    // Room for the longest message at once: growing the buffer would leave
    // copies of its start in freed memory.
    let mut message = Vec::with_capacity(max);
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return if message.is_empty() {
                Ok(None)
            } else {
                wipe(&mut message);
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "message cut off",
                ))
            };
        }
        if byte[0] == 0 {
            break;
        }
        if message.len() == max {
            wipe(&mut message);
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
            wipe(&mut error.into_bytes());
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

/// A method call: `{"method":"<interface>.<method>","parameters":{<params>}}`,
/// `params` already JSON (without braces).
pub fn call_message(interface: &str, method: &str, params: &str) -> Secret {
    Secret::new(format!(
        "{{\"method\":\"{interface}.{method}\",\"parameters\":{{{params}}}}}"
    ))
}

/// An error reply: `<interface>.<error>`, with `reason` for the person.
pub fn error_message(interface: &str, error: &str, reason: &str) -> String {
    format!(
        "{{\"error\":\"{interface}.{error}\",\"parameters\":{{\"reason\":{}}}}}",
        json::string(reason)
    )
}

/// The user ID of the process at the other end of `stream`, from the kernel
/// (SO_PEERCRED): no client can claim to be someone else.
pub fn peer_uid(stream: &std::os::unix::net::UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    use std::os::raw::{c_int, c_void};

    const SOL_SOCKET: c_int = 1;
    const SO_PEERCRED: c_int = 17;
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    unsafe extern "C" {
        fn getsockopt(
            fd: c_int,
            level: c_int,
            name: c_int,
            value: *mut c_void,
            len: *mut u32,
        ) -> c_int;
    }
    let mut cred = Ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: a valid socket and an out buffer of the given length.
    let r = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
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
            read_message(&mut reader, 64).unwrap().unwrap().expose(),
            "{\"a\":1}"
        );
        assert_eq!(
            read_message(&mut reader, 64).unwrap().unwrap().expose(),
            "{}"
        );
        assert!(read_message(&mut reader, 64).unwrap().is_none());
        assert!(read_message(&mut &b"{\"cut"[..], 64).is_err());
        assert!(read_message(&mut &b"{\"long\"}\0"[..], 4).is_err());
    }

    #[test]
    fn writes_calls_and_errors() {
        assert_eq!(
            call_message("io.beize.X", "Do", "\"a\":1").expose(),
            "{\"method\":\"io.beize.X.Do\",\"parameters\":{\"a\":1}}"
        );
        let error = error_message("io.beize.X", "Failed", "a \"b\"");
        let json = json::Json::parse(&error, false).unwrap();
        assert_eq!(json.object().str("error"), Some("io.beize.X.Failed"));
        assert_eq!(
            json.object()
                .object("parameters")
                .and_then(|p| p.str("reason")),
            Some("a \"b\"")
        );
    }
}
