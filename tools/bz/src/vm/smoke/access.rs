//! Scenarios of access: the emergency gate, the recovery in the boot menu
//! and privileges (ADR 0016, ADR 0018, ADR 0019).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use super::Installed;
use crate::paths::Paths;
use crate::say;
use crate::vm::checks::{boot_healthy, damage_root_file_system};
use crate::vm::console::{
    Answers, ConsoleEvent, EMERGENCY_HEADLINE, EMERGENCY_SHELL_STARTS, EMERGENCY_WRONG_KEY,
    OWNER_CONSOLE_CREDENTIALS, PASSWORD_PAUSE, RECOVERY_HEADLINE, SMOKE_CREDENTIALS,
    SMOKE_PASSWORD, SMOKE_USER, Session, strip_ansi,
};
use crate::vm::debug::Shell;
use crate::vm::machine::boot_once;
use crate::{image, layout};

/// Bounds a boot into the emergency gate, a wrong key (3 s penalty), a
/// restart after Ctrl+C and the shell.
const GATE_TIMEOUT: Duration = Duration::from_secs(90);
/// beize-account's questions (os/beize-accounts/src/bin/beize-account.rs),
/// for the owner "smoke" and the account "guest" the owner creates. None is
/// part of another ("New password:" – "New password again:" – "New password
/// of guest:"; "Password of guest:" starts with a capital).
const CURRENT_PASSWORD_PROMPT: &str = "Current password of smoke:";
const NEW_PASSWORD_PROMPT: &str = "New password:";
const NEW_PASSWORD_AGAIN_PROMPT: &str = "New password again:";
const OWN_PASSWORD_PROMPT: &str = "Your password (smoke):";
const RESET_OWNER_PROMPT: &str = "New password of smoke:";
const RESET_OWNER_AGAIN_PROMPT: &str = "New password of smoke again:";
const GUEST_NAME_PROMPT: &str = "Full name of guest";
const GUEST_PASSWORD_PROMPT: &str = "Password of guest:";
const GUEST_PASSWORD_AGAIN_PROMPT: &str = "Password of guest again:";
const RESET_GUEST_PROMPT: &str = "New password of guest:";
const RESET_GUEST_AGAIN_PROMPT: &str = "New password of guest again:";
/// The owner's password after changing it, and after resetting it.
const CHANGED_PASSWORD: &str = "smoke-changed-password";
const RESET_PASSWORD: &str = "smoke-reset-password";
const WRONG_PASSWORD: &str = "wrong-password";
/// The guest's password, as created and after the owner reset it.
const GUEST_PASSWORD: &str = "guest-first-password";
const GUEST_RESET_PASSWORD: &str = "guest-reset-password";

/// Like a thief who damages the disk so that the start fails after the TPM
/// unlocked the data (ADR 0016): the console must not hand out a shell – not
/// for a wrong key, not after Ctrl+C. With the recovery key it does, and from
/// that shell the TPM unlocks nothing.
pub(super) fn emergency_gate(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-emergency")?;
    damage_root_file_system(&machine.disk, &paths.work)?;
    // The key as a German keyboard types it with the US keymap: "/" for each
    // dash. Only its letters count (ADR 0017).
    let as_typed = installed.recovery_key.replace('-', "/");
    let session = Session::start(
        &machine,
        &Answers::emergency_gate(&as_typed),
        &SMOKE_CREDENTIALS,
    )?;
    let deadline = Instant::now() + GATE_TIMEOUT;
    let (mut headlines, mut refused, mut detached) = (0, false, false);
    let mut tpm_refused = None;
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            continue;
        };
        let line = strip_ansi(&line);
        let line = line.trim();
        if line.contains(EMERGENCY_HEADLINE) {
            headlines += 1;
        } else if line.contains(EMERGENCY_WRONG_KEY) {
            refused = true;
        } else if line.contains(EMERGENCY_SHELL_STARTS) {
            ensure!(
                refused && headlines >= 2,
                "the emergency gate opened a shell before it refused a wrong key and Ctrl+C"
            );
            // The TPM unlocked the root partition before its mount failed:
            // close it, then ask the TPM for the key once more.
            session.type_line("systemd-cryptsetup detach root && echo BEIZE-DETACHED");
            session.type_line(
                "systemd-cryptsetup attach probe /dev/gpt-auto-root-luks - \
                 tpm2-device=auto,headless=yes >/dev/null 2>&1 \
                 && echo BEIZE-TPM-UNLOCKED || echo BEIZE-TPM-REFUSED",
            );
        } else if line == "BEIZE-DETACHED" {
            detached = true;
        } else if line == "BEIZE-TPM-UNLOCKED" || line == "BEIZE-TPM-REFUSED" {
            tpm_refused = Some(line == "BEIZE-TPM-REFUSED");
            break;
        }
    }
    session.power_off_within(Duration::ZERO)?;
    match tpm_refused {
        None => bail!(
            "no emergency shell with the recovery key within {}s",
            GATE_TIMEOUT.as_secs()
        ),
        Some(_) if !detached => bail!("the emergency shell could not close the root partition"),
        Some(false) => bail!("from the emergency shell the TPM still unlocked the root partition"),
        Some(true) => say!(
            "  ok: no shell for a wrong key or after Ctrl+C; with the recovery key a shell \
             from which the TPM unlocks nothing"
        ),
    }
    machine.remove()
}

