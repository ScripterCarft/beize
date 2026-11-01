//! Waiting on what the desktop says: its journal lines, windows placed,
//! terminals, the dock.

use super::*;

/// How often the shell said `line`.
pub(super) fn shell_count(shell: &Shell, line: &str) -> Result<usize> {
    let (_, lines) = shell.capture(SHELL_SAID)?;
    Ok(lines.iter().filter(|l| *l == line).count())
}

/// Waits until the shell said `line` at least `count` times in this boot.
pub(super) fn wait_for_shell_count(shell: &Shell, line: &str, count: usize) -> Result<()> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let (_, lines) = shell.capture(SHELL_SAID)?;
        let said = lines.iter().filter(|l| *l == line).count();
        if said >= count {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the shell said {line:?} {said} times, not {count}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// The terminals in the owner's service manager, units of the template
/// `app-beize\x2dterminal@.service`, with the number of processes in each
/// (the terminal, and its shell once it has a window).
pub(super) fn terminals(shell: &Shell) -> Result<Vec<(String, usize)>> {
    let (_, lines) = shell.capture(
        "systemctl --user list-units 'app-beize\\x2dterminal@*' --state=active --plain \
         --no-legend | while read -r u _; do \
         echo \"BEIZE-TERMINAL $u $(grep -c . /sys/fs/cgroup$(systemctl --user show -P \
         ControlGroup \"$u\")/cgroup.procs)\"; done",
    )?;
    Ok(lines
        .iter()
        .filter_map(|l| {
            let (unit, count) = l.strip_prefix("BEIZE-TERMINAL ")?.split_once(' ')?;
            Some((unit.to_owned(), count.parse().ok()?))
        })
        .collect())
}

/// `(x, y)` moved by `(dx, dy)`: where the pointer goes.
pub(super) fn point((x, y): (usize, usize), (dx, dy): (i32, i32)) -> Result<(i32, i32)> {
    Ok((i32::try_from(x)? + dx, i32::try_from(y)? + dy))
}

/// The pointer's position `at` moved by `(dx, dy)`.
pub(super) fn by((x, y): (i32, i32), (dx, dy): (i32, i32)) -> (i32, i32) {
    (x + dx, y + dy)
}

/// Waits until `count` terminals run, each with its shell; their units.
pub(super) fn wait_for_terminals(shell: &Shell, count: usize) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for _ in 0..50 {
        found = terminals(shell)?;
        if found.len() == count && found.iter().all(|(_, processes)| *processes >= 2) {
            return Ok(found.into_iter().map(|(unit, _)| unit).collect());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("not {count} terminals with a shell each: {found:?}")
}

/// Waits until the shell in the terminal `unit` reads a line: readline
/// takes the terminal out of canonical mode for that. Keys pressed before
/// it would be echoed by the terminal driver ahead of the prompt.
pub(super) fn wait_for_prompt(shell: &Shell, unit: &str) -> Result<()> {
    let (_, lines) = shell.capture(&format!(
        "d=/sys/fs/cgroup$(systemctl --user show -P ControlGroup '{unit}'); r=; \
         for i in $(seq 300); do for p in $(cat \"$d/cgroup.procs\"); do \
         [ \"$(cat /proc/$p/comm)\" != beize-terminal ] && stty -F /proc/$p/fd/0 -a 2>/dev/null \
         | grep -qw -- -icanon && r=1; done; [ -n \"$r\" ] && break; sleep 0.2; done; \
         echo BEIZE-PROMPT-${{r:-none}}"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-PROMPT-1"),
        "the shell in {unit} never waited for a line: {lines:?}"
    );
    Ok(())
}

/// Where the compositor placed the `count`th window of `app` and its size,
/// waiting for it.
pub(super) fn placed(shell: &Shell, app: &str, count: usize) -> Result<Window> {
    let mut found = Vec::new();
    for _ in 0..50 {
        let (_, lines) = shell.capture(PLACED)?;
        found = lines
            .iter()
            .filter_map(|l| placement(l, app))
            .collect::<Vec<_>>();
        if found.len() >= count {
            return Ok(found[count - 1]);
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "the compositor placed {} windows of {app}, not {count}",
        found.len()
    )
}

/// Where the compositor last placed a window of `app`.
pub(super) fn last_placed(shell: &Shell, app: &str) -> Result<Window> {
    let (_, lines) = shell.capture(PLACED)?;
    lines
        .iter()
        .filter_map(|l| placement(l, app))
        .next_back()
        .with_context(|| format!("the compositor placed no window of {app}"))
}

/// A window: its top left corner on the screen and its size.
pub(super) type Window = ((usize, usize), (usize, usize));

/// The window of `app` that `line` (`window <app> <w>x<h> at <x>,<y>`)
/// names.
pub(super) fn placement(line: &str, app: &str) -> Option<Window> {
    let rest = line.strip_prefix("window ")?.strip_prefix(app)?;
    let (size, at) = rest.trim_start().split_once(" at ")?;
    let (w, h) = size.split_once('x')?;
    let (x, y) = at.split_once(',')?;
    Some((
        (x.parse().ok()?, y.parse().ok()?),
        (w.parse().ok()?, h.parse().ok()?),
    ))
}

/// What `units` last said but the pointer's moves, for a failure: the VM's
/// journal is gone with the VM.
pub(super) fn last_said(shell: &Shell, units: &[&str]) -> Result<Vec<String>> {
    let units: String = units.iter().map(|u| format!(" -u {u}")).collect();
    let (_, said) = shell.capture(&format!(
        "journalctl --user -b -o cat{units} | grep -v '^pointer at' | tail -n 30"
    ))?;
    Ok(said)
}

/// Waits until `app` (a program printing `configure WxH` for each
/// configure, as Beize UI's test program does) was last configured to
/// `size`.
pub(super) fn wait_for_configure(shell: &Shell, app: &str, size: (usize, usize)) -> Result<()> {
    let wanted = format!("configure {}x{}", size.0, size.1);
    let mut last = None;
    for _ in 0..50 {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -u {app} -o cat | grep '^configure ' | tail -n 1"
        ))?;
        last = lines.into_iter().find(|l| l.starts_with("configure "));
        if last.as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("{app} was not configured to {wanted}, last: {last:?}")
}

/// Puts the pointer back on the screen's top left corner, on the menu bar.
pub(super) fn park(qmp: &Path, screen: (usize, usize)) -> Result<()> {
    drag(qmp, screen, (0, 0), (0, 0), false)
}

/// Waits until the shell said a line `wanted` takes, in this boot.
pub(super) fn wait_for_shell_line(shell: &Shell, wanted: impl Fn(&str) -> bool) -> Result<String> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let (_, lines) = shell.capture(SHELL_SAID)?;
        if let Some(line) = lines.iter().find(|l| wanted(l)) {
            return Ok(line.clone());
        }
        ensure!(
            Instant::now() < deadline,
            "the shell did not say what was waited for; it said: {lines:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// The apps of the shell's last `dock:` line.
pub(super) fn last_dock(shell: &Shell) -> Result<Vec<String>> {
    let (_, lines) = shell.capture(SHELL_SAID)?;
    let line = lines
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("dock: "))
        .with_context(|| format!("the shell named no apps in its dock: {lines:?}"))?;
    Ok(line.split(", ").map(str::to_owned).collect())
}

/// Waits until the shell's last `dock:` line has apps that `wanted` takes.
pub(super) fn wait_for_dock(
    shell: &Shell,
    wanted: impl Fn(&[String]) -> bool,
) -> Result<Vec<String>> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        let apps = last_dock(shell)?;
        if wanted(&apps) {
            return Ok(apps);
        }
        ensure!(
            Instant::now() < deadline,
            "the dock did not become what was waited for: {apps:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

pub(super) fn wait_for_compositor_line(shell: &Shell, line: &str) -> Result<String> {
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 300); do journalctl --user -b -u beize-compositor.service -o cat \
         | grep -qx '{line}' && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep -x '{line}' \
         | sed 's/^/BEIZE-SAID /' | tail -n 1"
    ))?;
    if let Some(said) = lines.iter().find_map(|l| l.strip_prefix("BEIZE-SAID ")) {
        return Ok(said.to_owned());
    }
    let (_, last) = shell.capture(
        "journalctl --user -b -u beize-compositor.service -u beize-shell.service \
         -o short-monotonic | grep -v ': pointer at ' | tail -n 30",
    )?;
    bail!(
        "the compositor did not say \"{line}\"; last said:\n{}",
        last.join("\n")
    )
}
