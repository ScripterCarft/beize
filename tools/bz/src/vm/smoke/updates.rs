//! Scenarios of updates (ADR 0020): accepted, refused, and one that does not
//! start.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, ensure};

use super::Installed;
use super::integrity::{damage_verity, failed_start};
use crate::paths::Paths;
use crate::say;
use crate::tree;
use crate::vm::checks;
use crate::vm::checks::{boot_healthy, check_blessed};
use crate::vm::console::Answers;
use crate::vm::debug::Shell;
use crate::vm::machine::Machine;
use crate::vm::run::DEBUG_CONSOLE_CREDENTIALS;
use crate::vm::serve::Server;
use crate::{image, layout, release, secureboot, usr, util};

/// What `beize-update` says when it refuses a release, by the reason.
const UNTRUSTED_UPDATE: &str = "The update is not from Beize or was changed";
const NO_NEWER_UPDATE: &str = "There is no newer version.";

/// An update as the owner installs it (ADR 0020): a release of the next
/// version, fetched over HTTP from the host. Releases that are not what the
/// update key signed – a changed list, another key's signature, a changed
/// file – and an older version are refused, and nothing is written. The
/// right one goes into slot B; the next boot starts it, the TPM unlocks as
/// before, and boot counting marks it good – but not a boot in which a unit
/// of the variant's `running-units.txt` failed. A successful boot replaces an older
/// boot loader and its configuration with the ones from `/usr`.
pub(super) fn update(
    paths: &Paths,
    keys: &secureboot::Keys,
    installed: &Installed,
    good: &Path,
) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-update")?;
    let version = next_version();
    let releases = paths.variant_work.join("smoke-releases");
    tree::remove(&releases)?;
    let refused = refused_releases(&releases, good, keys, &paths.variant.id)?;
    let older_loader = paths.variant_work.join("smoke-update-loader.efi");
    older_boot_loader(paths, keys, &older_loader)?;
    for path in BOOT_LOADER_ON_ESP {
        put_on_esp(&machine.disk, &older_loader, path)?;
    }
    let older_conf = paths.variant_work.join("smoke-update-loader.conf");
    fs::write(&older_conf, OLDER_LOADER_CONF)?;
    put_on_esp(&machine.disk, &older_conf, LOADER_CONF_ON_ESP)?;
    fs::remove_file(&older_conf)?;

    let shell = Shell::boot(
        &machine,
        &Answers::none(),
        &DEBUG_CONSOLE_CREDENTIALS,
        false,
    )?;
    // The boot loader is updated at the end of the boot (beize-boot-update):
    // the signed one from /usr over both copies of the older one.
    let (_, lines) = shell.capture(
        "systemctl is-system-running --wait; \
         journalctl -b -o cat -u beize-boot-update.service",
    )?;
    ensure!(
        lines.iter().any(|l| l.trim() == "running"),
        "the boot did not finish as running: {lines:?}"
    );
    let copied = format!("Copied \"/usr/{}\" to ", usr::SIGNED_BOOT_LOADER_PATH);
    for path in BOOT_LOADER_ON_ESP {
        ensure!(
            lines
                .iter()
                .any(|l| l.contains(&copied) && l.trim_end().ends_with(&format!("{path}\"."))),
            "bootctl did not install the signed boot loader from /usr as {path}: {lines:?}"
        );
    }
    ensure!(
        lines.iter().any(|l| l.contains(&format!(
            "{LOADER_CONF_ON_ESP} from /usr/lib/beize/loader.conf"
        ))),
        "beize-boot-update did not bring the loader.conf of /usr onto the ESP: {lines:?}"
    );
    for (what, dir, says) in &refused {
        let server = Server::start(dir)?;
        let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
        ensure!(
            status != 0 && lines.iter().any(|l| l.contains(says)),
            "beize-update did not refuse {what} (exit status {status}): {lines:?}"
        );
    }
    let (_, lines) = shell.capture("ls /boot/EFI/Linux")?;
    ensure!(
        !lines.iter().any(|l| l.contains(&version)),
        "a refused release was written: {lines:?}"
    );
    // Nor is a copy of one kept (a changed file is refused while copying).
    let (_, lines) = shell.capture("ls -A /var/lib/beize-update /var/cache/beize-update")?;
    ensure!(
        !lines
            .iter()
            .any(|l| l.contains("verified") || l.starts_with("beize")),
        "a refused release was left on the disk: {lines:?}"
    );
    say!(
        "  ok: refused {}, nothing written",
        refused
            .iter()
            .map(|(what, _, _)| *what)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let server = Server::start(good)?;
    let started = Instant::now();
    let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
    let install_s = started.elapsed().as_secs_f64();
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {version} is installed"))),
        "beize-update did not install {version} (exit status {status}): {lines:?}"
    );
    drop(server);
    // systemd-sysupdate warned of nothing and wrote its install database.
    let (_, lines) = shell.capture(
        "echo \"warnings=$(journalctl -b -p warning -o cat -t systemd-sysupdate | wc -l) \
         installdb=$(ls /var/lib/systemd/sysupdate/installdb | wc -l)\"",
    )?;
    ensure!(
        lines
            .iter()
            .any(|l| l.trim().starts_with("warnings=0 installdb=")
                && !l.trim().ends_with("installdb=0")),
        "systemd-sysupdate warned or wrote no install database: {lines:?}"
    );
    shell.power_off()?;
    let listing = checks::uki_listing(&machine.disk)?;
    ensure!(
        // sysupdate writes the tries done as well: `+3-0`, the same count.
        listing
            .iter()
            .any(|name| name.starts_with(layout::uki_name(&version).trim_end_matches(".efi"))),
        "the update's UKI with boot counter is not on the ESP: {listing:?}"
    );
    say!("  ok: beize-update installed {version} into slot B ({install_s:.1}s)");
    let signed_loader = fs::read(paths.image.join("systemd-bootx64.efi"))?;
    for path in BOOT_LOADER_ON_ESP {
        ensure!(
            read_from_esp(&machine.disk, path, &paths.work)? == signed_loader,
            "the boot did not replace the older boot loader {path} with the signed one from /usr"
        );
    }
    fs::remove_file(&older_loader)?;
    ensure!(
        read_from_esp(&machine.disk, LOADER_CONF_ON_ESP, &paths.work)?
            == fs::read(paths.image.join("loader.conf"))?,
        "the boot did not replace the older {LOADER_CONF_ON_ESP} with the one from /usr"
    );
    say!(
        "  ok: the boot replaced an older boot loader and its loader.conf with the ones from /usr"
    );

    // A unit every boot runs fails: the boot does not count as successful,
    // and nothing of it is kept.
    let credentials = [&DEBUG_CONSOLE_CREDENTIALS[..], &[FAILING_NTP_CREDENTIAL]].concat();
    let shell = Shell::boot(&machine, &Answers::none(), &credentials, false)?;
    let (_, lines) = shell.capture(
        "echo \"state=$(systemctl is-system-running --wait) \
         ntp=$(systemctl is-failed ntpd-rs.service) \
         health=$(systemctl is-active beize-boot-health.service) \
         loader=$(systemctl is-active beize-boot-update.service)\"",
    )?;
    let expected = "state=degraded ntp=failed health=failed loader=inactive";
    ensure!(
        lines.iter().any(|l| l.trim() == expected),
        "a boot with a failed unit was not judged as failed (expected {expected}): {lines:?}"
    );
    shell.power_off()?;
    let listing = checks::uki_listing(&machine.disk)?;
    let tried_once = format!(
        "{}+{}-1.efi",
        layout::blessed_uki_name(&version).trim_end_matches(".efi"),
        layout::BOOT_TRIES - 1
    );
    ensure!(
        listing.contains(&tried_once),
        "after a failed boot the update's UKI should be {tried_once}: {listing:?}"
    );
    say!("  ok: a boot with a failed unit was not marked good ({tried_once})");

    // Boot #3: the starts with the root shell have no boot report.
    boot_healthy(&machine, 3, &Answers::none())?;
    check_blessed(&machine.disk, &version)?;
    say!("  ok: {version} started, the TPM unlocked the data as before");

    // The next update replaces the oldest version, never the running one.
    // It comes from a USB stick, as on the owner's PC: Beize and Windows
    // share the computer, there is no other one to serve it.
    let next = format!("{}.2", layout::OS_VERSION);
    test_release(paths, keys, &next, &releases.join(&next))?;
    let stick = paths.variant_work.join("smoke-update-stick.img");
    usb_stick(&releases.join(&next), &format!("beize-{next}"), &stick)?;
    machine.usb_stick = Some(stick.clone());
    let shell = Shell::boot(
        &machine,
        &Answers::none(),
        &DEBUG_CONSOLE_CREDENTIALS,
        false,
    )?;
    let (status, lines) = shell.capture(&format!(
        "mkdir -p /run/stick && mount -o ro /dev/disk/by-label/{STICK_LABEL} /run/stick \
         && beize-update install /run/stick/beize-{next}"
    ))?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {next} is installed"))),
        "beize-update did not install {next} from the USB stick (exit status {status}): {lines:?}"
    );
    shell.power_off()?;
    machine.usb_stick = None;
    fs::remove_file(&stick)?;
    let labels = usr_labels(&machine.disk)?;
    let expected = [&next, &next, &version, &version].map(|v| layout::usr_label(v));
    ensure!(
        labels == expected,
        "the second update did not replace slot A ({}): /usr slots are {labels:?}",
        layout::usr_label(layout::OS_VERSION)
    );
    let listing = checks::uki_listing(&machine.disk)?;
    ensure!(
        !listing.contains(&layout::blessed_uki_name(layout::OS_VERSION)),
        "the UKI of the replaced version is still on the ESP: {listing:?}"
    );
    boot_healthy(&machine, 4, &Answers::none())?;
    check_blessed(&machine.disk, &next)?;
    say!(
        "  ok: the second update {next}, from a USB stick, replaced {} in slot A",
        layout::OS_VERSION
    );
    machine.remove()?;
    fs::remove_dir_all(&releases)?;
    Ok(())
}

