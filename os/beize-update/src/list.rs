//! The list of a release, `SHA256SUMS`: one line per file, `<sha256>  <name>`,
//! as `bz release` writes it. Read strictly: exactly the three files of one
//! version of the running variant (ADR 0025), under the names
//! systemd-sysupdate matches – anything else, a release of another variant
//! too, is refused before a single file is fetched or written.

/// The largest list and signature accepted.
pub const MAX_LIST: u64 = 64 * 1024;
pub const MAX_SIGNATURE: u64 = 16 * 1024;

/// What a file of a release is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `beize-<variant>_<version>_<uuid>.usr.erofs`
    Usr,
    /// `beize-<variant>_<version>_<uuid>.usr.verity`
    UsrVerity,
    /// `beize-<variant>_<version>.efi`
    Uki,
}

impl Kind {
    /// The largest file of this kind: what fits into its slot (bz's
    /// `layout.rs`), and a UKI far larger than any real one.
    pub fn max_size(self) -> u64 {
        const MIB: u64 = 1024 * 1024;
        match self {
            Self::Usr => 4096 * MIB,
            Self::UsrVerity => 64 * MIB,
            Self::Uki => 512 * MIB,
        }
    }
}

/// One file of a release.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// SHA-256, 64 lowercase hex digits.
    pub sha256: String,
    pub kind: Kind,
}

/// A release, as its list describes it.
#[derive(Debug)]
pub struct List {
    pub version: String,
    pub entries: Vec<Entry>,
}

impl List {
    /// The release of `variant` (`VARIANT_ID`) that `text` lists.
    pub fn parse(text: &str, variant: &str) -> Result<Self, String> {
        if !valid_variant(variant) {
            return Err(format!("not a variant: {variant:?}"));
        }
        let mut version: Option<&str> = None;
        let mut entries: Vec<Entry> = Vec::new();
        for line in text.lines() {
            let (sha256, name) = line
                .split_once("  ")
                .ok_or_else(|| format!("not a line of a list: {line:?}"))?;
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(format!("not a SHA-256 sum: {sha256:?}"));
            }
            let (file_version, kind) = file_name(name, variant)
                .ok_or_else(|| format!("not a file of a release of {variant}: {name:?}"))?;
            match version {
                None => version = Some(file_version),
                Some(v) if v != file_version => {
                    return Err("the list names more than one version".into());
                }
                Some(_) => {}
            }
            if entries.iter().any(|e| e.kind == kind) {
                return Err(format!("the list names {name:?} twice"));
            }
            entries.push(Entry {
                name: name.to_owned(),
                sha256: sha256.to_owned(),
                kind,
            });
        }
        if entries.len() != 3 {
            return Err("the list does not name /usr, its hash tree and the UKI".into());
        }
        Ok(Self {
            version: version.unwrap_or_default().to_owned(),
            entries,
        })
    }
}

/// Version and kind of a release file name of `variant`; `None` for any
/// other name.
fn file_name<'a>(name: &'a str, variant: &str) -> Option<(&'a str, Kind)> {
    let rest = name
        .strip_prefix("beize-")?
        .strip_prefix(variant)?
        .strip_prefix('_')?;
    if let Some(version) = rest.strip_suffix(".efi") {
        return valid_version(version).then_some((version, Kind::Uki));
    }
    let (stem, kind) = if let Some(stem) = rest.strip_suffix(".usr.erofs") {
        (stem, Kind::Usr)
    } else {
        (rest.strip_suffix(".usr.verity")?, Kind::UsrVerity)
    };
    let (version, uuid) = stem.rsplit_once('_')?;
    (valid_version(version) && valid_uuid(uuid)).then_some((version, kind))
}

