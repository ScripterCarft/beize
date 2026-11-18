//! The authentication dialog (ADR 0031): the shell is polkit's agent for the
//! user's display session, tty1 (logind elects it when beize-session makes it
//! a Wayland session, recipes/patches/systemd). `run0 true` in the desktop's
//! terminal asks in the dialog: a wrong password keeps it open and says so,
//! the right one lets run0 run, Escape lets it fail. A window opening while
//! it asks opens behind, without the keyboard. No password lands in any
//! journal. Counter-checks: the serial console's run0 still asks there (its
//! session is not the agent's, `layout_follows` answers it); the right
//! password with a cookie polkitd never gave out gets the helper's FAILURE;
//! no one but polkitd may call the agent.

use super::*;

/// What the shell says when the dialog opens for the terminal's run0: the
/// app, and systemd's sentence for the job with run0's unit
/// (`polkit_message_for_job`), which differs every time.
const ASKS: (&str, &str) = (
    "authentication: Terminal | Terminal wants to start transient unit 'run-",
    ".service'. | Enter your password to allow this.",
);

/// How often the shell said it asks for the terminal's run0.
fn asked(shell: &Shell) -> Result<usize> {
    let (_, lines) = shell.capture(SHELL_SAID)?;
    Ok(lines
        .iter()
        .filter(|l| l.starts_with(ASKS.0) && l.ends_with(ASKS.1))
        .count())
}
/// The script the terminal runs: run0, its exit status appended to a file.
const SCRIPT: &str = "auth-test";
const WRONG: &str = "wrong-password";
/// The compositor's line for a window that opened behind (ADR 0030).
const BEHIND: &str = "opens behind: Beize holds the keyboard";