/// An update that is signed but does not start: the header of its /usr
/// hash tree is damaged, as from a broken build. Each of its tries fails in
/// the initrd and restarts by itself; then the previous version starts
/// again. That version started once: when it fails too, the start waits at
/// the emergency gate instead of restarting.
///
/// A damaged hash tree header fails when dm-verity is set up; damaged data
/// when /usr is mounted – `corrupted_usr` shows that. Both fail at once.
pub(super) fn broken_update(
    paths: &Paths,
    keys: &secureboot::Keys,
    installed: &Installed,
    good: &Path,
) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-broken-update")?;
    let version = next_version();
    let releases = paths.variant_work.join("smoke-broken-releases");
    tree::remove(&releases)?;
    fs::create_dir_all(&releases)?;
    let broken = releases.join(&version);
    util::run(
        Command::new("cp")
            .args(["--archive", "--reflink=auto"])
            .arg(good)
            .arg(&broken),
    )?;
    // Damage the hash tree, then sign the list of the damaged files.
    let list = fs::read_to_string(broken.join(release::LIST))?;
    let mut signed = String::new();
    for line in list.lines() {
        let (_, name) = line.split_once("  ").context("release list line")?;
        if name.ends_with(".usr.verity") {
            damage_verity(&broken.join(name), 0)?;
        }
        signed += &format!("{}  {name}\n", util::sha256_file(&broken.join(name))?);
    }
    fs::write(broken.join(release::LIST), signed)?;
    keys.sign_update(
        &broken.join(release::LIST),
        &broken.join(release::SIGNATURE),
    )?;
    install_update(&machine, &broken, &version)?;

    for attempt in 1..=layout::BOOT_TRIES {
        let start = failed_start(&machine)?;
        let try_n = format!("(try {attempt} of {})", layout::BOOT_TRIES);
        ensure!(
            start.verity_setup_failed()
                && start
                    .restarted
                    .as_ref()
                    .is_some_and(|line| line.contains(&try_n)),
            "the broken update did not restart after try {attempt}: {start:?}"
        );
        // It fails at once, not after the device timeout (see
        // system/initrd/.../dev-mapper-usr.device.d).
        ensure!(
            !start.failed.iter().any(|line| line.contains("Timed out")),
            "try {attempt} waited for /dev/mapper/usr until the device timeout: {start:?}"
        );
    }
    say!(
        "  ok: the broken update {version} restarted by itself after each of its {} tries",
        layout::BOOT_TRIES
    );

    // Boot #3: the starts with the root shell and the broken ones have no
    // boot report.
    boot_healthy(&machine, 3, &Answers::none())?;
    let listing = checks::uki_listing(&machine.disk)?;
    let used_up = format!(
        "{}+0-{}.efi",
        layout::blessed_uki_name(&version).trim_end_matches(".efi"),
        layout::BOOT_TRIES
    );
    ensure!(
        listing.contains(&layout::blessed_uki_name(layout::OS_VERSION))
            && listing.contains(&used_up),
        "expected {} and {used_up} on the ESP: {listing:?}",
        layout::blessed_uki_name(layout::OS_VERSION)
    );
    say!(
        "  ok: then the previous version {} started again; {used_up} has no tries left",
        layout::OS_VERSION
    );

    damage_verity(&machine.disk, image::USR_VERITY_OFFSET)?;
    let start = failed_start(&machine)?;
    ensure!(
        start.verity_setup_failed() && start.restarted.is_none(),
        "the previous version, which started before, did not wait at the emergency gate \
         when it failed: {start:?}"
    );
    say!(
        "  ok: when the previous version fails too, the start waits at the emergency gate \
         (it started once, so it is not on trial)"
    );
    machine.remove()?;
    fs::remove_dir_all(&releases)?;
    Ok(())
}

