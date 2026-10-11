//! `beize-update`: how Beize updates itself (ADR 0020). Like
//! softwareupdated on the Mac, one service decides: `beize-updated` installs
//! a release only if the update key signed its list, it is newer than the
//! running version and every file matches the list; systemd-sysupdate then
//! writes it into the free `/usr` slot and puts its UKI on the ESP.
//!
//! Fetching is a service of its own, `beize-update-fetch`: without
//! privileges, with only the network and a cache. It checks the list too, so
//! a wrong source cannot make it download gigabytes; `beize-updated` checks
//! its own copy again before anything is written.

pub mod crypto;
pub mod list;

use std::fs;
use std::io::Read;
use std::os::unix::net::UnixStream;

use beize_varlink::json::{self, Json};

/// The public update key; the image of each key set carries its own.
pub const KEY: &str = "/usr/lib/beize/update.pub.pem";
pub const OS_RELEASE: &str = "/usr/lib/os-release";
/// A release's list and the update key's signature of it.
pub const LIST: &str = "SHA256SUMS";
pub const SIGNATURE: &str = "SHA256SUMS.sig";
/// Where the fetcher puts a release (its CacheDirectory).
pub const CACHE: &str = "/var/cache/beize-update";
/// Where `beize-updated` keeps its checked copy: the only source
/// systemd-sysupdate installs from (bz's `layout::UPDATE_SOURCE_DIR`).
pub const VERIFIED: &str = "/var/lib/beize-update/verified";

/// The updater: `Install` source → version. Root only.
pub const SOCKET: &str = "/run/beize-update/io.beize.Update";
pub const INTERFACE: &str = "io.beize.Update";
/// The fetcher: `Fetch` source → version, `Clear`. Only for the updater.
pub const FETCH_SOCKET: &str = "/run/beize-update-fetch/io.beize.UpdateFetch";
pub const FETCH_INTERFACE: &str = "io.beize.UpdateFetch";

/// Errors of both interfaces, as `<interface>.<name>`.
pub mod error {
    pub const NOT_ALLOWED: &str = "NotAllowed";
    /// The source has no release newer than the running version.
    pub const NO_UPDATE: &str = "NoUpdate";
    /// The release is not signed with the update key, or not what its list
    /// says; `reason` says what.
    pub const UNTRUSTED: &str = "Untrusted";
    /// Anything else; `reason` says what.
    pub const FAILED: &str = "Failed";
}

/// Requests carry a source and replies a version or a reason.
const MAX_MESSAGE: usize = 16 * 1024;

/// The running version, from `/usr/lib/os-release`.
pub fn running_version() -> Result<String, String> {
    let text = fs::read_to_string(OS_RELEASE).map_err(|e| format!("{OS_RELEASE}: {e}"))?;
    list::image_version(&text)
        .filter(|v| list::valid_version(v))
        .map(str::to_owned)
        .ok_or_else(|| format!("{OS_RELEASE} has no valid IMAGE_VERSION"))
}

/// Reads at most `max` bytes of `reader`; more is an error.
pub fn read_limited(reader: impl Read, max: u64, what: &str) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(max + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("reading {what}: {e}"))?;
    if data.len() as u64 > max {
        return Err(format!("{what} is larger than {max} bytes"));
    }
    Ok(data)
}

/// Checks `list` against `signature` with the update key in `/usr` and
/// against the running version; the release it describes.
pub fn check_list(list: &[u8], signature: &[u8]) -> Result<list::List, Refused> {
    let key = fs::read(KEY).map_err(|e| Refused::failed(format!("{KEY}: {e}")))?;
    match crypto::verify(&key, list, signature) {
        Ok(true) => {}
        Ok(false) => {
            return Err(Refused(
                error::UNTRUSTED,
                "the list is not signed with the update key".into(),
            ));
        }
        Err(e) => return Err(Refused::failed(e)),
    }
    let text = std::str::from_utf8(list)
        .map_err(|_| Refused(error::UNTRUSTED, "the list is not text".into()))?;
    let release = list::List::parse(text).map_err(|e| Refused(error::UNTRUSTED, e))?;
    let running = running_version().map_err(Refused::failed)?;
    if !list::newer(&release.version, &running) {
        return Err(Refused(
            error::NO_UPDATE,
            format!("the source offers {}, this is {running}", release.version),
        ));
    }
    Ok(release)
}

