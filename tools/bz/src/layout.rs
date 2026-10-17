//! Versioning and disk layout, shared by everything that builds or boots an
//! image (ADR 0007, ADR 0008).
//!
//! The shipped image holds the ESP and `/usr` slot A: the EROFS data
//! partition plus its dm-verity hash partition. On first boot systemd-repart,
//! running in the initrd, adds slot B and the writable root partition,
//! following [`repart_definitions`].

use anyhow::{Result, ensure};

use crate::variant::Variant;

pub const MIB: u64 = 1024 * 1024;

/// Version of the OS image; part of partition labels and UKI names.
pub const OS_VERSION: &str = "0.4";

/// Size of each `/usr` data slot. An update must fit into one slot, and the
/// size is fixed for the life of an installation (ADR 0020): room for the
/// graphics stack, firmware and fonts still to come, as ChromeOS learned when
/// it had to grow its 2 GiB root slots to 4 GiB.
pub const USR_SLOT_SIZE: u64 = 4096 * MIB;
/// Size of each `/usr` verity slot: the hash tree of a full data slot
/// ([`verity_tree_size`], ~32 MiB), with room to spare.
pub const USR_VERITY_SLOT_SIZE: u64 = 64 * MIB;
/// dm-verity data and hash block size (the EROFS block size, too).
pub const VERITY_BLOCK_SIZE: u64 = 4096;
const _: () = assert!(verity_tree_size(USR_SLOT_SIZE) <= USR_VERITY_SLOT_SIZE);

/// Size of the dm-verity hash tree (SHA-256, [`VERITY_BLOCK_SIZE`]) of `data`
/// bytes: every level hashes the blocks of the one below until one block
/// remains, plus the superblock.
pub const fn verity_tree_size(data: u64) -> u64 {
    let per_block = VERITY_BLOCK_SIZE / 32;
    let mut blocks = data.div_ceil(VERITY_BLOCK_SIZE);
    let mut tree = 1;
    loop {
        blocks = blocks.div_ceil(per_block);
        tree += blocks;
        if blocks == 1 {
            return tree * VERITY_BLOCK_SIZE;
        }
    }
}
/// Holds systemd-boot and the UKIs of both slots with room to spare.
pub const ESP_SIZE: u64 = 512 * MIB;
/// FAT cluster size of the ESP. FAT32 requires at least 65525 clusters;
/// firmware does not recognise a smaller "FAT32" volume at all.
pub const ESP_CLUSTER_SIZE: u64 = 4096;
const FAT32_MIN_CLUSTERS: u64 = 65525;
// Checked at compile time; 1024 clusters of headroom for the FAT tables.
const _: () = assert!(ESP_SIZE / ESP_CLUSTER_SIZE > FAT32_MIN_CLUSTERS + 1024);
/// The root partition never gets smaller than this; it grows into free space.
const ROOT_MIN_SIZE: u64 = 2048 * MIB;
/// The smallest disk first boot can complete on: ESP, both `/usr` slots and
/// the smallest root partition, plus 1 MiB alignment and the GPT at each end.
pub const MIN_DISK_SIZE: u64 =
    ESP_SIZE + 2 * (USR_SLOT_SIZE + USR_VERITY_SLOT_SIZE) + ROOT_MIN_SIZE + 2 * MIB;

/// Boot attempts a new version gets before systemd-boot falls back to the
/// previous one. Cleared by systemd-bless-boot once boot-complete.target is reached.
pub const BOOT_TRIES: u32 = 3;

/// GPT partition types (UAPI.2 Discoverable Partitions Specification).
pub const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
pub const USR_TYPE: &str = "8484680C-9521-48C6-9C11-B0720656F69E";
pub const USR_VERITY_TYPE: &str = "77FF5F63-E7B6-4633-ACF4-1565B864C0E6";
pub const ROOT_TYPE: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";

