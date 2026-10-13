//! Password hashing with libxcrypt: yescrypt, its recommended method.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong};

use beize_varlink::wipe;

const CRYPT_OUTPUT_SIZE: usize = 384;
/// sizeof(struct crypt_data): libxcrypt fixes it at exactly 32 KiB.
const CRYPT_DATA_SIZE: usize = 32 * 1024;
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
    // SAFETY: crypt_gensalt_rn wrote a NUL-terminated setting into the buffer.
    let setting = unsafe { CStr::from_ptr(salted) };
    let phrase = CString::new(password).map_err(|_| "the password contains a NUL byte")?;
    let hash = crypt(phrase, setting).ok_or("libxcrypt could not hash the password")?;
    let hash = String::from_utf8(hash).map_err(|_| "libxcrypt returned an invalid hash")?;
    if !hash.starts_with("$y$") || hash.len() >= CRYPT_OUTPUT_SIZE {
        return Err("libxcrypt returned an unexpected hash".into());
    }
    Ok(hash)
}

/// Whether `password` matches `hash` (any method libxcrypt knows; a locked
/// field such as "!*" matches nothing). Compared in constant time.
pub fn verify(password: &str, hash: &str) -> bool {
    let (Ok(phrase), Ok(setting)) = (CString::new(password), CString::new(hash)) else {
        return false;
    };
    crypt(phrase, &setting).is_some_and(|result| same(&result, hash.as_bytes()))
}

/// crypt_rn: the hash of `phrase` with `setting`, or None. The passphrase
/// and libxcrypt's working memory are wiped afterwards.
fn crypt(phrase: CString, setting: &CStr) -> Option<Vec<u8>> {
    let mut data = vec![0u8; CRYPT_DATA_SIZE];
    // SAFETY: phrase and setting are NUL-terminated; data is a zeroed buffer
    // of sizeof(struct crypt_data), which crypt_rn requires.
    let result = unsafe {
        crypt_rn(
            phrase.as_ptr(),
            setting.as_ptr(),
            data.as_mut_ptr(),
            CRYPT_DATA_SIZE as c_int,
        )
    };
    // SAFETY: on success crypt_rn returns a NUL-terminated string in data.
    let hash = (!result.is_null()).then(|| unsafe { CStr::from_ptr(result) }.to_bytes().to_vec());
    wipe(&mut data);
    wipe(&mut phrase.into_bytes());
    hash
}

/// Equal bytes, compared without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_against_the_hash() {
        let hash = hash("correct horse").unwrap();
        assert!(verify("correct horse", &hash));
        assert!(!verify("correct horsf", &hash));
        assert!(!verify("", &hash));
        assert!(!verify("anything", "!*"), "a locked field matches nothing");
        assert!(!verify("anything", ""));
    }

    #[test]
    fn hashes_with_yescrypt_and_a_fresh_salt() {
        let a = hash("correct horse battery staple").unwrap();
        let b = hash("correct horse battery staple").unwrap();
        assert!(a.starts_with("$y$"), "{a}");
        assert_ne!(a, b, "the same password must get different salts");
        assert!(hash("nul\0byte").is_err());
    }
}
