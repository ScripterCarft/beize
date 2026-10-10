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
    fn crypt_free(cd: *mut c_void);
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