/// GPT label of both partitions of the `/usr` slot holding `version`, e.g. `beize_0.1`.
pub fn usr_label(version: &str) -> String {
    format!("beize_{version}")
}

/// File name of the UKI on the ESP, with the boot counter, e.g. `beize_0.1+3.efi`.
pub fn uki_name(version: &str) -> String {
    format!("beize_{version}+{BOOT_TRIES}.efi")
}

/// The UKI's name once systemd-bless-boot has marked it good.
pub fn blessed_uki_name(version: &str) -> String {
    format!("beize_{version}.efi")
}

/// The os-release of `/usr` and of the UKI. systemd-sysupdate takes the
/// running version from `IMAGE_VERSION` (ADR 0020); the variant is
/// `VARIANT_ID`, as Fedora's editions (ADR 0025).
pub fn os_release(version: &str, variant: &Variant) -> String {
    format!(
        "{}PRETTY_NAME=\"Beize {version} (Entwicklungsversion)\"\n\
         VERSION_ID={version}\nIMAGE_VERSION={version}\n\
         VARIANT=\"{}\"\nVARIANT_ID={}\n",
        initrd_release(),
        variant.name,
        variant.id
    )
}

/// The os-release of the initrd: without a version, since one initrd goes
/// into the UKIs of several versions (a release, `bz smoke`'s updates).
pub fn initrd_release() -> String {
    "NAME=\"Beize\"\nID=beize\nIMAGE_ID=beize\n".to_owned()
}

/// Where beize-updated puts the files of an update once it has verified
/// them; the only source systemd-sysupdate installs from (ADR 0020).
pub const UPDATE_SOURCE_DIR: &str = "/var/lib/beize-update/verified";

/// Release file names as systemd-sysupdate patterns: `@v` the version, `@u`
/// the partition UUID, derived from the root hash like in the image
/// ([`verity_partition_uuids`]), so that `usrhash=` finds an updated slot.
pub const RELEASE_USR: &str = "beize_@v_@u.usr.erofs";
pub const RELEASE_USR_VERITY: &str = "beize_@v_@u.usr.verity";
pub const RELEASE_UKI: &str = "beize_@v.efi";

/// Fills a release file name pattern.
pub fn release_file(pattern: &str, version: &str, uuid: &str) -> String {
    pattern
        .replace("@v", version)
        .replace("@u", &uuid.to_lowercase())
}

/// systemd-sysupdate transfers (`/usr/lib/sysupdate.d/`), as file name and
/// content. sysupdate writes them in this order: the UKI last, so a slot is
/// never booted before it is complete. The running version is protected;
/// two versions are kept, the running one and the other slot.
pub fn sysupdate_definitions() -> Vec<(&'static str, String)> {
    let source = |pattern: &str| {
        format!(
            "[Transfer]\nProtectVersion=%A\n\
             # A local source: beize-updated verified the signature and every file.\n\
             Verify=no\n\n\
             [Source]\nType=regular-file\nPath={UPDATE_SOURCE_DIR}\nMatchPattern={pattern}\n\n"
        )
    };
    let partition = |comment: &str, pattern: &str, ty: &str| {
        format!(
            "# {comment}\n{}[Target]\nType=partition\nPath=auto\nMatchPattern=beize_@v\n\
             MatchPartitionType={ty}\nPartitionFlags=0\nReadOnly=1\nInstancesMax=2\n",
            source(pattern)
        )
    };
    vec![
        (
            "10-usr.transfer",
            partition("The /usr data into the free slot.", RELEASE_USR, "usr"),
        ),
        (
            "11-usr-verity.transfer",
            partition("Its dm-verity hash tree.", RELEASE_USR_VERITY, "usr-verity"),
        ),
        (
            "20-uki.transfer",
            format!(
                "# The UKI last, with the boot counter (ADR 0007).\n{}[Target]\n\
                 Type=regular-file\nPath=/EFI/Linux\nPathRelativeTo=boot\n\
                 MatchPattern=beize_@v+@l-@d.efi beize_@v+@l.efi beize_@v.efi\n\
                 Mode=0644\nTriesLeft={BOOT_TRIES}\nTriesDone=0\nInstancesMax=2\n",
                source(RELEASE_UKI)
            ),
        ),
    ]
}

