//! Checks a passphrase – the recovery key – against a LUKS2 header with
//! libcryptsetup, without activating anything (ADR 0016). libcryptsetup is
//! in the initrd and in /usr anyway: systemd loads it to unlock the root.

use std::ffi::{CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

#[link(name = "cryptsetup")]
unsafe extern "C" {
    fn crypt_init(cd: *mut *mut c_void, device: *const c_char) -> c_int;
    fn crypt_load(cd: *mut c_void, requested_type: *const c_char, params: *mut c_void) -> c_int;
    fn crypt_activate_by_passphrase(
        cd: *mut c_void,
        name: *const c_char,
        keyslot: c_int,
        passphrase: *const c_char,
        passphrase_size: usize,
        flags: u32,
    ) -> c_int;
    fn crypt_token_json_get(cd: *mut c_void, token: c_int, json: *mut *const c_char) -> c_int;
    fn crypt_token_json_set(cd: *mut c_void, token: c_int, json: *const c_char) -> c_int;
    fn crypt_free(cd: *mut c_void);
}

/// The LUKS2 header's last token (it has 32), which systemd's lowest-free
/// numbering reaches last: the recovery key was made but not yet confirmed.
const UNCONFIRMED_TOKEN: c_int = 31;
const UNCONFIRMED_TYPE: &str = "beize-recovery-unconfirmed";

/// `beize-tpm recovery-unconfirmed set|clear|check DEVICE`, on the volume
/// itself: it goes with the disk, and is gone when a new installation writes
/// a new header. `check` succeeds if the mark is there.
pub fn recovery_unconfirmed(command: Option<&str>, device: Option<&str>) -> Result<(), String> {
    let device = device.ok_or("no device")?;
    let path = CString::new(device).map_err(|_| format!("{device} is not a valid path"))?;
    let mut cd = ptr::null_mut();
    // SAFETY: `cd` is written by crypt_init; `path` is a valid C string.
    let result = unsafe { crypt_init(&mut cd, path.as_ptr()) };
    if result < 0 {
        return Err(format!("opening {device}: error {}", -result));
    }
    // SAFETY: `cd` came from crypt_init and is freed once below; the JSON
    // strings are valid for the calls, the one read is libcryptsetup's.
    let verdict = unsafe {
        let loaded = crypt_load(cd, c"LUKS2".as_ptr(), ptr::null_mut());
        if loaded < 0 {
            Err(format!(
                "reading the LUKS2 header of {device}: error {}",
                -loaded
            ))
        } else {
            match command {
                Some("set") => {
                    let json = CString::new(format!(
                        "{{\"type\":\"{UNCONFIRMED_TYPE}\",\"keyslots\":[]}}"
                    ))
                    .map_err(|e| e.to_string())?;
                    let set = crypt_token_json_set(cd, UNCONFIRMED_TOKEN, json.as_ptr());
                    if set < 0 {
                        Err(format!("marking {device}: error {}", -set))
                    } else {
                        Ok(())
                    }
                }
                Some("clear") => {
                    let cleared = crypt_token_json_set(cd, UNCONFIRMED_TOKEN, ptr::null());
                    if cleared < 0 {
                        Err(format!("clearing the mark on {device}: error {}", -cleared))
                    } else {
                        Ok(())
                    }
                }
                Some("check") => {
                    let mut json = ptr::null();
                    let got = crypt_token_json_get(cd, UNCONFIRMED_TOKEN, &mut json);
                    let marked = got >= 0
                        && !json.is_null()
                        && std::ffi::CStr::from_ptr(json)
                            .to_string_lossy()
                            .contains(UNCONFIRMED_TYPE);
                    if marked {
                        Ok(())
                    } else {
                        Err(format!("{device} has no unconfirmed recovery key"))
                    }
                }
                _ => Err("usage: beize-tpm recovery-unconfirmed set|clear|check DEVICE".into()),
            }
        }
    };
    // SAFETY: cd came from crypt_init and is not used afterwards.
    unsafe { crypt_free(cd) };
    verdict
}

const CRYPT_ANY_SLOT: c_int = -1;
/// libcryptsetup's answers for a passphrase no key slot accepts.
const EPERM: c_int = 1;
const ENOENT: c_int = 2;

/// Whether `passphrase` opens a key slot of the LUKS2 volume on `device`.
/// Nothing is activated: without a name, libcryptsetup only checks.
pub fn opens(device: &Path, passphrase: &str) -> Result<bool, String> {
    let path = CString::new(device.as_os_str().as_bytes())
        .map_err(|_| format!("{} is not a valid path", device.display()))?;
    let mut cd = ptr::null_mut();
    // SAFETY: cd is an out pointer, path a NUL-terminated string that
    // outlives the call.
    let result = unsafe { crypt_init(&mut cd, path.as_ptr()) };
    if result < 0 {
        return Err(format!("opening {}: error {}", device.display(), -result));
    }
    // SAFETY: cd came from crypt_init and is freed exactly once below; the
    // passphrase is passed with its length (no NUL needed) and not kept.
    let verdict = unsafe {
        match crypt_load(cd, c"LUKS2".as_ptr(), ptr::null_mut()) {
            loaded if loaded < 0 => Err(format!(
                "reading the LUKS2 header of {}: error {}",
                device.display(),
                -loaded
            )),
            _ => match crypt_activate_by_passphrase(
                cd,
                ptr::null(),
                CRYPT_ANY_SLOT,
                passphrase.as_ptr().cast(),
                passphrase.len(),
                0,
            ) {
                slot if slot >= 0 => Ok(true),
                error if error == -EPERM || error == -ENOENT => Ok(false),
                error => Err(format!(
                    "checking the key against {}: error {}",
                    device.display(),
                    -error
                )),
            },
        }
    };
    // SAFETY: cd came from crypt_init and is not used afterwards.
    unsafe { crypt_free(cd) };
    verdict
}
