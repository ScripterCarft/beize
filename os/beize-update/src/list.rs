//! The list of a release, `SHA256SUMS`: one line per file, `<sha256>  <name>`,
//! as `bz release` writes it. Read strictly: exactly the three files of one
//! version, under the names systemd-sysupdate matches – anything else is
//! refused before a single file is fetched or written.

/// The largest list and signature accepted.
pub const MAX_LIST: u64 = 64 * 1024;
pub const MAX_SIGNATURE: u64 = 16 * 1024;

/// What a file of a release is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `beize_<version>_<uuid>.usr.erofs`
    Usr,
    /// `beize_<version>_<uuid>.usr.verity`
    UsrVerity,
    /// `beize_<version>.efi`
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
    pub fn parse(text: &str) -> Result<Self, String> {
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
            let (file_version, kind) =
                file_name(name).ok_or_else(|| format!("not a file of a release: {name:?}"))?;
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

/// Version and kind of a release file name; `None` for any other name.
fn file_name(name: &str) -> Option<(&str, Kind)> {
    let rest = name.strip_prefix("beize_")?;
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
    if let Ok(list) = List::parse(text) {
        assert!(valid_version(&list.version));
        let _ = newer(&list.version, "0.2");
        for entry in &list.entries {
            let _ = entry.kind.max_size();
        }
    }
}

/// `IMAGE_VERSION` from an os-release file's content.
pub fn image_version(os_release: &str) -> Option<&str> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("IMAGE_VERSION="))
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

    fn release(version: &str) -> Vec<String> {
        vec![
            format!("beize_{version}_{UUID}.usr.erofs"),
            format!("beize_{version}_{UUID}.usr.verity"),
            format!("beize_{version}.efi"),
        ]
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
        let names = release("0.2");
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        let list = List::parse(&list(&names)).unwrap();
        assert_eq!(list.version, "0.2");
        assert_eq!(
            list.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [Kind::Usr, Kind::UsrVerity, Kind::Uki]
        );
        assert_eq!(list.entries[2].name, "beize_0.2.efi");
    }

    #[test]
    fn refuses_anything_else() {
        let good = release("0.2");
        let mut cases: Vec<Vec<String>> = vec![
            good[..2].to_vec(),                                  // a file missing
            [good.clone(), vec![good[2].clone()]].concat(),      // a file twice
            [&good[..2], &[release("0.3")[2].clone()]].concat(), // two versions
            [&good[..2], &["beize_0.2.efi.sig".into()]].concat(),
            [&good[..2], &["../beize_0.2.efi".into()]].concat(),
            [&good[..2], &["beize_0.2-rc1.efi".into()]].concat(),
            [
                &good[1..],
                &[format!("beize_0.2_{}.usr.erofs", "x".repeat(36))],
            ]
            .concat(),
        ];
        cases.push(release(".2"));
        cases.push(release("0..2"));
        for names in cases {
            let names: Vec<_> = names.iter().map(String::as_str).collect();
            assert!(List::parse(&list(&names)).is_err(), "{names:?}");
        }
        let names = release("0.2");
        let text = list(&names.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(List::parse(&text.replacen("  ", " ", 1)).is_err());
        assert!(List::parse(&text.replacen(&SUM[..2], "AB", 1)).is_err());
        assert!(List::parse(&text.replacen(&SUM[..2], "0", 1)).is_err());
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
    }
}
