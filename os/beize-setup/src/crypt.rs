//! Password hashing with libxcrypt: yescrypt, its recommended method.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong};

const CRYPT_OUTPUT_SIZE: usize = 384;
const CRYPT_GENSALT_OUTPUT_SIZE: usize = 192;

#[link(name = "crypt")]
unsafe extern "C" {
    fn crypt_gensalt_rn(
        prefix: *const c_char,
        count: c_ulong,
        rbytes: *const c_char,
        nrbytes: c_int,
        output: *mut c_char,
        output_size: c_int,
    ) -> *mut c_char;
    fn crypt_rn(
        phrase: *const c_char,
        setting: *const c_char,
        data: *mut u8,
        size: c_int,
    ) -> *mut c_char;
}

/// A yescrypt hash of `password` with a fresh random salt, as /etc/shadow
/// stores it (`$y$...`).
pub fn hash(password: &str) -> Result<String, String> {
    let phrase = CString::new(password).map_err(|_| "the password contains a NUL byte")?;
    let prefix = c"$y$";
    let mut setting = [0 as c_char; CRYPT_GENSALT_OUTPUT_SIZE];
    // SAFETY: valid NUL-terminated prefix, no caller-supplied random bytes
    // (libxcrypt reads them from the kernel), output buffer of the given size.
    let salted = unsafe {
        crypt_gensalt_rn(
            prefix.as_ptr(),
            0,
            std::ptr::null(),
            0,
            setting.as_mut_ptr(),
            CRYPT_GENSALT_OUTPUT_SIZE as c_int,
        )
    };
    if salted.is_null() {
        return Err("libxcrypt could not create a salt".into());
    }
    // struct crypt_data is CRYPT_DATA_INTERNAL_SIZE + CRYPT_OUTPUT_SIZE + ...:
    // 32 KiB is far more than libxcrypt's (stable) layout needs.
    let mut data = vec![0u8; 32 * 1024];
    // SAFETY: phrase and setting are NUL-terminated; data is a zeroed buffer
    // of the given size, which crypt_rn requires.
    let result = unsafe {
        crypt_rn(
            phrase.as_ptr(),
            setting.as_ptr(),
            data.as_mut_ptr(),
            data.len() as c_int,
        )
    };
    if result.is_null() {
        return Err("libxcrypt could not hash the password".into());
    }
    // SAFETY: on success crypt_rn returns a NUL-terminated string inside data.
    let hash = unsafe { CStr::from_ptr(result) }
        .to_str()
        .map_err(|_| "libxcrypt returned an invalid hash")?
        .to_owned();
    data.fill(0);
    if !hash.starts_with("$y$") || hash.len() >= CRYPT_OUTPUT_SIZE {
        return Err("libxcrypt returned an unexpected hash".into());
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_with_yescrypt_and_a_fresh_salt() {
        let a = hash("correct horse battery staple").unwrap();
        let b = hash("correct horse battery staple").unwrap();
        assert!(a.starts_with("$y$"), "{a}");
        assert_ne!(a, b, "the same password must get different salts");
        assert!(hash("nul\0byte").is_err());
    }
}
