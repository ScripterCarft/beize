//! Text that must not outlive its use: passwords, the recovery key and the
//! JSON that carries them. Overwritten with zeros when dropped.

use std::fmt;
use std::sync::atomic::{Ordering, compiler_fence};

pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(text)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zeros are valid UTF-8; the string is dropped right after.
        wipe(unsafe { self.0.as_bytes_mut() });
    }
}

/// Overwrites `bytes` with zeros in a way the compiler keeps, although the
/// memory is usually freed right after (a plain `fill(0)` may be dropped).
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: a valid, exclusively borrowed byte.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Never printed, not even by accident in an error message.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_not_printed() {
        let secret = Secret::new("hunter22".into());
        assert_eq!(secret.expose(), "hunter22");
        assert_eq!(format!("{secret:?}"), "Secret(…)");
    }
}
