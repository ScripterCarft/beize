//! The network as the menu bar and the control center show it (ADR 0028
//! §5): networkd's links over Varlink (`io.systemd.Network.Describe`, which
//! anyone may call), asked again whenever networkd writes its state anew –
//! watched as sd-network's monitor watches it (inotify on its state files;
//! sd-network itself is not in libsystemd's public API). Wi-Fi comes with
//! iwd (#17); until then networkd knows wired links only.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use beize_varlink::json::Json;
use rustix::fs::inotify::{self, CreateFlags, WatchFlags};

const SOCKET: &str = "/run/systemd/netif/io.systemd.Network";
/// The longest answer read: a few links with their addresses and routes.
const MAX_ANSWER: usize = 1 << 20;

/// What kind of link the computer is online through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ethernet,
    WiFi,
}

/// The network now: the link it is online through, if any – else the one
/// that comes closest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub kind: Option<Kind>,
    /// Online: the link is routable.
    pub connected: bool,
}

impl Status {
    /// Asks networkd; offline if it cannot be asked.
    pub fn now() -> Status {
        match describe() {
            Ok(status) => status,
            Err(e) => {
                tracing::warn!("asking networkd: {e}");
                Status {
                    kind: None,
                    connected: false,
                }
            }
        }
    }
}

/// Tells when networkd wrote its state anew: its fd becomes readable, then
/// `take` what came.
pub struct Monitor(OwnedFd);

impl Monitor {
    /// Watches what sd-network's monitor watches (sd-network.c): the links'
    /// files, which networkd replaces by renaming, and its overall state.
    pub fn new() -> Result<Monitor, String> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
            .map_err(|e| e.to_string())?;
        for (dir, flags) in [
            (
                "/run/systemd/netif/links/",
                WatchFlags::MOVED_TO | WatchFlags::DELETE,
            ),
            ("/run/systemd/netif/", WatchFlags::MOVED_TO),
        ] {
            inotify::add_watch(&fd, dir, flags).map_err(|e| format!("{dir}: {e}"))?;
        }
        Ok(Monitor(fd))
    }

    /// Readable when something changed.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    /// Takes what came, so the fd waits again.
    pub fn take(&self) {
        let mut buffer = [0u8; 4096];
        while matches!(rustix::io::read(&self.0, &mut buffer), Ok(n) if n > 0) {}
    }
}

fn describe() -> Result<Status, String> {
    let mut stream = UnixStream::connect(SOCKET).map_err(|e| format!("{SOCKET}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let call = beize_varlink::call_message("io.systemd.Network", "Describe", "");
    beize_varlink::write_message(&mut stream, call.expose()).map_err(|e| e.to_string())?;
    let answer = beize_varlink::read_message(&mut stream, MAX_ANSWER)
        .map_err(|e| e.to_string())?
        .ok_or("no answer")?;
    let json = Json::parse(answer.expose(), false)?;
    let reply = json.object();
    if let Some(error) = reply.str("error") {
        return Err(error.into());
    }
    let parameters = reply.object("parameters").ok_or("no parameters")?;
    let mut best: Option<(u8, Kind)> = None;
    for link in parameters.objects("Interfaces") {
        // Virtual links – bridges, veth, tunnels – name their kind: they
        // are no way online of their own (docker0 is routable without an
        // uplink).
        if link.str("Kind").is_some() {
            continue;
        }
        let kind = match link.str("Type") {
            Some("ether") => Kind::Ethernet,
            Some("wlan") => Kind::WiFi,
            _ => continue,
        };
        // How far the link is: routable is online.
        let rank = match link.str("OperationalState") {
            Some("routable") => 3,
            Some("degraded") => 2,
            Some("carrier" | "degraded-carrier") => 1,
            _ => 0,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, kind));
        }
    }
    Ok(Status {
        kind: best.map(|(_, kind)| kind),
        connected: best.is_some_and(|(rank, _)| rank == 3),
    })
}