/// The volume label of the test's USB stick.
const STICK_LABEL: &str = "BEIZE";

/// A USB stick image as it comes from a shop: an MBR with one FAT32
/// partition, holding the release in `dir` as directory `name`.
fn usb_stick(dir: &Path, name: &str, image: &Path) -> Result<()> {
    const SIZE: u64 = 512 * layout::MIB;
    const START: u64 = 2048; // sectors of 512 bytes
    let file = fs::File::create(image)?;
    file.set_len(SIZE)?;
    drop(file);
    let mut sfdisk = Command::new("sfdisk")
        .arg("--quiet")
        .arg(image)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    sfdisk
        .stdin
        .take()
        .context("sfdisk stdin")?
        .write_all(format!("label: dos\nstart={START}, type=c\n").as_bytes())?;
    ensure!(
        sfdisk.wait()?.success(),
        "sfdisk failed on {}",
        image.display()
    );
    util::output(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", STICK_LABEL])
            .arg(format!("--offset={START}"))
            .arg(image)
            .arg(((SIZE - START * 512) / 1024).to_string()),
    )?;
    util::output(
        Command::new("mcopy")
            .arg("-s")
            .arg("-i")
            .arg(format!("{}@@{}", image.display(), START * 512))
            .arg(dir)
            .arg(format!("::/{name}")),
    )?;
    Ok(())
}

