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
/// Where the UKIs lie.
const UKI_DIR: &str = "/boot/EFI/Linux";
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

/// The vendor GUID of systemd-boot's variables, and the one set for a start
/// that counts its tries.
const COUNT_PATH: &str =
    "/sys/firmware/efi/efivars/LoaderBootCountPath-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";

/// An update replaces the version not running, which is then the last good one
/// if this one is on trial: with both bad, nothing would start. As Android's
/// A/B and ChromeOS update only from a slot marked successful. After
/// boot-complete.target systemd-bless-boot has marked this start good; the
/// variable stays (`beize_tpm::trial`).
fn refuse_on_trial() -> Result<(), Refused> {
    if fs::read(COUNT_PATH).map_or(true, |value| value.len() <= 4) {
        return Ok(());
    }
    let blessed = Command::new("systemctl")
        .args(["is-active", "--quiet", "systemd-bless-boot.service"])
        .status()
        .is_ok_and(|status| status.success());
    if blessed {
        return Ok(());
    }
    Err(Refused::failed(
        "this start has not been marked good yet: restart, or fix what failed, first",
    ))
}

fn install(params: Object) -> Result<String, Refused> {
    refuse_on_trial()?;
    let source = params.str("source").unwrap_or("");
    let _lock = lock()?;
    let fetched = call(
        FETCH_SOCKET,
        FETCH_INTERFACE,
        "Fetch",
        &format!("\"source\":{}", json::string(source)),
    );
    let copied = fetched.and_then(|_| copy_checked());
    // The cache is the fetcher's; whatever happened, it empties it.
    let cleared = call(FETCH_SOCKET, FETCH_INTERFACE, "Clear", "");
    let installed = copied.and_then(|version| {
        cleared?;
        sysupdate(&version)?;
        Ok(version)
    });
    // The copy too, whatever happened: a refused release may be gigabytes.
    let _ = fs::remove_dir_all(VERIFIED);
    let version = installed?;
    // sysupdate says yes to a version it has, also one that used up its tries.
    if !starts_next(Path::new(UKI_DIR), &version)? {
        return Err(Refused::failed(format!(
            "{version} did not start here before and has no tries left; a newer update is needed"
        )));
    }
    eprintln!("installed {version}; it starts with the next boot");
    Ok(version_reply(&version))
}

/// Whether `dir` has `version`'s UKI with tries left, or one not counting:
/// `beize_<v>.efi`, `beize_<v>+<left>[-<done>].efi` (ADR 0007).
fn starts_next(dir: &Path, version: &str) -> Result<bool, Refused> {
    let entries = fs::read_dir(dir).map_err(|e| Refused::failed(format!("{UKI_DIR}: {e}")))?;
    let prefix = format!("beize_{version}");
    Ok(entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
            return false;
        };
        let Some(rest) = rest.strip_suffix(".efi") else {
            return false;
        };
        if rest.is_empty() {
            return true;
        }
        rest.strip_prefix('+')
            .and_then(|tries| tries.split('-').next())
            .and_then(|left| left.parse::<u32>().ok())
            .is_some_and(|left| left > 0)
    }))
}

/// systemd-sysupdate writes the checked copy into the free slot and the ESP.
fn sysupdate(version: &str) -> Result<(), Refused> {
    // What an install cut short left: sysupdate refuses the same version until
    // it is gone ("Vacuum it to try installing again"). Only partial instances
    // and those beyond InstancesMax go; a failure here shows in the update.
    let cleared = Command::new(SYSUPDATE)
        .arg("vacuum")
        .status()
        .map_err(|e| Refused::failed(format!("{SYSUPDATE}: {e}")))?;
    if !cleared.success() {
        return Err(Refused::failed(format!(
            "systemd-sysupdate could not clear the slots ({cleared})"
        )));
    }
    let status = Command::new(SYSUPDATE)
        .args(["update", version])
        .status()
        .map_err(|e| Refused::failed(format!("{SYSUPDATE}: {e}")))?;
    if !status.success() {
        return Err(Refused::failed(format!(
            "systemd-sysupdate could not install {version} ({status})"
        )));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_without_tries_does_not_start() {
        let dir = std::env::temp_dir().join(format!("beize-updated-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a directory for the test");
        let has = |name: &str, version: &str| {
            let file = dir.join(name);
            fs::write(&file, "").expect("a file for the test");
            let found = starts_next(&dir, version).expect("the listing");
            fs::remove_file(&file).expect("removing it");
            found
        };
        assert!(has("beize_0.7.efi", "0.7"));
        assert!(has("beize_0.7+3-0.efi", "0.7"));
        assert!(has("beize_0.7+1.efi", "0.7"));
        assert!(!has("beize_0.7+0-3.efi", "0.7"));
        assert!(!has("beize_0.7.1+3-0.efi", "0.7"));
        fs::remove_dir(&dir).expect("removing the directory");
    }
}
