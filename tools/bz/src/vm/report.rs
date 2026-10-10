//! The boot report the guest prints once systemd has finished booting
//! (system/usr/lib/beize/boot-report).

/// Printed by /usr/lib/beize/boot-report once systemd has finished booting.
pub(super) const REPORT_MARKER: &str = "BEIZE-BOOT-REPORT ";

/// The boot outcome as reported by the guest.
#[derive(Debug, PartialEq)]
pub(super) struct BootReport {
    pub(super) state: String,
    pub(super) secure_boot: String,
    /// `verity` if /usr is mounted from a dm-verity device.
    pub(super) usr: String,
    /// `encrypted` if / is mounted from a LUKS2 device.
    pub(super) root: String,
    /// How often this installation has booted, counted on the root partition.
    pub(super) boot: u32,
    pub(super) machine_id: String,
    pub(super) uptime: String,
    /// `refused` if the TPM did not unlock the root partition for the running
    /// system (it may only in the initrd).
    pub(super) system_unlock: String,
    /// networkd's operational state; `routable` = address + default route.
    pub(super) network: String,
    /// DNS servers resolved learned from the network.
    pub(super) dns: Vec<String>,
    /// The addresses `_gateway` resolved to through NSS (IPv4 and IPv6).
    pub(super) lookup: Vec<String>,
    /// The zone /etc/localtime points at, or `none`.
    pub(super) time_zone: String,
    /// How many time servers completed the NTS key exchange. Needs the
    /// internet, so reported, not required.
    pub(super) nts: String,
    /// The system clock when reporting, in seconds since 1970.
    pub(super) clock: u64,
    /// How long the kernel, the initrd and the system took to start, in µs
    /// (systemd's "Startup finished"); 0 = not measured.
    pub(super) kernel: u64,
    pub(super) initrd: u64,
    pub(super) userspace: u64,
    /// Lines of the critical chain beize-boot-chain wrote into the journal.
    pub(super) chain: u32,
    /// Units that are failed now, in the running system.
    pub(super) failed: Vec<String>,
    /// Units whose start failed at any time in this boot, the initrd and
    /// muted console phases included (from the journal).
    pub(super) boot_failed: Vec<String>,
}

impl BootReport {
    /// Parses `BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=1 machineid=… uptime=2.95s systemunlock=refused network=routable dns=10.0.2.3 lookup=10.0.2.2 tz=Europe/Berlin nts=4 clock=1791500000 kernel=612000 initrd=2100000 userspace=1400000 chain=12 failed=none bootfailed=none`.
    pub(super) fn parse(line: &str) -> Option<Self> {
        let fields = &line[line.find(REPORT_MARKER)? + REPORT_MARKER.len()..];
        let field = |key: &str| {
            fields
                .split_whitespace()
                .find_map(|f| f.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        let list = |key: &str| {
            Some(match field(key)?.as_str() {
                "none" => Vec::new(),
                list => list.split(',').map(str::to_owned).collect(),
            })
        };
        Some(Self {
            state: field("state")?,
            secure_boot: field("secureboot")?,
            usr: field("usr")?,
            root: field("root")?,
            boot: field("boot")?.parse().ok()?,
            machine_id: field("machineid")?,
            uptime: field("uptime")?,
            system_unlock: field("systemunlock")?,
            network: field("network")?,
            dns: list("dns")?,
            lookup: list("lookup")?,
            time_zone: field("tz")?,
            nts: field("nts")?,
            clock: field("clock")?.parse().ok()?,
            kernel: field("kernel")?.parse().ok()?,
            initrd: field("initrd")?.parse().ok()?,
            userspace: field("userspace")?.parse().ok()?,
            chain: field("chain")?.parse().ok()?,
            failed: list("failed")?,
            boot_failed: list("bootfailed")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: &str = "BEIZE-BOOT-REPORT state=running secureboot=on usr=verity root=encrypted boot=1 \
                         machineid=0123456789abcdef0123456789abcdef uptime=2.95s systemunlock=refused \
                         network=routable dns=10.0.2.3 lookup=10.0.2.2,fe80::2 tz=America/Sao_Paulo nts=4 \
                         clock=1791500000 kernel=612000 initrd=2100000 userspace=1400000 chain=12 \
                         failed=none bootfailed=none";

    #[test]
    fn parses_clean_report() {
        let report = BootReport::parse(CLEAN).unwrap();
        assert_eq!(report.state, "running");
        assert_eq!(report.secure_boot, "on");
        assert_eq!(report.usr, "verity");
        assert_eq!(report.root, "encrypted");
        assert_eq!(report.boot, 1);
        assert_eq!(report.machine_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(report.uptime, "2.95s");
        assert_eq!(report.system_unlock, "refused");
        assert_eq!(report.network, "routable");
        assert_eq!(report.dns, ["10.0.2.3"]);
        assert_eq!(report.lookup, ["10.0.2.2", "fe80::2"]);
        assert_eq!(report.time_zone, "America/Sao_Paulo");
        assert_eq!(report.nts, "4");
        assert_eq!(report.clock, 1_791_500_000);
        assert_eq!(report.kernel, 612_000);
        assert_eq!(report.initrd, 2_100_000);
        assert_eq!(report.userspace, 1_400_000);
        assert_eq!(report.chain, 12);
        assert!(report.failed.is_empty());
        assert!(report.boot_failed.is_empty());
    }

    #[test]
    fn parses_degraded_report_behind_console_noise() {
        let line = "\x1b[0m[  3.1] BEIZE-BOOT-REPORT state=degraded secureboot=off usr=unprotected root=plain boot=2 \
                    machineid=x uptime=3.1s systemunlock=none \
                    network=timeout dns=none lookup=none tz=none nts=0 \
                    clock=946684800 kernel=0 initrd=0 userspace=0 chain=0 failed=systemd-logind.service,systemd-logind-varlink.socket \
                    bootfailed=systemd-logind.service,beize-tpm.service";
        let report = BootReport::parse(line).unwrap();
        assert_eq!(report.state, "degraded");
        assert_eq!(report.network, "timeout");
        assert!(report.dns.is_empty());
        assert!(report.lookup.is_empty());
        assert_eq!(report.secure_boot, "off");
        assert_eq!(report.root, "plain");
        assert_eq!(report.boot, 2);
        assert_eq!(
            report.failed,
            ["systemd-logind.service", "systemd-logind-varlink.socket"]
        );
        // "bootfailed=" is a field of its own, not a second "failed=".
        assert_eq!(
            report.boot_failed,
            ["systemd-logind.service", "beize-tpm.service"]
        );
    }

    #[test]
    fn ignores_other_lines_and_incomplete_reports() {
        assert_eq!(
            BootReport::parse("[  OK  ] Reached target Basic System."),
            None
        );
        assert_eq!(BootReport::parse("BEIZE-BOOT-REPORT state=running"), None);
        assert_eq!(BootReport::parse(&CLEAN.replace("boot=1", "boot=x")), None);
        assert_eq!(BootReport::parse(&CLEAN.replace(" chain=12", "")), None);
    }
}