/// A version: numbers separated by dots, e.g. `0.2` or `1.10.3`.
pub fn valid_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() <= 8
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A variant: lowercase letters, digits and `-`, as bz's `variant.rs`.
pub fn valid_variant(variant: &str) -> bool {
    !variant.is_empty()
        && variant
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_uuid(uuid: &str) -> bool {
    let groups: Vec<_> = uuid.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Whether version `a` is newer than `b` (both [`valid_version`]):
/// compared number by number, `1.10` after `1.9`, `1.0.1` after `1.0`.
pub fn newer(a: &str, b: &str) -> bool {
    let numbers = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    numbers(a) > numbers(b)
}

/// What a fetched list goes through: parsed, and its version compared. For
/// the fuzzers (fuzz/) and their regression test; panics only on a bug.
#[doc(hidden)]
pub fn fuzz_list(bytes: &[u8]) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    if let Ok(list) = List::parse(text, "desktop") {
        assert!(valid_version(&list.version));
        let _ = newer(&list.version, "0.2");
        for entry in &list.entries {
            let _ = entry.kind.max_size();
        }
    }
}

/// `IMAGE_VERSION` from an os-release file's content.
pub fn image_version(os_release: &str) -> Option<&str> {
    field(os_release, "IMAGE_VERSION")
}

/// `VARIANT_ID` from an os-release file's content (ADR 0025).
pub fn variant_id(os_release: &str) -> Option<&str> {
    field(os_release, "VARIANT_ID")
}

fn field<'a>(os_release: &'a str, key: &str) -> Option<&'a str> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(|v| v.trim_matches('"'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const UUID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn list(names: &[&str]) -> String {
        names.iter().map(|n| format!("{SUM}  {n}\n")).collect()
    }

    fn release_of(prefix: &str, version: &str) -> Vec<String> {
        vec![
            format!("{prefix}_{version}_{UUID}.usr.erofs"),
            format!("{prefix}_{version}_{UUID}.usr.verity"),
            format!("{prefix}_{version}.efi"),
        ]
    }

    fn release(version: &str) -> Vec<String> {
        release_of("beize-desktop", version)
    }

    fn parse(names: &[String]) -> Result<List, String> {
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        List::parse(&list(&names), "desktop")
    }

    /// Every input the fuzzers keep (fuzz/corpus, and what once crashed in
    /// fuzz/regressions) runs on every test, without a fuzzer.
    #[test]
    fn fuzz_inputs_still_pass() {
        let fuzz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut inputs = 0;
        for dir in ["corpus/update_list", "regressions/update_list"] {
            for entry in std::fs::read_dir(fuzz.join(dir)).into_iter().flatten() {
                fuzz_list(&std::fs::read(entry.unwrap().path()).unwrap());
                inputs += 1;
            }
        }
        assert!(inputs > 0);
    }

    #[test]
    fn reads_a_release() {
        let list = parse(&release("0.2")).unwrap();
        assert_eq!(list.version, "0.2");
        assert_eq!(
            list.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [Kind::Usr, Kind::UsrVerity, Kind::Uki]
        );
        assert_eq!(list.entries[2].name, "beize-desktop_0.2.efi");
    }

    /// A desktop takes only the desktop's releases: not the base's, not one
    /// of a variant whose name starts the same, not one from before variants.
    #[test]
    fn refuses_another_variant() {
        for prefix in [
            "beize-base",
            "beize-desktop-x",
            "beize",
            "beize-",
            "beize-Desktop",
        ] {
            assert!(parse(&release_of(prefix, "0.2")).is_err(), "{prefix}");
        }
        let names = release_of("beize-base", "0.2");
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        assert!(List::parse(&list(&names), "base").is_ok());
        assert!(List::parse(&list(&names), "").is_err());
        assert!(List::parse(&list(&names), "base_").is_err());
    }

    #[test]
    fn refuses_anything_else() {
        let good = release("0.2");
        let mut cases: Vec<Vec<String>> = vec![
            good[..2].to_vec(),                                  // a file missing
            [good.clone(), vec![good[2].clone()]].concat(),      // a file twice
            [&good[..2], &[release("0.3")[2].clone()]].concat(), // two versions
            [&good[..2], &["beize-desktop_0.2.efi.sig".into()]].concat(),
            [&good[..2], &["../beize-desktop_0.2.efi".into()]].concat(),
            [&good[..2], &["beize-desktop_0.2-rc1.efi".into()]].concat(),
            [
                &good[1..],
                &[format!("beize-desktop_0.2_{}.usr.erofs", "x".repeat(36))],
            ]
            .concat(),
        ];
        cases.push(release(".2"));
        cases.push(release("0..2"));
        for names in cases {
            assert!(parse(&names).is_err(), "{names:?}");
        }
        let names = release("0.2");
        let text = list(&names.iter().map(String::as_str).collect::<Vec<_>>());
        for broken in [
            text.replacen("  ", " ", 1),
            text.replacen(&SUM[..2], "AB", 1),
            text.replacen(&SUM[..2], "0", 1),
        ] {
            assert!(List::parse(&broken, "desktop").is_err());
        }
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.2", "0.1"));
        assert!(newer("1.10", "1.9"));
        assert!(newer("1.0.1", "1.0"));
        assert!(!newer("0.1", "0.1"));
        assert!(!newer("0.1", "0.2"));
        assert!(!valid_version(""));
        assert!(!valid_version("0.1a"));
        assert!(!valid_version("1.2.3.4.5.6.7.8.9"));
    }

    #[test]
    fn reads_the_image_version() {
        let text = "NAME=\"Beize\"\nIMAGE_VERSION=0.1\n";
        assert_eq!(image_version(text), Some("0.1"));
        assert_eq!(image_version("NAME=x\n"), None);
        let text = "VARIANT=\"Desktop\"\nVARIANT_ID=desktop\n";
        assert_eq!(variant_id(text), Some("desktop"));
        assert_eq!(variant_id("VARIANT=\"Desktop\"\n"), None);
    }
}
