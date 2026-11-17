//! Animations (ADR 0032 §7), on a desktop of their own where every spring
//! is `SLOWER` times longer (`BEIZE_ANIMATIONS_SLOWER`, as GNOME's
//! `slow-down-factor`): "while it runs" is then seconds, longer than a stall
//! of the VM's vCPUs, so the checks never depend on the VM's timing. They
//! check what happens and in which order, by the compositor's journal
//! lines, never how fast (ADR 0027). That no animation wakes the compositor
//! once it ended is the desktop's stillness check (`still_and_signed_out`),
//! after many windows opened and closed there: with animations that never
//! end, the compositor switched 239 times in its 3 s (17.11.).

use super::*;

/// How many times longer every spring is: opening in place then takes
/// some 9 s, closing some 7 s.
const SLOWER: u32 = 20;
/// A run of frames summed up without its end: every 30 s (`pacing`).
const PIECE: f64 = 30.0;

pub(in crate::vm::smoke) fn animations(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-animations")?;
    machine.test_medium = test_medium(paths)?;
    // The sign-in moves the pointer to a place (`console::drag`).
    machine.tablet = true;
    machine.memory_mib = DESKTOP_MEMORY_MIB;
    ensure!(
        machine.test_medium.is_some(),
        "no test medium: the test client is missing"
    );
    // run0 asks for the owner's password: to unplug the screen before the
    // login and plug it in after, and to mount the test medium.
    let shell = Shell::boot(
        &machine,
        &Answers::passwords(&[SMOKE_PASSWORD; 3]),
        &OWNER_CONSOLE_CREDENTIALS,
        false,
    )?;
    let mut journal = JournalOnFailure {
        shell: &shell,
        passed: false,
    };
    let qmp = machine.qmp();
    let (status, lines) = shell.capture(&format!(
        "systemctl --user set-environment BEIZE_ANIMATIONS_SLOWER={SLOWER}"
    ))?;
    ensure!(status == 0, "could not slow the animations down: {lines:?}");
    sign_in(&shell, &machine, &qmp)?;
    wait_for_compositor_line(&shell, &format!("animations {SLOWER} times slower"))?;
    key_while_opening(&shell, &qmp)?;
    closing(&shell)?;
    journal.passed = true;
    Ok(())
}

/// How often the compositor said `line` (a pattern, as grep's) this boot.
fn said(shell: &Shell, line: &str) -> Result<usize> {
    let (_, lines) = shell.capture(&format!(
        "echo BEIZE-COUNT-$(journalctl --user -b -u beize-compositor.service -o cat \
         | grep -cx '{line}')"
    ))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-COUNT-")?.trim().parse().ok())
        .with_context(|| format!("could not count \"{line}\": {lines:?}"))
}

/// A key pressed while a window opens reaches it: the window has the
/// keyboard from its first frame on, the animation is only what is drawn
/// (ADR 0032 §2). Then the animation ends.
fn key_while_opening(shell: &Shell, qmp: &Path) -> Result<()> {
    let (_, lines) = shell.capture(
        "systemd-run --user --unit=animations-open /run/t/beize-testclient --seconds 120 && \
         for i in $(seq 300); do journalctl --user -u animations-open -o cat \
         | grep -q '^keyboard focus' && break; sleep 0.2; done; \
         journalctl --user -u animations-open -o cat | grep '^keyboard focus'",
    )?;
    ensure!(
        lines.iter().any(|l| l == "keyboard focus"),
        "the window opening got no keyboard: {lines:?}"
    );
    ensure!(
        said(shell, "animation: beize-testclient opens")? == 1,
        "the compositor did not say the window opens"
    );
    keys(qmp, &["a"])?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do journalctl --user -u animations-open -o cat \
         | grep -q '^key 30 pressed' && break; sleep 0.2; done; \
         journalctl --user -u animations-open -o cat | grep '^key '",
    )?;
    let opened = said(shell, "animation: beize-testclient opened")?;
    ensure!(
        lines.iter().any(|l| l == "key 30 pressed"),
        "a key pressed while the window opened did not reach it: {lines:?}"
    );
    ensure!(
        opened == 0,
        "the window had opened before the key reached it: the check did not run while it opened"
    );
    wait_for_compositor_line(shell, "animation: beize-testclient opened")?;
    say!("  ok: a key pressed while a window opened reached it, then the opening ended");
    shell.capture("systemctl --user stop animations-open")?;
    Ok(())
}

/// A window that closes while it opens leaves its picture, which fades from
/// where the opening was and ends; its run of frames ends with it, summed
/// up, not every 30 s.
fn closing(shell: &Shell) -> Result<()> {
    // It ends itself after 3 s, as an app closes its window (its toplevel,
    // then its surface): while it still opens, so its picture fades from
    // where its opening was.
    let (status, lines) = shell.capture(
        "systemd-run --user --unit=animations-close /run/t/beize-testclient --seconds 3",
    )?;
    ensure!(
        status == 0,
        "could not start the test client (exit status {status}): {lines:?}"
    );
    wait_for_compositor_line(shell, "animation: beize-testclient stopped opening")?;
    wait_for_compositor_line(shell, "animation: beize-testclient closes")?;
    wait_for_compositor_line(shell, "animation: beize-testclient closed")?;
    // Then still: the run of frames ends, summed up, not every 30 s.
    let (_, lines) = shell.capture(
        "for i in $(seq 50); do journalctl --user -b -u beize-compositor.service -o cat \
         | sed -n '/^animation: beize-testclient closed$/,$p' | grep -q '^pacing: ' && break; \
         sleep 0.2; done; journalctl --user -b -u beize-compositor.service -o cat \
         | sed -n '/^animation: beize-testclient closed$/,$p' | grep '^pacing: ' | head -n 1",
    )?;
    let pacing = lines
        .iter()
        .find(|l| l.starts_with("pacing: "))
        .with_context(|| "the frames did not stop once the closing ended".to_owned())?;
    let seconds: f64 = pacing
        .split(" in ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("no length in the summary: {pacing}"))?;
    ensure!(
        seconds < PIECE - 1.0,
        "the frames went on after the closing ended: {pacing}"
    );
    say!(
        "  ok: a window closed while it opened, its picture faded from there and ended, and \
         the frames stopped: {pacing}"
    );
    Ok(())
}