/// The recovery (ADR 0018), chosen in the boot menu: the TPM refuses it, so
/// the initrd asks for the recovery key; the gate takes that key instead of
/// asking again and opens a root shell with the data unlocked. The next
/// normal start unlocks by the TPM again: the recovery bound nothing to it.
pub(super) fn recovery(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-recovery")?;
    boot_once(
        &machine,
        &format!(
            "{}{}",
            layout::blessed_uki_name(layout::OS_VERSION),
            image::RECOVERY_ENTRY
        ),
    )?;
    // A forgotten password (ADR 0019): root in the recovery sets a new one
    // through the account service, and PAM's own check takes it.
    let answers = Answers::recovery(&installed.recovery_key)
        .answering(RESET_OWNER_PROMPT, &[RESET_PASSWORD])
        .answering(RESET_OWNER_AGAIN_PROMPT, &[RESET_PASSWORD]);
    let session = Session::start(&machine, &answers, &SMOKE_CREDENTIALS)?;
    let deadline = Instant::now() + GATE_TIMEOUT;
    let (mut prompted, mut headline, mut shell, mut reset) = (false, false, None, None);
    let mut failed = Vec::new();
    while let Some(event) = session.next(deadline) {
        let ConsoleEvent::Line(line) = event else {
            prompted = true;
            continue;
        };
        let line = strip_ansi(&line);
        if line.contains("[FAILED]") {
            failed.push(line);
        } else if line.contains(RECOVERY_HEADLINE) {
            headline = true;
        } else if line.contains(EMERGENCY_SHELL_STARTS) {
            session.type_line(
                "echo BEIZE-RECOVERY-$(id -u)-$(systemctl show -P ConditionResult \
                 systemd-tpm2-setup-early)-$(ls /home)",
            );
        } else if let Some(found) = line.strip_prefix("BEIZE-RECOVERY-") {
            shell = Some(found.to_owned());
            session.type_line(&format!(
                "beize-account reset {SMOKE_USER} && printf '%s\\0' {RESET_PASSWORD} \
                 | unix_chkpwd {SMOKE_USER} nullok; echo BEIZE-RESET-$?"
            ));
        } else if let Some(status) = line.strip_prefix("BEIZE-RESET-") {
            reset = Some(status.to_owned());
            break;
        }
    }
    session.power_off_within(Duration::ZERO)?;
    ensure!(
        prompted,
        "the recovery start did not ask for the recovery key"
    );
    ensure!(
        headline,
        "the recovery start did not reach the recovery gate"
    );
    let Some(shell) = shell else {
        bail!(
            "no root shell within {}s after the recovery key (did the gate ask for it again?)",
            GATE_TIMEOUT.as_secs()
        );
    };
    ensure!(
        shell.starts_with("0-") && shell.ends_with(&format!("-{SMOKE_USER}")),
        "the recovery shell is not root with the data unlocked: {shell}"
    );
    // The NvPCR setup needs the signed initrd policy: skipped, not failed
    // (on real TPMs it fails rather than giving up quietly).
    ensure!(
        shell.starts_with("0-no-"),
        "the recovery started the TPM's NvPCR setup in the system: {shell}"
    );
    say!("  ok: the recovery asks for the recovery key once and opens a root shell with the data");
    ensure!(
        reset.as_deref() == Some("0"),
        "beize-account reset in the recovery did not give the owner a new password \
         (status {reset:?})"
    );
    say!("  ok: in the recovery root sets a new password for a forgotten one");
    boot_healthy(&machine, 3, &Answers::none())?;
    say!("  ok: after the recovery the TPM unlocks by itself again");
    machine.remove()
}