/// The partition UUIDs systemd derives from a dm-verity root hash (UAPI.2):
/// the data partition gets the first 128 bits, the hash partition the last
/// 128 bits. `usrhash=` on the kernel command line thereby selects exactly
/// the slot whose content matches.
pub fn verity_partition_uuids(root_hash: &str) -> Result<(String, String)> {
    ensure!(
        root_hash.len() == 64 && root_hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected a SHA-256 root hash, got {root_hash:?}"
    );
    let uuid = |hex: &str| {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
        .to_uppercase()
    };
    Ok((uuid(&root_hash[..32]), uuid(&root_hash[32..])))
}

/// systemd-repart definitions (`/usr/lib/repart.d/`), as file name and content.
/// Existing partitions are matched to definitions of the same type in order.
/// Every partition except root has a fixed size: repart grows partitions
/// without an upper bound into free space, existing ones included.
pub fn repart_definitions() -> Vec<(&'static str, String)> {
    let fixed = |comment: &str, ty: &str, label: Option<&str>, size: u64| {
        let label = label.map(|l| format!("Label={l}\n")).unwrap_or_default();
        format!(
            "# {comment}\n[Partition]\nType={ty}\n{label}\
             SizeMinBytes={size}\nSizeMaxBytes={size}\n"
        )
    };
    vec![
        (
            "00-esp.conf",
            fixed("Shipped in the image.", "esp", None, ESP_SIZE),
        ),
        (
            "10-usr-a.conf",
            fixed(
                "Shipped in the image: data of the /usr slot the system was installed with.",
                "usr",
                None,
                USR_SLOT_SIZE,
            ),
        ),
        (
            "11-usr-a-verity.conf",
            fixed(
                "Shipped in the image: dm-verity hash tree of /usr slot A.",
                "usr-verity",
                None,
                USR_VERITY_SLOT_SIZE,
            ),
        ),
        (
            "12-usr-b.conf",
            fixed(
                "Created on first boot: data of /usr slot B, for A/B updates.",
                "usr",
                Some("_empty"),
                USR_SLOT_SIZE,
            ),
        ),
        (
            "13-usr-b-verity.conf",
            fixed(
                "Created on first boot: dm-verity hash tree of /usr slot B.",
                "usr-verity",
                Some("_empty"),
                USR_VERITY_SLOT_SIZE,
            ),
        ),
        (
            "20-root.conf",
            format!(
                "# Created on first boot: /, holding /etc, /var and /home.\n\
                 # Emptied by a factory reset.\n\
                 # LUKS2, sealed to the TPM: the Secure Boot state (PCR 7) and nothing\n\
                 # added to the boot (PCR 12: extra kernel parameters, credentials,\n\
                 # add-ons), plus the signed PCR 11 policy of the UKI (ADR 0009/0010).\n\
                 [Partition]\nType=root\nLabel=root\nFormat=ext4\nFactoryReset=yes\n\
                 Encrypt=tpm2\nTPM2PCRs=7+12\n\
                 SizeMinBytes={ROOT_MIN_SIZE}\n"
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_version_and_boot_counter() {
        assert_eq!(usr_label("0.2"), "beize_0.2");
        assert_eq!(uki_name("0.2"), "beize_0.2+3.efi");
        assert_eq!(blessed_uki_name("0.2"), "beize_0.2.efi");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let release = os_release("0.2", &Variant::load(&root, "desktop").unwrap());
        assert!(release.contains("\nIMAGE_VERSION=0.2\n"));
        assert!(release.contains("\nVARIANT=\"Desktop\"\nVARIANT_ID=desktop\n"));
        assert!(!initrd_release().contains("VERSION"));
    }

    /// The emergency gate decides by tries and entry names whether a new
    /// version restarts (ADR 0020).
    #[test]
    fn beize_tpm_knows_tries_and_names() {
        let trial = include_str!("../../../os/beize-tpm/src/trial.rs");
        assert!(trial.contains(&format!("pub const BOOT_TRIES: u32 = {BOOT_TRIES};")));
        assert!(trial.contains("const ENTRY_PREFIX: &str = \"beize_\";"));
        assert!(blessed_uki_name("0.2").starts_with("beize_"));
    }

    #[test]
    fn verity_tree_size_matches_veritysetup() {
        // veritysetup format of 4 GiB: 8192 + 64 + 1 hash blocks + superblock.
        assert_eq!(verity_tree_size(4096 * MIB), (8192 + 64 + 1 + 1) * 4096);
        assert_eq!(verity_tree_size(4096), 2 * 4096);
    }

    #[test]
    fn sysupdate_writes_what_the_image_names() {
        let defs = sysupdate_definitions();
        let names: Vec<_> = defs.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "sysupdate runs the transfers in name order");
        assert!(names.last().unwrap().contains("uki"), "the UKI comes last");
        let uki = &defs[2].1;
        // The patterns name what the image ships, with and without counter.
        for name in [uki_name("@v"), blessed_uki_name("@v")] {
            let pattern = name.replace(&format!("+{BOOT_TRIES}"), "+@l");
            assert!(uki.contains(&pattern), "{pattern}");
        }
        assert!(uki.contains(&format!("TriesLeft={BOOT_TRIES}\n")));
        for (_, def) in &defs[..2] {
            assert!(def.contains(&format!("MatchPattern={}\n", usr_label("@v"))));
        }
        for (_, def) in &defs {
            assert!(def.contains(&format!("Path={UPDATE_SOURCE_DIR}\n")));
            assert!(def.contains("ProtectVersion=%A\n"));
        }
        assert_eq!(
            release_file(RELEASE_USR, "0.2", "ABCD-EF"),
            "beize_0.2_abcd-ef.usr.erofs"
        );
    }

    #[test]
    fn only_root_grows() {
        for (name, def) in repart_definitions() {
            let fixed = def.contains("SizeMaxBytes=");
            assert_eq!(fixed, !def.contains("Type=root"), "{name}");
        }
    }

    #[test]
    fn derives_partition_uuids_from_root_hash() {
        let hash = "0123456789abcdef0123456789abcdeffedcba9876543210fedcba9876543210";
        let (data, verity) = verity_partition_uuids(hash).unwrap();
        assert_eq!(data, "01234567-89AB-CDEF-0123-456789ABCDEF");
        assert_eq!(verity, "FEDCBA98-7654-3210-FEDC-BA9876543210");
        assert!(verity_partition_uuids("abc").is_err());
    }

    #[test]
    fn slot_b_matches_slot_a() {
        let defs = repart_definitions();
        let def = |name: &str| &defs.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!(def("12-usr-b.conf").contains("Type=usr\n"));
        assert!(def("12-usr-b.conf").contains(&format!("SizeMaxBytes={USR_SLOT_SIZE}")));
        assert!(def("13-usr-b-verity.conf").contains("Type=usr-verity\n"));
        assert!(
            def("13-usr-b-verity.conf").contains(&format!("SizeMaxBytes={USR_VERITY_SLOT_SIZE}"))
        );
        // Order matters: shipped partitions are matched before new ones.
        let names: Vec<_> = defs.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn root_is_sealed_to_the_tpm() {
        let defs = repart_definitions();
        let root = &defs.iter().find(|(n, _)| *n == "20-root.conf").unwrap().1;
        assert!(root.contains("Encrypt=tpm2\n"));
        assert!(root.contains("TPM2PCRs=7+12\n"));
    }
}
