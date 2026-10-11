//! root in /etc/shadow. People's accounts are made through beize-accounts
//! (ADR 0019); root stays a classic system account.

/// `shadow` with root locked: nobody logs in as root (ADR 0018). The owner
/// gets root with run0 and their own password; the emergency gate and the
/// recovery ask for the recovery key instead (ADR 0016). Locked even if root
/// had a password before, like macOS's disabled root and Fedora's with an
/// administrator account.
pub fn lock_root(shadow: &str) -> Result<String, String> {
    let mut found_root = false;
    let mut out = String::new();
    for line in shadow.lines() {
        let mut fields: Vec<String> = line.split(':').map(str::to_owned).collect();
        if fields.first().map(String::as_str) == Some("root") && fields.len() >= 3 {
            found_root = true;
            fields[1] = LOCKED.to_owned();
        }
        out.push_str(&fields.join(":"));
        out.push('\n');
    }
    if !found_root {
        return Err("root has no entry in /etc/shadow".into());
    }
    Ok(out)
}

/// A password field no password matches: "!" locks, "*" matches no hash.
const LOCKED: &str = "!*";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_root_and_nothing_else() {
        let shadow = lock_root("root:!*:20000::::::\nsystemd-network:!*:::::::\n").unwrap();
        assert_eq!(shadow, "root:!*:20000::::::\nsystemd-network:!*:::::::\n");
        // A root password from before is locked, too.
        let had = lock_root("root:$y$j9T$old$hash:19000::::::\nbin:x:::::::\n").unwrap();
        assert_eq!(had, "root:!*:19000::::::\nbin:x:::::::\n");
    }

    #[test]
    fn refuses_a_shadow_without_root() {
        assert!(lock_root("bin:!*:::::::\n").is_err());
    }
}
