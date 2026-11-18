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
    let screen = screen_size(&machine)?;
    click_while_minimizing(&shell, &qmp, screen)?;
    open_from_dock(&shell, &qmp, screen)?;
    click_while_snapping(&shell, &qmp, screen)?;
    key_while_menu_fades(&shell, &qmp, screen)?;
    key_while_control_center_grows(&shell, &qmp, screen)?;
    journal.passed = true;
    Ok(())
}

/// The dock starts the terminal, which does not run: with the shell's
/// activation token in its transient unit, its window grows out of its
/// symbol (ADR 0032 §4) and has the keyboard.
fn open_from_dock(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let apps = last_dock(shell)?;
    let tile = apps
        .iter()
        .position(|a| a == "beize-terminal")
        .with_context(|| format!("the terminal is not in the dock, or runs: {apps:?}"))?;
    click(qmp, screen, dock_tile(screen, apps.len(), tile)?, 1)?;
    park(qmp, screen)?;
    wait_for_compositor_line(shell, "activated beize-terminal")?;
    wait_for_compositor_line(shell, "animation: beize-terminal opens from its symbol")?;
    wait_for_compositor_line(shell, "animation: beize-terminal opened")?;
    ensure!(
        said(shell, "animation: beize-terminal opens")? == 0,
        "the terminal grew in where it is as well"
    );
    let (_, lines) = shell.capture(
        "systemctl --user list-units 'app-beize\\x2dterminal@*' --state=active --plain \
         --no-legend | wc -l",
    )?;
    ensure!(
        lines.iter().any(|l| l.trim() == "1"),
        "the terminal does not run in a unit of its own: {lines:?}"
    );
    say!("  ok: the dock started the terminal in a unit of its own, it grew out of its symbol");
    shell.capture("systemctl --user stop 'app-beize\\x2dterminal@*'")?;
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

/// Waits until the compositor said `line` `count` times this boot.
fn wait_for_count(shell: &Shell, line: &str, count: usize) -> Result<()> {
    for _ in 0..300 {
        if said(shell, line)? >= count {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("the compositor did not say \"{line}\" {count} times")
}

/// Starts the test client in the unit `unit`, in the compositor's frame,
/// and waits until its window opened: where its content lies.
fn opened_window(shell: &Shell, unit: &str) -> Result<Window> {
    let before = said(shell, "animation: beize-testclient opened")?;
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit={unit} /run/t/beize-testclient --server-side --seconds 300"
    ))?;
    ensure!(
        status == 0,
        "could not start the test client (exit status {status}): {lines:?}"
    );
    wait_for_count(shell, "animation: beize-testclient opened", before + 1)?;
    last_placed(shell, "beize-testclient")
}

/// A click while a window goes into its symbol in the dock reaches the
/// window below: minimized, the window is gone from the screen at once,
/// only its picture moves (ADR 0032 §2). Then it goes into its symbol, and
/// the dock brings it back out of it.
fn click_while_minimizing(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let below = opened_window(shell, "animations-below")?;
    // It opens a cascade's step down and right of the first: over it.
    let front = opened_window(shell, "animations-front")?;
    let ((bx, by), (bw, bh)) = below;
    let ((fx, fy), (fw, fh)) = front;
    let (left, top) = (bx.max(fx), by.max(fy));
    let (right, bottom) = ((bx + bw).min(fx + fw), (by + bh).min(fy + fh));
    ensure!(
        left + 40 < right && top + 40 < bottom,
        "the two windows do not overlap: {below:?}, {front:?}"
    );
    let over_both = (
        i32::try_from((left + right) / 2)?,
        i32::try_from((top + bottom) / 2)?,
    );
    // The test client's content lies below the compositor's title bar.
    let minimize = (
        i32::try_from(fx + CLOSE_X + CIRCLE_STEP)?,
        i32::try_from(fy.saturating_sub(TITLE_BAR / 2))?,
    );
    click(qmp, screen, minimize, 1)?;
    wait_for_compositor_line(shell, "animation: beize-testclient minimizes")?;
    click(qmp, screen, over_both, 1)?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do journalctl --user -u animations-below -o cat \
         | grep -q '^button 0x110 pressed' && break; sleep 0.2; done; \
         journalctl --user -u animations-below -o cat | grep '^button '",
    )?;
    let minimized = said(shell, "animation: beize-testclient minimized")?;
    ensure!(
        lines.iter().any(|l| l == "button 0x110 pressed"),
        "a click while the window went into the dock did not reach the window below: {lines:?}"
    );
    ensure!(
        minimized == 0,
        "the window was in the dock before the click reached the one below: the check did not \
         run while it went"
    );
    let (_, lines) =
        shell.capture("journalctl --user -u animations-front -o cat | grep '^button '")?;
    ensure!(
        !lines.iter().any(|l| l == "button 0x110 pressed"),
        "the click while the window went into the dock reached it: {lines:?}"
    );
    wait_for_compositor_line(shell, "animation: beize-testclient minimized")?;
    say!(
        "  ok: a click while a window went into the dock reached the window below, then it was \
         in its symbol"
    );
    // With the other window gone, the dock brings the minimized one back.
    // Stopped, its client is gone at once: its window goes without an
    // animation (ADR 0032 §2); `stop` returns once its process ended.
    let (status, lines) = shell.capture(
        "systemctl --user stop animations-below; systemctl --user is-active animations-below",
    )?;
    ensure!(
        status != 0 && !lines.iter().any(|l| l == "active"),
        "the window below did not end: {lines:?}"
    );
    let apps = last_dock(shell)?;
    let tile = apps
        .iter()
        .position(|a| a == "beize-testclient running")
        .with_context(|| format!("the test client is not in the dock: {apps:?}"))?;
    click(qmp, screen, dock_tile(screen, apps.len(), tile)?, 1)?;
    park(qmp, screen)?;
    wait_for_compositor_line(shell, "animation: beize-testclient comes back")?;
    wait_for_compositor_line(shell, "animation: beize-testclient came back")?;
    say!("  ok: the dock brought the window back out of its symbol");
    shell.capture("systemctl --user stop animations-front")?;
    Ok(())
}