/// A refused request: error name and the reason for the person.
#[derive(Debug)]
pub struct Refused(pub &'static str, pub String);

impl Refused {
    pub fn failed(reason: impl Into<String>) -> Self {
        Self(error::FAILED, reason.into())
    }
}

/// One call to a service: the reply's parameters, or its error.
pub fn call(socket: &str, interface: &str, method: &str, params: &str) -> Result<Json, Refused> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| Refused::failed(format!("{socket} is not reachable ({e})")))?;
    let broken = |e: std::io::Error| Refused::failed(format!("{interface} failed ({e})"));
    let request = beize_varlink::call_message(interface, method, params);
    beize_varlink::write_message(&mut stream, request.expose()).map_err(broken)?;
    let reply = beize_varlink::read_message(&mut stream, MAX_MESSAGE)
        .map_err(broken)?
        .ok_or_else(|| Refused::failed(format!("{interface} ended the connection")))?;
    let reply = Json::parse(reply.expose(), false).map_err(Refused::failed)?;
    if let Some(error) = reply.object().str("error") {
        let reason = reply
            .object()
            .object("parameters")
            .and_then(|p| p.str("reason"))
            .unwrap_or("")
            .to_owned();
        let name = [
            error::NOT_ALLOWED,
            error::NO_UPDATE,
            error::UNTRUSTED,
            error::FAILED,
        ]
        .into_iter()
        .find(|name| {
            error
                .strip_prefix(interface)
                .and_then(|e| e.strip_prefix('.'))
                == Some(name)
        })
        .unwrap_or(error::FAILED);
        let reason = if reason.is_empty() {
            error.to_owned()
        } else {
            reason
        };
        return Err(Refused(name, reason));
    }
    Ok(reply)
}

/// Serves one connection on stdin (socket activation, Accept=yes): reads
/// one request, answers it with `handle` (method, parameters, caller's
/// user ID), and ends.
pub fn serve(
    interface: &str,
    handle: impl FnOnce(&str, json::Object, u32) -> Result<String, Refused>,
) -> std::process::ExitCode {
    use std::os::fd::FromRawFd;
    // SAFETY: systemd passes the accepted connection as stdin
    // (StandardInput=socket); nothing else uses fd 0.
    let mut stream = unsafe { UnixStream::from_raw_fd(0) };
    let reply = match request(&mut stream, interface) {
        Ok((request, uid)) => {
            let object = request.object();
            let method = object
                .str("method")
                .and_then(|m| m.strip_prefix(interface))
                .and_then(|m| m.strip_prefix('.'))
                .unwrap_or("");
            match object.object("parameters") {
                Some(params) => match handle(method, params, uid) {
                    Ok(params) => format!("{{\"parameters\":{{{params}}}}}"),
                    Err(Refused(name, reason)) => {
                        eprintln!("{method}: {reason}");
                        beize_varlink::error_message(interface, name, &reason)
                    }
                },
                None => beize_varlink::error_message(interface, error::FAILED, "no parameters"),
            }
        }
        Err(reason) => {
            eprintln!("{reason}");
            beize_varlink::error_message(interface, error::FAILED, &reason)
        }
    };
    match beize_varlink::write_message(&mut stream, &reply) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

fn request(stream: &mut UnixStream, interface: &str) -> Result<(Json, u32), String> {
    let uid = beize_varlink::peer_uid(stream).map_err(|e| e.to_string())?;
    let text = beize_varlink::read_message(stream, MAX_MESSAGE)
        .map_err(|e| format!("{interface}: {e}"))?
        .ok_or("the connection ended without a request")?;
    Ok((Json::parse(text.expose(), false)?, uid))
}

/// `version` as reply parameters.
pub fn version_reply(version: &str) -> String {
    format!("\"version\":{}", json::string(version))
}
