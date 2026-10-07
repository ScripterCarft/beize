//! Versioning and disk layout, shared by everything that builds or boots an
//! image (ADR 0007, ADR 0008).
//!
//! The shipped image holds the ESP and `/usr` slot A: the EROFS data
//! partition plus its dm-verity hash partition. On first boot systemd-repart,
//! running in the initrd, adds slot B and the writable root partition,
//! following [`repart_definitions`].

use anyhow::{Result, ensure};

pub const MIB: u64 = 1024 * 1024;

/// Version of the OS image; part of partition labels and UKI names.
pub const OS_VERSION: &str = "0.1";

/// Size of each `/usr` data slot. An update must fit into one slot.
pub const USR_SLOT_SIZE: u64 = 1024 * MIB;
/// Size of each `/usr` verity slot: the hash tree of a full 1 GiB data slot
/// with 4 KiB blocks and SHA-256 takes about 8 MiB.
pub const USR_VERITY_SLOT_SIZE: u64 = 64 * MIB;
/// dm-verity data and hash block size (the EROFS block size, too).
pub const VERITY_BLOCK_SIZE: u64 = 4096;
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

/// Boot attempts a new version gets before systemd-boot falls back to the
/// previous one. Cleared by systemd-bless-boot once boot-complete.target is reached.
pub const BOOT_TRIES: u32 = 3;

/// GPT partition types (UAPI.2 Discoverable Partitions Specification).
pub const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
pub const USR_TYPE: &str = "8484680C-9521-48C6-9C11-B0720656F69E";
pub const USR_VERITY_TYPE: &str = "77FF5F63-E7B6-4633-ACF4-1565B864C0E6";
pub const ROOT_TYPE: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";

/// GPT label of both partitions of the `/usr` slot holding this version, e.g. `beize_0.1`.
pub fn usr_label() -> String {
    format!("beize_{OS_VERSION}")
}

/// File name of the UKI on the ESP, with the boot counter, e.g. `beize_0.1+3.efi`.
pub fn uki_name() -> String {
    format!("beize_{OS_VERSION}+{BOOT_TRIES}.efi")
}

/// The UKI's name once systemd-bless-boot has marked it good.
pub fn blessed_uki_name() -> String {
    format!("beize_{OS_VERSION}.efi")
}

pub fn os_release() -> String {
    format!(
        "NAME=\"Beize\"\n\
         ID=beize\n\
         PRETTY_NAME=\"Beize {OS_VERSION} (Entwicklungsversion)\"\n\
         VERSION_ID={OS_VERSION}\n\
         IMAGE_ID=beize\n\
         IMAGE_VERSION={OS_VERSION}\n"
    )
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
                 # LUKS2, sealed to the TPM and the Secure Boot state (PCR 7, ADR 0009).\n\
                 [Partition]\nType=root\nLabel=root\nFormat=ext4\nFactoryReset=yes\n\
                 Encrypt=tpm2\nTPM2PCRs=7\n\
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
        assert_eq!(usr_label(), format!("beize_{OS_VERSION}"));
        assert_eq!(uki_name(), format!("beize_{OS_VERSION}+3.efi"));
        assert_eq!(blessed_uki_name(), format!("beize_{OS_VERSION}.efi"));
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
        assert!(root.contains("TPM2PCRs=7\n"));
    }
}