/// The version the updates install: one after the image's.
pub(super) fn next_version() -> String {
    format!("{}.1", layout::OS_VERSION)
}

/// Writes the release of `version`, signed with the test keys `keys`, into
/// `dir`; its output only if it fails.
pub(super) fn test_release(
    paths: &Paths,
    keys: &secureboot::Keys,
    version: &str,
    dir: &Path,
) -> Result<()> {
    let (built, said) =
        util::capture(|| release::write(paths, keys, version, image::Target::Vm, dir));
    if built.is_err() {
        said.into_iter().for_each(util::say_line);
    }
    built
}

/// Installs the release in `dir` as the owner would, from a root shell over
/// HTTP, and powers off.
fn install_update(machine: &Machine, dir: &Path, version: &str) -> Result<()> {
    let shell = Shell::boot(machine, &Answers::none(), &DEBUG_CONSOLE_CREDENTIALS, false)?;
    let server = Server::start(dir)?;
    let (status, lines) = shell.capture(&format!("beize-update install {}", server.url()))?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains(&format!("Beize {version} is installed"))),
        "beize-update did not install {version} (exit status {status}): {lines:?}"
    );
    drop(server);
    shell.power_off()
}

/// The GPT labels of the /usr partitions of `disk`, in partition order:
/// slot A data and verity, then slot B.
fn usr_labels(disk: &Path) -> Result<Vec<String>> {
    let dump = util::output(Command::new("sfdisk").arg("--dump").arg(disk))?;
    Ok(dump
        .lines()
        .filter(|line| {
            let line = line.to_uppercase();
            line.contains(&format!("TYPE={}", layout::USR_TYPE))
                || line.contains(&format!("TYPE={}", layout::USR_VERITY_TYPE))
        })
        .filter_map(|line| line.split("name=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect())
}

/// Where the boot loader lies on the ESP: systemd-boot's own path and the
/// removable-media path the firmware starts.
const BOOT_LOADER_ON_ESP: [&str; 2] = ["/EFI/systemd/systemd-bootx64.efi", "/EFI/BOOT/BOOTX64.EFI"];
/// The boot loader's configuration on the ESP, and the one Beize 0.3
/// installed: no update brought another onto the ESP before ADR 0024.
const LOADER_CONF_ON_ESP: &str = "/loader/loader.conf";
const OLDER_LOADER_CONF: &str =
    "timeout 0\neditor no\nconsole-mode keep\nsecure-boot-enroll force\n";
/// Makes ntpd-rs, a unit every boot runs, fail: a drop-in as a credential
/// (systemd-debug-generator), `[Service] ExecStartPre=/usr/bin/false`.
const FAILING_NTP_CREDENTIAL: &str = "type=11,value=io.systemd.credential.binary:\
     systemd.unit-dropin.ntpd-rs.service=W1NlcnZpY2VdCkV4ZWNTdGFydFByZT0vdXNyL2Jpbi9mYWxzZQo=";

/// The image's boot loader as an older version, signed with `keys`, into
/// `out`: its version (`#### LoaderInfo: systemd-boot 262 ####`, which
/// bootctl compares) begins with 0 instead.
fn older_boot_loader(paths: &Paths, keys: &secureboot::Keys, out: &Path) -> Result<()> {
    const INFO: &[u8] = b"#### LoaderInfo: systemd-boot ";
    let mut loader = fs::read(
        paths
            .sysroot
            .join("usr/lib/systemd/boot/efi/systemd-bootx64.efi"),
    )?;
    let mut patched = 0;
    let mut at = 0;
    while let Some(found) = loader[at..].windows(INFO.len()).position(|w| w == INFO) {
        let version = at + found + INFO.len();
        ensure!(
            matches!(loader[version], b'1'..=b'9'),
            "unexpected boot loader version"
        );
        loader[version] = b'0';
        patched += 1;
        at = version;
    }
    ensure!(patched > 0, "the boot loader carries no version");
    let unsigned = out.with_extension("unsigned");
    fs::write(&unsigned, loader)?;
    keys.sign(paths, &unsigned, out)?;
    fs::remove_file(&unsigned)?;
    Ok(())
}

/// Writes `file` to `path` on the ESP of `disk`.
fn put_on_esp(disk: &Path, file: &Path, path: &str) -> Result<()> {
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg(file)
            .arg(format!("::{path}")),
    )
}

/// The content of `path` on the ESP of `disk`, read through a file in `work`.
fn read_from_esp(disk: &Path, path: &str, work: &Path) -> Result<Vec<u8>> {
    let copy = work.join(format!(
        "esp-{}",
        path.rsplit('/').next().unwrap_or_default()
    ));
    util::run(
        Command::new("mcopy")
            .arg("-o")
            .arg("-i")
            .arg(format!("{}@@{}", disk.display(), image::ESP_OFFSET))
            .arg(format!("::{path}"))
            .arg(&copy),
    )?;
    let content = fs::read(&copy)?;
    fs::remove_file(&copy)?;
    Ok(content)
}

/// Releases next to `good` that beize-update must refuse: what each is, its
/// directory and what beize-update says.
fn refused_releases(
    releases: &Path,
    good: &Path,
    keys: &secureboot::Keys,
    variant: &str,
) -> Result<Vec<(&'static str, PathBuf, &'static str)>> {
    let copy = |name: &str, all: bool| -> Result<PathBuf> {
        let dir = releases.join(name);
        fs::create_dir_all(&dir)?;
        for entry in fs::read_dir(good)? {
            let path = entry?.path();
            let file = path.file_name().context("file name")?;
            if all || [release::LIST, release::SIGNATURE].contains(&file.to_str().unwrap_or("")) {
                fs::copy(&path, dir.join(file))?;
            }
        }
        Ok(dir)
    };
    let list = fs::read_to_string(good.join(release::LIST))?;

    // The list changed after signing: another sum for the first file.
    let changed_list = copy("changed-list", false)?;
    let flipped = if list.starts_with('0') { "1" } else { "0" };
    fs::write(
        changed_list.join(release::LIST),
        format!("{flipped}{}", &list[1..]),
    )?;

    // The list signed with another ML-DSA-65 key.
    let other_key = copy("other-key", false)?;
    util::output(
        Command::new("openssl")
            .args(["genpkey", "-algorithm", "ML-DSA-65", "-out"])
            .arg(other_key.join("key")),
    )?;
    util::output(
        Command::new("openssl")
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(other_key.join("key"))
            .arg("-in")
            .arg(other_key.join(release::LIST))
            .arg("-out")
            .arg(other_key.join(release::SIGNATURE)),
    )?;
    fs::remove_file(other_key.join("key"))?;

    // A file changed after signing: one byte of the UKI.
    let changed_file = copy("changed-file", true)?;
    let uki = list
        .lines()
        .filter_map(|line| line.split_once("  ").map(|(_, name)| name))
        .find(|name| name.ends_with(".efi"))
        .context("the release has no UKI")?
        .to_owned();
    let mut bytes = fs::read(changed_file.join(&uki))?;
    bytes[4096] ^= 1;
    fs::write(changed_file.join(&uki), bytes)?;

    // Correctly signed, but older than the running version.
    let older = releases.join("older");
    fs::create_dir_all(&older)?;
    fs::write(
        older.join(release::LIST),
        list.lines()
            .map(|line| {
                let (sum, name) = line.split_once("  ").unwrap_or_default();
                let (stream, rest) = name.split_once('_').unwrap_or_default();
                let version = rest.split('_').next().unwrap_or_default();
                let version = version.trim_end_matches(".efi");
                format!("{sum}  {stream}_{}\n", rest.replacen(version, "0.0.9", 1))
            })
            .collect::<String>(),
    )?;
    keys.sign_update(&older.join(release::LIST), &older.join(release::SIGNATURE))?;

    // Correctly signed, newer, but of another variant (ADR 0025).
    let other = if variant == "base" { "desktop" } else { "base" };
    let other_variant = releases.join("other-variant");
    fs::create_dir_all(&other_variant)?;
    fs::write(
        other_variant.join(release::LIST),
        list.replace(&format!("beize-{variant}_"), &format!("beize-{other}_")),
    )?;
    keys.sign_update(
        &other_variant.join(release::LIST),
        &other_variant.join(release::SIGNATURE),
    )?;

    Ok(vec![
        ("a changed list", changed_list, UNTRUSTED_UPDATE),
        ("another key's signature", other_key, UNTRUSTED_UPDATE),
        ("a changed file", changed_file, UNTRUSTED_UPDATE),
        ("an older version", older, NO_NEWER_UPDATE),
        ("another variant's release", other_variant, UNTRUSTED_UPDATE),
    ])
}
