//! `beize-updated` – Beize's updater (ADR 0020), the one way the system
//! changes, like softwareupdated on the Mac. One process per request
//! (beize-update.socket, Accept=yes, root only); it has no network.
//!
//! `Install` source → version:
//! 1. beize-update-fetch fetches the release into its cache, checking it;
//! 2. the updater copies it into its own directory and checks the copy
//!    again – signature, newer version, every sum – so nothing the fetcher
//!    could still change is ever written;
//! 3. systemd-sysupdate writes /usr and its hash tree into the free slot and
//!    the UKI, with three boot attempts, onto the ESP. The new version
//!    starts with the next boot; if it fails three times, systemd-boot
//!    starts the previous one again.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use beize_update::list::{MAX_LIST, MAX_SIGNATURE};
use beize_update::{
    CACHE, FETCH_INTERFACE, FETCH_SOCKET, INTERFACE, LIST, Refused, SIGNATURE, VERIFIED, call,
    check_list, crypto, error, read_limited, version_reply,
};
use beize_varlink::json::{self, Object};
use sha2::{Digest, Sha256};

const SYSUPDATE: &str = "/usr/lib/systemd/systemd-sysupdate";
/// One update at a time.
const LOCK: &str = "/var/lib/beize-update/lock";

fn main() -> ExitCode {
    beize_update::serve(INTERFACE, |method, params, uid| {
        if uid != 0 {
            return Err(Refused(
                error::NOT_ALLOWED,
                "only root installs updates".into(),
            ));
        }
        match method {
            "Install" => install(params),
            _ => Err(Refused::failed(format!("no method {method:?}"))),
        }
    })
}

fn install(params: Object) -> Result<String, Refused> {
    let source = params.str("source").unwrap_or("");
    let _lock = lock()?;
    call(
        FETCH_SOCKET,
        FETCH_INTERFACE,
        "Fetch",
        &format!("\"source\":{}", json::string(source)),
    )?;
    let copied = copy_checked();
    // The cache is the fetcher's; whatever happened, it empties it.
    let cleared = call(FETCH_SOCKET, FETCH_INTERFACE, "Clear", "");
    let version = copied?;
    cleared?;

    let status = Command::new(SYSUPDATE)
        .args(["update", &version])
        .status()
        .map_err(|e| Refused::failed(format!("{SYSUPDATE}: {e}")));
    let _ = fs::remove_dir_all(VERIFIED);
    let status = status?;
    if !status.success() {
        return Err(Refused::failed(format!(
            "systemd-sysupdate could not install {version} ({status})"
        )));
    }
    eprintln!("installed {version}; it starts with the next boot");
    Ok(version_reply(&version))
}

/// Copies the fetched release into [`VERIFIED`] (only root's) and checks the
/// copy: the list's signature and version, then every file's sum.
fn copy_checked() -> Result<String, Refused> {
    let verified = Path::new(VERIFIED);
    if verified.exists() {
        fs::remove_dir_all(verified).map_err(|e| Refused::failed(format!("{VERIFIED}: {e}")))?;
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(verified)
        .map_err(|e| Refused::failed(format!("{VERIFIED}: {e}")))?;
    let cache = Path::new(CACHE);
    let small = |name: &str, max: u64| -> Result<Vec<u8>, Refused> {
        let file =
            File::open(cache.join(name)).map_err(|e| Refused::failed(format!("{name}: {e}")))?;
        let data = read_limited(file, max, name).map_err(|e| Refused(error::UNTRUSTED, e))?;
        fs::write(verified.join(name), &data)
            .map_err(|e| Refused::failed(format!("{name}: {e}")))?;
        Ok(data)
    };
    let list = small(LIST, MAX_LIST)?;
    let signature = small(SIGNATURE, MAX_SIGNATURE)?;
    let release = check_list(&list, &signature)?;
    for entry in &release.entries {
        let from = File::open(cache.join(&entry.name))
            .map_err(|e| Refused::failed(format!("{}: {e}", entry.name)))?;
        let mut to = File::create(verified.join(&entry.name))
            .map_err(|e| Refused::failed(format!("{}: {e}", entry.name)))?;
        let sum = copy_hashed(from, &mut to, entry.kind.max_size())
            .map_err(|e| Refused::failed(format!("{}: {e}", entry.name)))?;
        if sum.as_deref() != Some(entry.sha256.as_str()) {
            return Err(Refused(
                error::UNTRUSTED,
                format!("{} does not match the list", entry.name),
            ));
        }
    }
    Ok(release.version)
}

/// Copies at most `max` bytes and returns the SHA-256 of what was written;
/// `None` if there was more.
fn copy_hashed(from: File, to: &mut File, max: u64) -> std::io::Result<Option<String>> {
    let mut digest = Sha256::new();
    let mut reader = from.take(max + 1);
    let mut buffer = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max {
            return Ok(None);
        }
        to.write_all(&buffer[..read])?;
        digest.update(&buffer[..read]);
    }
    to.sync_all()?;
    Ok(Some(crypto::hex(digest)))
}

/// An exclusive lock on [`LOCK`], released when the file is closed.
fn lock() -> Result<File, Refused> {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let file = File::create(LOCK).map_err(|e| Refused::failed(format!("{LOCK}: {e}")))?;
    // SAFETY: a valid, open file descriptor.
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
        return Err(Refused::failed("another update is being installed"));
    }
    Ok(file)
}