/// Privileges (ADR 0018), as the owner at a terminal: run0 refuses a wrong
/// password and gives root for the right one; a file made setuid or given a
/// capability grants nothing, because / and /usr are mounted nosuid.
pub(super) fn privileges(paths: &Paths, installed: &Installed) -> Result<()> {
    let machine = installed.machine.copy_as(paths, "smoke-privileges")?;
    // polkit's questions, in order: the serial console is no active session,
    // so polkit asks every time. A wrong password comes last: polkit's agent
    // may ask again after one – however often, from there on only wrong
    // passwords are typed.
    let mut passwords = vec![
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
        SMOKE_PASSWORD,
        CHANGED_PASSWORD,
        CHANGED_PASSWORD,
    ];
    passwords.extend([WRONG_PASSWORD; 10]);
    let answers = Answers::passwords(&passwords)
        // The owner creates "guest", first with a wrong password of their
        // own, then resets the guest's password.
        .answering(
            OWN_PASSWORD_PROMPT,
            &[WRONG_PASSWORD, CHANGED_PASSWORD, CHANGED_PASSWORD],
        )
        .answering(GUEST_NAME_PROMPT, &["Guest"; 2])
        .answering(GUEST_PASSWORD_PROMPT, &[GUEST_PASSWORD; 2])
        .answering(GUEST_PASSWORD_AGAIN_PROMPT, &[GUEST_PASSWORD; 2])
        .answering(RESET_GUEST_PROMPT, &[GUEST_RESET_PASSWORD])
        .answering(RESET_GUEST_AGAIN_PROMPT, &[GUEST_RESET_PASSWORD])
        // beize-account passwd twice: with a wrong current password, then
        // with the right one.
        .answering(CURRENT_PASSWORD_PROMPT, &[WRONG_PASSWORD, SMOKE_PASSWORD])
        .answering(NEW_PASSWORD_PROMPT, &[CHANGED_PASSWORD; 2])
        .answering(NEW_PASSWORD_AGAIN_PROMPT, &[CHANGED_PASSWORD; 2]);
    let shell = Shell::boot(&machine, &answers, &OWNER_CONSOLE_CREDENTIALS, false)?;
    // How long the login took, on the guest's clock: from the start of the
    // serial getty to the start of the owner's shell (field 22 of its stat,
    // in clock ticks of 10 ms on x86).
    let (_, lines) = shell.capture(
        "read -r -a s < /proc/$$/stat; echo BEIZE-LOGIN \
         $(systemctl show -P ExecMainStartTimestampMonotonic serial-getty@ttyS0.service) ${s[21]}",
    )?;
    let login_ms = lines
        .iter()
        .find_map(|l| {
            let mut fields = l.strip_prefix("BEIZE-LOGIN ")?.split(' ');
            let getty_us: u64 = fields.next()?.parse().ok()?;
            let shell_ticks: u64 = fields.next()?.parse().ok()?;
            (shell_ticks * 10).checked_sub(getty_us / 1000)
        })
        .with_context(|| format!("could not measure the login: {lines:?}"))?;

    let started = Instant::now();
    let (status, lines) = shell.capture("run0 bash -c 'echo BEIZE-UID-$(id -u)'")?;
    let run0_ms = started.elapsed().saturating_sub(PASSWORD_PAUSE).as_millis();
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-UID-0")),
        "run0 with the owner's password did not run as root (exit status {status}): {lines:?}"
    );
    ensure!(
        lines
            .iter()
            .any(|l| l.contains(&format!("Authenticating as: Smoke Test ({SMOKE_USER})"))),
        "polkit did not ask for the owner's own password: {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains("Unknown line")),
        "polkit's agent got the helper's error messages: {lines:?}"
    );
    say!("  ok: run0 runs as root with the owner's password");
    say!(
        "  measured: login {login_ms} ms (getty to shell), run0 with the password \
         {run0_ms} ms (without typing)"
    );
    // Nobody logs in as root: no password matches its entry.
    let (_, lines) = shell.capture("run0 getent shadow root")?;
    ensure!(
        lines.iter().any(|l| l.starts_with("root:!*:")),
        "root's password is not locked: {lines:?}"
    );
    say!("  ok: root has no password, only run0 gives root");

    // Mount options of / and /usr (field 6 of mountinfo), without grep.
    let (_, lines) = shell.capture(
        "while read -r _ _ _ _ m o _; do case $m in /|/usr) echo \"BEIZE-MOUNT $m $o\";; esac; \
         done < /proc/self/mountinfo",
    )?;
    for mount in ["/", "/usr"] {
        let options = lines
            .iter()
            .find_map(|l| l.strip_prefix(&format!("BEIZE-MOUNT {mount} ")))
            .with_context(|| format!("{mount} is not mounted: {lines:?}"))?;
        let options: Vec<&str> = options.split(',').collect();
        ensure!(
            options.contains(&"nosuid") && options.contains(&"nodev"),
            "{mount} is mounted without nosuid,nodev: {options:?}"
        );
    }
    // The counter-check: as root, a setuid copy of id and a copy of cat with
    // the capability to read any file, both in /var.
    let (status, lines) = shell.capture(
        "run0 bash -c 'cp /usr/bin/id /usr/bin/cat /var/tmp/ && chmod 4755 /var/tmp/id \
         && setcap cap_dac_read_search=ep /var/tmp/cat && echo BEIZE-PREPARED'",
    )?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-PREPARED")),
        "could not prepare the setuid and capability files (exit status {status}): {lines:?}"
    );
    // The same as the owner's own, whatever ID the account service gave.
    let (_, lines) = shell.capture("echo BEIZE-SETUID-$(/var/tmp/id -u)-$(id -u)")?;
    ensure!(
        lines.iter().any(|l| {
            l.strip_prefix("BEIZE-SETUID-")
                .and_then(|ids| ids.split_once('-'))
                .is_some_and(|(setuid, own)| setuid == own && own != "0")
        }),
        "a setuid-root file in /var ran with other rights than the owner's: {lines:?}"
    );
    let (status, _) = shell.capture("/var/tmp/cat /etc/shadow > /dev/null")?;
    ensure!(
        status != 0,
        "a file with a capability in /var read /etc/shadow for the owner"
    );
    say!(
        "  ok: / and /usr are mounted nosuid,nodev; a setuid file and a file capability \
         in /var grant nothing"
    );
    // Units' system call filters are in force (systemd with libseccomp):
    // mode 2 is "filter" in /proc/<pid>/status.
    for unit in ["systemd-journald", "ntpd-rs"] {
        let (_, lines) = shell.capture(&format!(
            "while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
             done < /proc/$(systemctl show -P MainPID {unit})/status"
        ))?;
        ensure!(
            lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
            "{unit} runs without its system call filter: {lines:?}"
        );
    }
    say!("  ok: units run with their system call filters (seccomp)");
    // ping and tracepath as the owner, with neither setuid nor capabilities:
    // ICMP datagram sockets (net.ipv4.ping_group_range).
    for command in [
        "ping -c 1 -W 2 127.0.0.1",
        "ping -c 1 -W 2 ::1",
        "tracepath -n -m 2 127.0.0.1",
    ] {
        let (status, lines) = shell.capture(&format!("{command} > /dev/null"))?;
        ensure!(
            status == 0,
            "`{command}` failed for the owner (exit status {status}): {lines:?}"
        );
    }
    say!("  ok: the owner pings (IPv4, IPv6) and traces a path without privileges");
    // lsblk as the owner shows what udev knows (recipes/lsblk.toml).
    let (_, lines) = shell.capture("lsblk -n -o PARTLABEL,FSTYPE")?;
    ensure!(
        lines
            .iter()
            .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["root", "crypto_LUKS"]),
        "lsblk shows the owner no partition names and file systems: {lines:?}"
    );
    say!("  ok: lsblk shows the owner partition names and file systems");
    // A crash of the owner's program, reported to the owner with its stack
    // trace (ADR 0023): systemd-coredump unwinds it with libdw. main in
    // sleep and __libc_start_call_main in glibc are in no dynamic symbol
    // table; only MiniDebugInfo names them.
    let (_, lines) = shell.capture(
        "/usr/bin/sleep 600 & p=$!; /usr/bin/sleep 1; kill -SEGV $p; wait $p; \
         for _ in {1..40}; do coredumpctl info --no-pager $p > /tmp/crash 2>&1 && break; \
         /usr/bin/sleep 0.25; done; cat /tmp/crash",
    )?;
    let frame = |function: &str, object: &str| {
        lines
            .iter()
            .any(|l| l.contains(&format!(" {function} ({object} + 0x")))
    };
    ensure!(
        frame("main", "sleep") && frame("__libc_start_call_main", "libc.so.6"),
        "the owner's crashed program has no stack trace with its functions' names: {lines:?}"
    );
    say!("  ok: a crash of the owner's program comes with a stack trace that names its functions");

    // Accounts (ADR 0019), through Beize's account service: the owner
    // changes their own password – a wrong current one changes nothing.
    let (status, lines) = shell.capture("beize-account passwd")?;
    ensure!(
        status != 0
            && lines
                .iter()
                .any(|l| l.contains("That password is not right.")),
        "beize-account passwd changed the password with a wrong current one \
         (exit status {status}): {lines:?}"
    );
    let started = Instant::now();
    let (status, lines) = shell.capture("beize-account passwd")?;
    let passwd_ms = started.elapsed().as_millis();
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("Password changed.")),
        "beize-account passwd did not change the owner's password (exit status {status}): \
         {lines:?}"
    );
    let (status, lines) = shell.capture("run0 bash -c 'echo BEIZE-NEW-$(id -u)'")?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("BEIZE-NEW-0")),
        "run0 did not take the owner's new password (exit status {status}): {lines:?}"
    );
    say!("  ok: the owner changed their password with beize-account; run0 takes the new one");
    // An administrator creates an account – not with a wrong password of
    // their own – and resets its password, as on macOS.
    let (status, lines) = shell.capture("beize-account add guest")?;
    ensure!(
        status != 0
            && lines
                .iter()
                .any(|l| l.contains("That password is not right.")),
        "an account was created without the administrator's password \
         (exit status {status}): {lines:?}"
    );
    let (status, lines) = shell.capture("beize-account add guest")?;
    ensure!(
        status == 0 && lines.iter().any(|l| l.contains("Account guest created.")),
        "the owner could not create an account (exit status {status}): {lines:?}"
    );
    let (status, lines) = shell.capture("beize-account reset guest")?;
    ensure!(
        status == 0
            && lines
                .iter()
                .any(|l| l.contains("New password set for guest.")),
        "the owner could not reset the guest's password (exit status {status}): {lines:?}"
    );
    // PAM's own check (as root, which may read the hash) takes the new one.
    let (_, lines) = shell.capture(&format!(
        "run0 bash -c \"printf '%s\\0' {GUEST_RESET_PASSWORD} | unix_chkpwd guest nullok \
         && echo BEIZE-GUEST-OK\""
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-GUEST-OK"),
        "the guest's reset password does not unlock the account: {lines:?}"
    );
    say!(
        "  ok: an administrator creates an account and resets its password, only with their \
         own password"
    );
    // Last (see the answers above): a wrong password gives nothing.
    let (status, lines) = shell.capture("run0 id -u")?;
    ensure!(
        status != 0 && lines.iter().any(|l| l.contains("AUTHENTICATION FAILED")),
        "run0 did not refuse a wrong password (exit status {status}): {lines:?}"
    );
    ensure!(
        !lines.iter().any(|l| l.contains(WRONG_PASSWORD)),
        "the password was shown while it was typed: {lines:?}"
    );
    say!("  ok: run0 refuses a wrong password");
    say!("  measured: password change {passwd_ms} ms (with typing)");
    shell.stop()?;
    machine.remove()
}