/// A click while a window snaps to the left half, where only its new place
/// is, reaches it: the model has the new place with the commit of its new
/// size, only the frame drawn springs there (ADR 0032 §2, §5). Then the
/// frame rests.
fn click_while_snapping(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let ((x, y), (_, h)) = opened_window(shell, "animations-snap")?;
    // Inside the left half (10 px from the screen's edge, `SNAP_MARGIN`),
    // left of where the window is.
    let only_new = (30, y + h / 2);
    ensure!(
        only_new.0 + 20 < x,
        "the window reaches the left edge already: at {x},{y}"
    );
    keys(qmp, &["meta_l+left"])?;
    wait_for_compositor_line(shell, "snapped beize-testclient Left")?;
    wait_for_compositor_line(shell, "animation: beize-testclient moves")?;
    click(
        qmp,
        screen,
        (i32::try_from(only_new.0)?, i32::try_from(only_new.1)?),
        1,
    )?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do journalctl --user -u animations-snap -o cat \
         | grep -q '^button 0x110 pressed' && break; sleep 0.2; done; \
         journalctl --user -u animations-snap -o cat | grep '^button '",
    )?;
    let moved = said(shell, "animation: beize-testclient moved")?;
    ensure!(
        lines.iter().any(|l| l == "button 0x110 pressed"),
        "a click on its new place while the window snapped did not reach it: {lines:?}"
    );
    ensure!(
        moved == 0,
        "the frame rested before the click reached the window: the check did not run while it \
         moved"
    );
    wait_for_compositor_line(shell, "animation: beize-testclient moved")?;
    say!("  ok: a click on its new place while a window snapped reached it, then its frame rested");
    shell.capture("systemctl --user stop animations-snap")?;
    Ok(())
}

/// A key pressed while the Beize menu fades out reaches the window: the
/// menu's popup and its grab are gone at once, only its picture fades
/// (ADR 0032 §2, §5). Then the fading ends.
fn key_while_menu_fades(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    opened_window(shell, "animations-menu")?;
    let (beize, _) = super::menus::bar_buttons(shell)?;
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 1)?;
    keys(qmp, &["esc"])?;
    wait_for_compositor_line(shell, "animation: menu closes")?;
    keys(qmp, &["a"])?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do journalctl --user -u animations-menu -o cat \
         | grep -q '^key 30 pressed' && break; sleep 0.2; done; \
         journalctl --user -u animations-menu -o cat | grep '^key '",
    )?;
    let closed = said(shell, "animation: menu closed")?;
    ensure!(
        lines.iter().any(|l| l == "key 30 pressed"),
        "a key pressed while the menu faded did not reach the window: {lines:?}"
    );
    ensure!(
        closed == 0,
        "the menu had faded before the key reached the window: the check did not run while it \
         faded"
    );
    wait_for_compositor_line(shell, "animation: menu closed")?;
    say!("  ok: a key pressed while the Beize menu faded out reached the window, then it ended");
    shell.capture("systemctl --user stop animations-menu")?;
    Ok(())
}

/// Escape while the control center grows out of its button closes it: it
/// has the keyboard from its first frame on (ADR 0032 §2); it goes back
/// into its button from where it had grown, and that ends.
fn key_while_control_center_grows(shell: &Shell, qmp: &Path, screen: (usize, usize)) -> Result<()> {
    let (_, centre) = super::menus::bar_buttons(shell)?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 1)?;
    wait_for_compositor_line(shell, "animation: control center appears")?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 1)?;
    ensure!(
        said(shell, "animation: control center appeared")? == 0,
        "the control center had grown before Escape reached it: the check did not run while it \
         grew"
    );
    wait_for_compositor_line(shell, "animation: control center closes")?;
    wait_for_compositor_line(shell, "animation: control center closed")?;
    say!(
        "  ok: Escape while the control center grew out of its button closed it, and it went \
         back into it"
    );
    Ok(())
}
