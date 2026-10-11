//! The passphrase cache of systemd-cryptsetup in the kernel keyring.
//!
//! systemd-cryptsetup stores a typed passphrase (here: the recovery key) as
//! a "user" key named "cryptsetup" in root's user keyring for 2.5 minutes,
//! NUL-separated if there are several. Reading it needs a service with
//! `KeyringMode=shared`, like systemd-cryptsetup@.service itself.

use std::ffi::{CString, c_char, c_long};

// x86_64 system call numbers; glibc has no wrappers for these.
const SYS_REQUEST_KEY: c_long = 249;
const SYS_KEYCTL: c_long = 250;
const KEYCTL_READ: c_long = 11;

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}

/// The passphrases cached under `name`: every one typed, a mistyped one too.
pub fn cached_passphrases(name: &str) -> Vec<String> {
    read_key(name).unwrap_or_default()
}

fn read_key(name: &str) -> Option<Vec<String>> {
    let kind = CString::new("user").ok()?;
    let description = CString::new(name).ok()?;
    // SAFETY: both strings are NUL-terminated and outlive the call; no
    // callout and no destination keyring are passed.
    let serial = unsafe {
        syscall(
            SYS_REQUEST_KEY,
            kind.as_ptr(),
            description.as_ptr(),
            std::ptr::null::<c_char>(),
            0 as c_long,
        )
    };
    if serial < 0 {
        return None;
    }
    let mut buffer = vec![0u8; 4096];
    // SAFETY: the buffer is valid for writes of its length; the kernel writes
    // at most that many bytes and returns the full payload length.
    let length = unsafe {
        syscall(
            SYS_KEYCTL,
            KEYCTL_READ,
            serial,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    let length = usize::try_from(length).ok()?.min(buffer.len());
    buffer.truncate(length);
    let passphrases = buffer
        .split(|&b| b == 0)
        .filter(|passphrase| !passphrase.is_empty())
        .filter_map(|passphrase| String::from_utf8(passphrase.to_vec()).ok())
        .collect();
    buffer.fill(0);
    Some(passphrases)
}
