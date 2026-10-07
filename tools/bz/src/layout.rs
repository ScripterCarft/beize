//! Versioning and disk layout, shared by everything that builds or boots an
//! image (ADR 0007).
//!
//! The shipped image holds the ESP and one `/usr` slot (A). On first boot
//! systemd-repart, running in the initrd, adds the second `/usr` slot (B) and
//! the writable root partition, following [`repart_definitions`].

pub const MIB: u64 = 1024 * 1024;

/// Version of the OS image; part of partition labels and UKI names.
pub const OS_VERSION: &str = "0.1";

/// Size of each `/usr` slot. An update must fit into one slot.
pub const USR_SLOT_SIZE: u64 = 1024 * MIB;
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

/// GPT label of the `/usr` slot holding this version, e.g. `beize_0.1`.
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

/// systemd-repart definitions (`/usr/lib/repart.d/`), as file name and content.
/// Existing partitions are matched to definitions of the same type in order.
pub fn repart_definitions() -> Vec<(&'static str, String)> {
    vec![
        (
            "00-esp.conf",
            "# Shipped in the image.\n[Partition]\nType=esp\n".to_owned(),
        ),
        (
            "10-usr-a.conf",
            "# Shipped in the image: the /usr slot the system was installed with.\n\
             [Partition]\nType=usr\n"
                .to_owned(),
        ),
        (
            "11-usr-b.conf",
            format!(
                "# Created on first boot: the second /usr slot for A/B updates.\n\
                 [Partition]\nType=usr\nLabel=_empty\n\
                 SizeMinBytes={USR_SLOT_SIZE}\nSizeMaxBytes={USR_SLOT_SIZE}\n"
            ),
        ),
        (
            "20-root.conf",
            format!(
                "# Created on first boot: /, holding /etc, /var and /home.\n\
                 # Emptied by a factory reset.\n\
                 [Partition]\nType=root\nLabel=root\nFormat=ext4\nFactoryReset=yes\n\
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
    fn second_usr_slot_matches_the_first() {
        let defs = repart_definitions();
        let usr_b = &defs
            .iter()
            .find(|(name, _)| *name == "11-usr-b.conf")
            .unwrap()
            .1;
        assert!(usr_b.contains(&format!("SizeMinBytes={USR_SLOT_SIZE}")));
        assert!(usr_b.contains(&format!("SizeMaxBytes={USR_SLOT_SIZE}")));
        // Order matters: the shipped slot must be matched before the new one.
        let names: Vec<_> = defs.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
