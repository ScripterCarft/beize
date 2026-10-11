//! The account databases in /etc: passwd, group and shadow. Pure text
//! transformations, so they can be tested without touching a system.

/// First UID/GID for people (below are system users, sysusers.d).
pub const FIRST_PERSON_ID: u32 = 1000;
const LAST_PERSON_ID: u32 = 59999;

/// Whether `name` is acceptable as a login name: portable characters only,
/// starting with a letter, at most 32 characters, and not a system name.
pub fn validate_name(name: &str) -> Result<(), String> {
    const RESERVED: [&str; 8] = [
        "root", "admin", "daemon", "nobody", "bin", "sys", "wheel", "beize",
    ];
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !first_ok || !rest_ok || name.len() > 32 {
        return Err(
            "use lowercase letters, digits, - and _, starting with a letter (at most 32)".into(),
        );
    }
    if RESERVED.contains(&name) {
        return Err(format!("\"{name}\" is reserved for the system"));
    }
    Ok(())
}

/// Lines of a colon-separated database as fields.
fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split(':').collect())
}

/// Whether a user or group of this name exists in `text` (passwd or group).
pub fn exists(text: &str, name: &str) -> bool {
    records(text).any(|fields| fields[0] == name)
}

/// The lowest ID from 1000 on that neither passwd nor group uses, so the
/// owner's user and group get the same number.
pub fn free_id(passwd: &str, group: &str) -> Option<u32> {
    let used: Vec<u32> = records(passwd)
        .chain(records(group))
        .filter_map(|fields| fields.get(2)?.parse().ok())
        .collect();
    (FIRST_PERSON_ID..=LAST_PERSON_ID).find(|id| !used.contains(id))
}

/// `passwd` with the owner appended.
pub fn add_user(passwd: &str, name: &str, id: u32, full_name: &str) -> String {
    let gecos = full_name.replace([':', '\n'], " ");
    append(
        passwd,
        &format!("{name}:x:{id}:{id}:{gecos}:/home/{name}:/usr/bin/bash"),
    )
}

/// `group` with the owner's own group appended and the owner added to
/// `wheel` (administrators).
pub fn add_group(group: &str, name: &str, id: u32) -> Result<String, String> {
    let mut found_wheel = false;
    let mut out = String::new();
    for line in group.lines() {
        let mut fields: Vec<&str> = line.split(':').collect();
        if fields.first() == Some(&"wheel") && fields.len() == 4 {
            found_wheel = true;
            let members = if fields[3].is_empty() {
                name.to_owned()
            } else {
                format!("{},{name}", fields[3])
            };
            fields[3] = &members;
            out.push_str(&fields.join(":"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !found_wheel {
        return Err("the group wheel does not exist".into());
    }
    Ok(append(&out, &format!("{name}:x:{id}:")))
}

/// `shadow` with the owner's entry appended and root locked: nobody logs in
/// as root (ADR 0018). The owner gets root with run0 and their own password;
/// the emergency gate and the recovery ask for the recovery key instead
/// (ADR 0016). Locked even if root had a password before, like macOS's
/// disabled root and Fedora's with an administrator account. `days` is
/// today in days since 1970 (last password change).
pub fn add_shadow(shadow: &str, name: &str, hash: &str, days: u64) -> Result<String, String> {
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
    Ok(append(&out, &format!("{name}:{hash}:{days}:0:99999:7:::")))
}

/// A password field no password matches: "!" locks, "*" matches no hash.
const LOCKED: &str = "!*";

fn append(text: &str, line: &str) -> String {
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/usr/bin/bash\n\
                          systemd-network:x:192:192::/:/usr/bin/nologin\n\
                          nobody:x:65534:65534:Kernel Overflow User:/:/usr/bin/nologin\n";
    const GROUP: &str = "root:x:0:\nwheel:x:10:\nsystemd-network:x:192:\nnobody:x:65534:\n";
    const SHADOW: &str = "root:!*:20000::::::\nsystemd-network:!*:::::::\n";

    #[test]
    fn validates_login_names() {
        assert!(validate_name("joe").is_ok());
        assert!(validate_name("joe-2_b").is_ok());
        for bad in [
            "",
            "Joe",
            "1joe",
            "jo e",
            "jo:e",
            "root",
            "wheel",
            &"a".repeat(33),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn finds_a_free_id_shared_by_user_and_group() {
        assert_eq!(free_id(PASSWD, GROUP), Some(1000));
        let group = format!("{GROUP}docker:x:1000:\n");
        assert_eq!(free_id(PASSWD, &group), Some(1001));
    }

    #[test]
    fn adds_the_owner() {
        let passwd = add_user(PASSWD, "joe", 1000, "Joe: Owner");
        assert!(passwd.ends_with("joe:x:1000:1000:Joe  Owner:/home/joe:/usr/bin/bash\n"));
        assert!(exists(&passwd, "joe"));

        let group = add_group(GROUP, "joe", 1000).unwrap();
        assert!(group.contains("wheel:x:10:joe\n"));
        assert!(group.ends_with("joe:x:1000:\n"));
        let twice = add_group(&group, "ann", 1001).unwrap();
        assert!(twice.contains("wheel:x:10:joe,ann\n"));

        let shadow = add_shadow(SHADOW, "joe", "$y$j9T$salt$hash", 20005).unwrap();
        assert!(shadow.starts_with("root:!*:20000:"));
        // A root password from before is locked, too.
        let had = add_shadow("root:$y$j9T$old$hash:19000::::::\n", "joe", "$y$h", 20005).unwrap();
        assert!(had.starts_with("root:!*:19000:"));
        assert!(shadow.contains("systemd-network:!*:::::::\n"));
        assert!(shadow.ends_with("joe:$y$j9T$salt$hash:20005:0:99999:7:::\n"));
    }

    #[test]
    fn refuses_databases_without_root_or_wheel() {
        assert!(add_group("root:x:0:\n", "joe", 1000).is_err());
        assert!(add_shadow("bin:!*:::::::\n", "joe", "h", 1).is_err());
    }
}
