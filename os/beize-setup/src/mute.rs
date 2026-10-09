//! Keeps the console quiet while the setup asks questions on it: no kernel
//! messages and no "[  OK  ] Started …" lines in the middle of a prompt.
//!
//! systemd-mute-console mutes both for as long as a varlink connection that
//! called io.systemd.MuteConsole.Mute stays open – the mechanism
//! systemd-firstboot uses (`--mute-console=yes`). Dropping the guard closes
//! the connection, and the output comes back.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const SOCKET: &str = "/run/systemd/io.systemd.MuteConsole";

/// Console muted while this lives.
pub struct Muted(#[allow(dead_code)] UnixStream);

/// Mutes the console and returns once it is muted. Without the service (e.g.
/// in a container) the console stays as it is: the setup still works, just
/// less tidy.
pub fn console() -> Option<Muted> {
    let mut stream = UnixStream::connect(SOCKET).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    // Varlink: one JSON object per message, terminated by a NUL byte. "more"
    // keeps the call – and with it the muting – open.
    stream
        .write_all(b"{\"method\":\"io.systemd.MuteConsole.Mute\",\"more\":true}\0")
        .ok()?;
    // The first reply says the console is muted now.
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).ok()?;
        if byte[0] == 0 {
            break;
        }
    }
    Some(Muted(stream))
}