pub(super) fn authentication(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    // The agent is the display session's, and that is tty1.
    let agent = wait_for_shell_line(shell, |l| l.starts_with("polkit agent for session "))?;
    let (_, said) = shell.capture(
        "d=$(loginctl show-user $(id -u) -p Display --value); echo \"BEIZE-SAID $d \
         $(loginctl show-session $d -p TTY --value)\"",
    )?;
    let display = said
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SAID "))
        .unwrap_or_default()
        .to_owned();
    let (session, tty) = display.split_once(' ').unwrap_or_default();
    ensure!(
        tty == "tty1" && agent == format!("polkit agent for session {session}"),
        "the shell's polkit agent is not tty1's: {agent:?}, display {display:?}"
    );

    let (status, lines) = shell.capture(&format!(
        "printf 'run0 true; echo run0 $? >> %s/{SCRIPT}.out\\n' \"$HOME\" > ~/{SCRIPT} \
         && rm -f ~/{SCRIPT}.out"
    ))?;
    ensure!(status == 0, "could not write the script: {lines:?}");
    let outcomes = || -> Result<Vec<String>> {
        let (_, lines) = shell.capture(&format!("sed 's/^/BEIZE-SAID /' ~/{SCRIPT}.out"))?;
        Ok(lines
            .iter()
            .filter_map(|l| l.strip_prefix("BEIZE-SAID "))
            .map(str::to_owned)
            .collect())
    };
    let run = |n: usize| -> Result<()> {
        // Into the terminal, near its top left, away from the dialog.
        let ((x, y), _) = last_placed(shell, "beize-terminal")?;
        click(
            qmp,
            screen,
            (i32::try_from(x + 40)?, i32::try_from(y + TITLE_BAR + 40)?),
            1,
        )?;
        thread::sleep(Duration::from_millis(300));
        keys(qmp, &german_presses(&format!("bash {SCRIPT}"))?)?;
        wait_for(|| Ok(asked(shell)? >= n))
            .with_context(|| format!("the dialog did not ask for run0 the {n}. time"))
    };

    run(1)?;
    // The lock carries the terminal's own icon, as the dock draws it.
    wait_for_shell_count(shell, "authentication badge: terminal-bold", 1)?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "auth")?.write_png(&machine.logs.join("auth.png"))?;
    // A wrong password: the dialog stays and says so, the field empty – the
    // right one alone then has to pass.
    keys(qmp, &german_presses(WRONG)?)?;
    wait_for_shell_count(shell, "authentication: failed", 1)?;
    // With the verdict it shook, and the shake ended (ADR 0032).
    wait_for_compositor_line(shell, "animation: dialog shakes")?;
    wait_for_compositor_line(shell, "animation: dialog shook")?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "auth-failed")?.write_png(&machine.logs.join("auth-failed.png"))?;
    ensure!(
        shell_count(shell, "authentication: allowed")? == 0 && outcomes()?.is_empty(),
        "a wrong password let run0 run"
    );
    keys(qmp, &german_presses(SMOKE_PASSWORD)?)?;
    wait_for_shell_count(shell, "authentication: allowed", 1)?;
    let ran = wait_for(|| Ok(outcomes()?.len() == 1))
        .and_then(|()| outcomes())
        .context("run0 did not end after the dialog allowed it")?;
    ensure!(
        ran == ["run0 0"],
        "run0 did not run with the password from the dialog: {ran:?}"
    );
    // It appeared and went as a dialog, as the shell marked it.
    wait_for_compositor_line(shell, "animation: dialog appeared")?;
    wait_for_compositor_line(shell, "animation: dialog closed")?;
    say!(
        "  ok: run0 in the terminal asked in the dialog (\"{}…{}\"); a wrong password kept \
         it open and shook it, the right one alone let run0 run; it appeared and went animated",
        ASKS.0,
        ASKS.1
    );

    // Asking again; a window opening meanwhile opens behind, without the
    // keyboard; Escape then lets run0 fail.
    run(2)?;
    let behind = |shell: &Shell| -> Result<usize> {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -b -u beize-compositor.service -o cat | grep -c '{BEHIND}' \
             | sed 's/^/BEIZE-COUNT /'"
        ))?;
        lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .context("the windows opened behind were not counted")
    };
    let before = behind(shell)?;
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit=auth-behind /run/t/beize-testclient --seconds 30; \
         for i in $(seq 300); do [ $(journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '{BEHIND}') -gt {before} ] && break; sleep 0.2; done; \
         journalctl --user -u auth-behind -o cat | grep -E '^(configure|keyboard focus)'; \
         systemctl --user stop auth-behind"
    ))?;
    ensure!(
        behind(shell)? == before + 1
            && lines.iter().any(|l| l.starts_with("configure "))
            && !lines.iter().any(|l| l.starts_with("keyboard focus")),
        "a window opening during the dialog did not open behind without the keyboard: {lines:?}"
    );
    // Its keys went nowhere else: the dialog is still open.
    ensure!(
        shell_count(shell, "authentication: cancelled")? == 0,
        "the dialog closed when a window opened"
    );
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "authentication: cancelled", 1)?;
    let ran = wait_for(|| Ok(outcomes()?.len() == 2))
        .and_then(|()| outcomes())
        .context("run0 did not end after Escape")?;
    ensure!(
        ran.get(1).is_some_and(|l| l != "run0 0"),
        "run0 ran although the dialog was cancelled: {ran:?}"
    );
    say!(
        "  ok: a window opening during the dialog opened behind without the keyboard; Escape \
         let run0 fail"
    );

    // No password in any journal of this boot – the system's too, which the
    // owner reads as a member of wheel (polkit's helper writes there).
    let (_, lines) = shell.capture(&format!(
        "echo BEIZE-HELPER $(journalctl -b -o cat -u 'polkit-agent-helper@*' | wc -l); \
         echo BEIZE-FOUND $(journalctl -b -o cat | grep -c -e '{SMOKE_PASSWORD}' -e '{WRONG}')"
    ))?;
    let number = |prefix: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(prefix)?.trim().parse::<usize>().ok())
    };
    ensure!(
        number("BEIZE-HELPER ").is_some_and(|n| n > 0),
        "the system journal with polkit's helper is not readable here: {lines:?}"
    );
    ensure!(
        number("BEIZE-FOUND ") == Some(0),
        "a password is in this boot's journal: {lines:?}"
    );
    say!("  ok: neither password is in any journal of this boot");

    // Counter-checks. The right password with a cookie polkitd never gave
    // out: the helper says FAILURE – the password alone allows nothing.
    let (_, lines) = shell.capture(&format!(
        "printf '%s\\n' '{SMOKE_PASSWORD}' | /run/t/beize-testclient --polkit-helper \
         beize-no-such-cookie"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "helper: FAILURE"),
        "polkit's helper did not refuse a cookie polkitd never gave out: {lines:?}"
    );
    // Only polkitd may call the agent (polkit's bus policy): the owner may not.
    let (_, lines) = shell.capture(
        "n=$(busctl --system --no-legend list | awk '$3 == \"beize-shell\" { print $1; exit }'); \
         echo BEIZE-NAME $n; busctl --system call \"$n\" \
         /org/freedesktop/PolicyKit1/AuthenticationAgent \
         org.freedesktop.PolicyKit1.AuthenticationAgent CancelAuthentication s x 2>&1",
    )?;
    ensure!(
        lines
            .iter()
            .any(|l| l.starts_with("BEIZE-NAME :") && l.len() > "BEIZE-NAME :".len())
            && lines.iter().any(|l| l.contains("Access denied")),
        "the owner could call the shell's polkit agent: {lines:?}"
    );
    say!(
        "  ok: counter-checks – the right password with a made-up cookie gets the helper's \
         FAILURE; the owner may not call the agent, only polkitd"
    );
    Ok(())
}

/// Waits until `done`, as long as a screen may take.
fn wait_for(mut done: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    while !done()? {
        ensure!(Instant::now() < deadline, "not in time");
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}
