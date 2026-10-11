//! The update key's signature, checked with OpenSSL's libcrypto (in the
//! image anyway): ML-DSA-65 over the whole list (ADR 0020). Only a key of
//! exactly that kind is accepted, so no other algorithm can be slipped in.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use sha2::{Digest, Sha256};

/// The only algorithm an update key may have.
const ALGORITHM: &std::ffi::CStr = c"ML-DSA-65";

#[link(name = "crypto")]
unsafe extern "C" {
    fn BIO_new_mem_buf(buf: *const c_void, len: c_int) -> *mut c_void;
    fn BIO_free(bio: *mut c_void) -> c_int;
    fn PEM_read_bio_PUBKEY(
        bio: *mut c_void,
        out: *mut *mut c_void,
        callback: *mut c_void,
        user: *mut c_void,
    ) -> *mut c_void;
    fn EVP_PKEY_free(key: *mut c_void);
    fn EVP_PKEY_is_a(key: *const c_void, name: *const c_char) -> c_int;
    fn EVP_MD_CTX_new() -> *mut c_void;
    fn EVP_MD_CTX_free(ctx: *mut c_void);
    fn EVP_DigestVerifyInit_ex(
        ctx: *mut c_void,
        pctx: *mut *mut c_void,
        mdname: *const c_char,
        libctx: *mut c_void,
        props: *const c_char,
        key: *mut c_void,
        params: *const c_void,
    ) -> c_int;
    fn EVP_DigestVerify(
        ctx: *mut c_void,
        signature: *const u8,
        signature_len: usize,
        data: *const u8,
        data_len: usize,
    ) -> c_int;
}

/// Whether `signature` is the update key's (`public_pem`) signature of `data`.
pub fn verify(public_pem: &[u8], data: &[u8], signature: &[u8]) -> Result<bool, String> {
    let len = c_int::try_from(public_pem.len()).map_err(|_| "the update key is too large")?;
    // SAFETY: each object is checked for null after creation and freed
    // exactly once below; buffers outlive the calls that read them.
    unsafe {
        let bio = BIO_new_mem_buf(public_pem.as_ptr().cast(), len);
        if bio.is_null() {
            return Err("out of memory".into());
        }
        let key = PEM_read_bio_PUBKEY(bio, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        BIO_free(bio);
        if key.is_null() {
            return Err("the update key is not a public key".into());
        }
        if EVP_PKEY_is_a(key, ALGORITHM.as_ptr()) != 1 {
            EVP_PKEY_free(key);
            return Err("the update key is not an ML-DSA-65 key".into());
        }
        let ctx = EVP_MD_CTX_new();
        if ctx.is_null() {
            EVP_PKEY_free(key);
            return Err("out of memory".into());
        }
        // ML-DSA signs the message itself: no separate digest (mdname null).
        let valid = EVP_DigestVerifyInit_ex(
            ctx,
            ptr::null_mut(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            key,
            ptr::null(),
        ) == 1
            && EVP_DigestVerify(
                ctx,
                signature.as_ptr(),
                signature.len(),
                data.as_ptr(),
                data.len(),
            ) == 1;
        EVP_MD_CTX_free(ctx);
        EVP_PKEY_free(key);
        Ok(valid)
    }
}

/// SHA-256 as 64 lowercase hex digits, as the list writes it.
pub fn hex(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A key pair and a signature made with the openssl command, as bz does.
    fn signed(algorithm: &str, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "beize-update-test-{}-{algorithm}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("openssl")
                .args(args)
                .current_dir(&dir)
                .status()
                .unwrap();
            assert!(status.success(), "openssl {args:?}");
        };
        std::fs::write(dir.join("data"), data).unwrap();
        run(&["genpkey", "-algorithm", algorithm, "-out", "key"]);
        run(&["pkey", "-in", "key", "-pubout", "-out", "pub"]);
        run(&[
            "pkeyutl", "-sign", "-rawin", "-inkey", "key", "-in", "data", "-out", "sig",
        ]);
        let result = (
            std::fs::read(dir.join("pub")).unwrap(),
            std::fs::read(dir.join("sig")).unwrap(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        result
    }

    #[test]
    fn accepts_only_the_signature_of_the_key() {
        let (public, signature) = signed("ML-DSA-65", b"list\n");
        assert_eq!(verify(&public, b"list\n", &signature), Ok(true));
        assert_eq!(verify(&public, b"list!\n", &signature), Ok(false));
        let mut broken = signature.clone();
        broken[10] ^= 1;
        assert_eq!(verify(&public, b"list\n", &broken), Ok(false));
        let (other, _) = signed("ML-DSA-65", b"list\n");
        assert_eq!(verify(&other, b"list\n", &signature), Ok(false));
        assert!(verify(b"not a key", b"list\n", &signature).is_err());
    }

    #[test]
    fn refuses_other_algorithms() {
        let (public, signature) = signed("ED25519", b"list\n");
        assert!(verify(&public, b"list\n", &signature).is_err());
    }

    #[test]
    fn writes_sha256_as_hex() {
        let mut digest = Sha256::new();
        digest.update(b"abc");
        assert_eq!(
            hex(digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
