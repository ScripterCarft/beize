//! The session: signing in, the screen going and coming back, the shell
//! restarting, the layout, idling, the screen off, signing out; screenshots.

use super::*;

/// The VM's screen, as the kernel and the compositor name its connector.
pub(super) const VM_SCREEN: &str = "Virtual-1";

/// The screen goes and comes back (ADR 0029): unplugged, the compositor
/// lets it go – nothing on it, the CRTC off – and keeps the window; plugged
/// in again, it takes the screen anew and shows the window in its place.
/// The test unplugs it and plugs it in with `off` and `detect` on its
/// connector's status and a change of the GPU's udev device, the event a
/// hotplug sends: virtio-gpu sends none for that itself (the kernel's probe
/// fires one only for drivers that poll,
/// `drm_helper_probe_single_connector_modes`).
pub(super) fn screen_comes_back(
    shell: &Shell,
    machine: &Machine,
    at: (usize, usize),
    size: (usize, usize),
) -> Result<()> {
    let restored = Image::read_ppm(&fs::read(machine.logs.join("restored-screen.ppm"))?)?;
    let shown = restored.crop(at, size)?;
    // Twice, the second time in the mode of the first (virtio-gpu has 75 Hz
    // until probed): the compositor draws the returned screen itself.
    for round in ["", "-again"] {
        set_screen(shell, "off")?;
        wait_for_compositor_line(shell, &format!("screen {VM_SCREEN} is gone"))?;
        wait_for_gone(machine, (at, size), &shown, &format!("screen-gone{round}"))?;
        set_screen(shell, "detect")?;
        wait_for_compositor_line(shell, &format!("screen {VM_SCREEN}: .*, it had none"))?;
        wait_for_pattern(shell, machine, at, size, &format!("screen-back{round}"))?;
    }
    say!(
        "  ok: unplugged, the screen went dark and the window stayed; plugged in, it came back \
         with the window in its place, twice"
    );
    Ok(())
}

/// Unplugs the VM's screen (`off`) or plugs it in (`detect`), as root
/// (`screen_comes_back` says how).
pub(super) fn set_screen(shell: &Shell, status: &str) -> Result<()> {
    let (code, lines) = shell.capture(&format!(
        "run0 sh -c 'c=$(echo /sys/class/drm/card*-{VM_SCREEN}) && echo {status} > \
         $c/status && udevadm trigger --action=change $(dirname $(readlink -f $c))'"
    ))?;
    ensure!(
        code == 0,
        "could not set the screen {status} (exit status {code}): {lines:?}"
    );
    Ok(())
}

/// The owner signs in on tty1 and the desktop starts: a Wayland session, the
/// compositor and the shell sandboxed, the shell known as such, the desktop
/// shown in one piece with its panels; the test medium mounted. Without a
/// screen the compositor waits, and the desktop starts once one comes.
pub(super) fn sign_in(shell: &Shell, machine: &Machine, qmp: &Path) -> Result<()> {
    // With no screen connected, the compositor starts and waits for one;
    // the desktop starts once it comes (ADR 0029).
    set_screen(shell, "off")?;
    // At the keyboard, as the owner would: name, password, beize-session,
    // each when the console has had time to ask.
    for (typed, pause) in [(SMOKE_USER, 1), (SMOKE_PASSWORD, 2), ("beize-session", 0)] {
        keys(qmp, &german_presses(typed)?)?;
        thread::sleep(Duration::from_secs(pause));
    }
    wait_for_compositor_line(shell, "no screen connected: the desktop starts once one is")?;
    // The test's programs start from outside, as no app would, and are to come
    // to the front (`forward`).
    shell.capture("mkdir -p ~/.config/beize && echo beize- > ~/.config/beize/come-forward")?;
    // The mouse moves meanwhile: no screen to keep it on (the compositor once
    // died of this). From a place it is not at: the same place is no event.
    drag(qmp, (1280, 800), (100, 100), (200, 200), false)?;
    let (_, lines) = shell.capture(
        "systemctl --user is-active beize-session.target beize-compositor.service \
         beize-shell.service",
    )?;
    ensure!(
        lines == ["inactive", "activating", "inactive"],
        "without a screen, the compositor should wait and the desktop not start yet \
         (target, compositor, shell): {lines:?}"
    );
    set_screen(shell, "detect")?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do systemctl --user -q is-active beize-session.target && break; \
         sleep 0.2; done; systemctl --user is-active beize-session.target",
    )?;
    ensure!(
        lines.iter().any(|l| l == "active"),
        "the desktop did not start after the login on tty1: {lines:?}"
    );

    // The session: the login on tty1, active, of type wayland (libseat
    // passed beize-session's XDG_SESSION_TYPE on to logind).
    let (_, lines) = shell.capture(
        "s=$(systemctl --user show-environment | sed -n 's/^XDG_SESSION_ID=//p'); \
         loginctl show-session \"$s\" -p TTY -p Active -p Type -p Name",
    )?;
    for wanted in ["TTY=tty1", "Active=yes", "Type=wayland", "Name=smoke"] {
        ensure!(
            lines.iter().any(|l| l == wanted),
            "the desktop's session is not {wanted}: {lines:?}"
        );
    }
    // The compositor: the owner's process, in its system call filter, with
    // room for its clients' buffers (open files; 1024 ran out, 0.5.105).
    let (_, lines) = shell.capture(
        "p=$(systemctl --user show -P MainPID beize-compositor.service); \
         echo BEIZE-OWNER-$(stat -c %U /proc/$p); \
         while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
         done < /proc/$p/status; \
         echo BEIZE-FILES-$(awk '/^Max open files/ {print $4}' /proc/$p/limits)",
    )?;
    ensure!(
        lines
            .iter()
            .any(|l| l == &format!("BEIZE-OWNER-{SMOKE_USER}"))
            && lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
        "the compositor does not run as the owner in its system call filter: {lines:?}"
    );
    ensure!(
        lines.iter().any(|l| l == "BEIZE-FILES-524288"),
        "the compositor may not open 524288 files (its clients' buffers): {lines:?}"
    );
    say!(
        "  ok: the login on tty1 started the desktop: a Wayland session, the compositor as the \
         owner, sandboxed, with room for 524288 open files"
    );
    let (status, lines) = shell.capture(
        "run0 sh -c 'mkdir -p /run/t && mount -o ro /dev/disk/by-id/virtio-beize-test /run/t'",
    )?;
    ensure!(
        status == 0,
        "could not mount the test medium (exit status {status}): {lines:?}"
    );

    // The shell: its own process in its system call filter, which the
    // compositor knows as the shell by its unit; its menu bar on the screen.
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 300); do [ \"$({SHELL_CONNECTED})\" -ge 1 ] && break; sleep 0.2; done; \
         echo BEIZE-SHELL-$({SHELL_CONNECTED}); \
         p=$(systemctl --user show -P MainPID beize-shell.service); \
         while read -r k v; do [ \"$k\" = Seccomp: ] && echo BEIZE-SECCOMP-$v; \
         done < /proc/$p/status"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-SHELL-1") && lines.iter().any(|l| l == "BEIZE-SECCOMP-2"),
        "the shell did not connect once as the shell, in its system call filter: {lines:?}"
    );
    wait_for_panels(machine)?;
    say!(
        "  ok: the shell runs sandboxed, the compositor knows it, its menu bar shows the time, \
         its dock floats along the bottom with round corners"
    );
    // In one piece: the desktop showed once the shell said its panels were
    // drawn, not without them after the wait (a warning, which fails).
    let shown = wait_for_compositor_line(
        shell,
        "desktop shown with the shell, [0-9]* ms after the screen",
    )?;
    say!("  ok: the desktop appeared with the shell's panels: {shown}");

    Ok(())
}

/// A screenshot equals the screen; Ctrl+Alt+T opens terminals, the typed
/// sample shows as its reference has it, the pointer over its text is the
/// text cursor. The second terminal's window.
pub(super) fn screenshot_and_terminals(
    paths: &Paths,
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
) -> Result<Window> {
    // Super+Shift+3 saves the screen as it shows it: the PNG equals QEMU's
    // own screendump of the still desktop.
    let size = screenshot(shell, machine)?;
    say!(
        "  ok: Super+Shift+3 saved a screenshot, {}x{}, as the screen shows it",
        size.0,
        size.1
    );

    // The desktop starts with no terminal, as macOS' with no window;
    // Ctrl+Alt+T opens one and another: each in a unit of its own, with the
    // owner's shell in it.
    let none = terminals(shell)?;
    ensure!(
        none.is_empty(),
        "a terminal opened with the desktop: {none:?}"
    );
    keys(qmp, &["ctrl+alt+t"])?;
    let first = wait_for_terminals(shell, 1)?;
    keys(qmp, &["ctrl+alt+t"])?;
    let both = wait_for_terminals(shell, 2)?;
    ensure!(
        both.contains(&first[0]),
        "the first terminal ended when the second opened: {both:?}"
    );
    wait_for_shell_line(shell, |l| l == "dock: beize-terminal running")?;
    wait_for_shell_line(shell, |l| l == "menu bar: Terminal")?;
    say!(
        "  ok: the desktop started with no terminal, Ctrl+Alt+T opened one and another, each \
         with its shell; the dock shows the terminal running, the menu bar names it"
    );

    // The new terminal has the keyboard: typed into once its window is on
    // the screen (placed, it is active in the same step) and its shell
    // reads, the sample's command runs and the screen shows what it
    // printed. Typed before the window shows, as no one would, the first
    // keys went to the first terminal on a slow VM.
    let second = both
        .iter()
        .find(|u| **u != first[0])
        .context("no second terminal")?;
    let terminal = placed(shell, "beize-terminal", 2)?;
    wait_for_prompt(shell, second)?;
    keys(qmp, &german_presses(TERMINAL_SAMPLE_COMMAND)?)?;
    wait_for_screen(paths, machine, terminal, TERMINAL_SAMPLE, "terminal")?;
    say!(
        "  ok: the typed command ran in the terminal, its window shows the sample as the \
         reference has it"
    );
    // Over its text, the text cursor the terminal names.
    let ((x, y), (w, h)) = terminal;
    let middle = (i32::try_from(x + w / 2)?, i32::try_from(y + h / 2)?);
    drag(qmp, screen_size(machine)?, middle, middle, false)?;
    wait_for_compositor_line(shell, "cursor text: 1 frame at 24 px")?;
    park(qmp, screen_size(machine)?)?;
    say!("  ok: over the terminal's text the pointer is the theme's text cursor");

    Ok(terminal)
}

/// The shell killed and stopped comes back; its panels stay meanwhile and
/// Beize UI's window `ui` with them.
pub(super) fn shell_restarts(
    paths: &Paths,
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    ui: Window,
) -> Result<()> {
    // Killed, the shell starts again (ADR 0028 §1): the compositor knows
    // it once more and keeps the old one's panels until the new one is
    // ready (§6, a warning if it is not within the wait), the bar comes
    // back, the desktop and the window stay.
    let (_, lines) = shell.capture(&format!(
        "systemctl --user kill -s KILL beize-shell.service; \
         for i in $(seq 300); do [ \"$({SHELL_CONNECTED})\" -ge 2 ] && break; sleep 0.2; done; \
         echo BEIZE-SHELL-$({SHELL_CONNECTED}); \
         systemctl --user is-active beize-shell.service beize-compositor.service \
         beize-session.target"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-SHELL-2")
            && lines.iter().filter(|l| *l == "active").count() == 3,
        "the shell did not come back after it was killed, or the desktop went: {lines:?}"
    );
    wait_for_compositor_line(
        shell,
        "the shell failed: its 2 panels stay until a new one is ready",
    )?;
    let replaced = wait_for_compositor_line(
        shell,
        "a new shell is ready [0-9]* ms after the old one failed: its panels replace the kept ones",
    )?;
    wait_for_panels(machine)?;
    wait_for_screen(
        paths,
        machine,
        content(ui),
        UI_SAMPLE,
        "beize-ui-after-shell",
    )?;
    say!(
        "  ok: killed, the shell came back with its menu bar and dock, kept until then; the \
         desktop and the window stayed: {replaced}"
    );
    // Stopped, with no new shell yet, its panels are still on the screen
    // once the compositor has drawn without the shell – it said so, and
    // half a second of frames followed, well within the wait of seconds –,
    // and a shell started then replaces them.
    let (_, lines) = shell.capture(
        "k() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^the shell failed: '; }; systemctl --user stop beize-shell.service; \
         for i in $(seq 300); do [ \"$(k)\" -ge 2 ] && break; sleep 0.1; done; sleep 0.5; \
         echo BEIZE-KEPT-$(k); systemctl --user is-active beize-shell.service",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "BEIZE-KEPT-2"),
        "the shell did not stop, or the compositor did not keep its panels: {lines:?}"
    );
    let file = machine.logs.join("kept-screen.ppm");
    screendump(qmp, &file)?;
    let kept = Image::read_ppm(&fs::read(&file)?)?;
    if let Err(e) = menu_bar(&kept).and_then(|()| dock(&kept).map(|_| ())) {
        let shown = machine.logs.join("kept-screen.png");
        kept.write_png(&shown)?;
        return Err(e.context(format!(
            "the stopped shell's panels did not stay; the screen is {}",
            shown.display()
        )));
    }
    let (_, lines) = shell.capture(
        "r() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^a new shell is ready '; }; systemctl --user start beize-shell.service; \
         for i in $(seq 300); do [ \"$(r)\" -ge 2 ] && break; sleep 0.2; done; \
         echo BEIZE-REPLACED-$(r)",
    )?;
    ensure!(
        lines.iter().any(|l| l == "BEIZE-REPLACED-2"),
        "a shell started after one stopped did not replace its kept panels: {lines:?}"
    );
    wait_for_panels(machine)?;
    say!("  ok: stopped, the shell's panels stayed on the screen until a new one replaced them");

    Ok(())
}

/// The layout set with localectl applies at once, and the next one too:
/// localed renames a new file over the watched one, which is watched anew
/// (`keyboard::Watch`). Back to the owner's layout after.
pub(super) fn layout_follows(shell: &Shell) -> Result<()> {
    let (_, lines) = shell.capture(
        "( n() { journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c '^keyboard layout '; }; \
         w() { for i in $(seq 300); do [ \"$(n)\" -gt \"$1\" ] && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep '^keyboard layout ' \
         | tail -n 1 | sed 's/^/BEIZE-LAYOUT /'; }; \
         . /etc/vconsole.conf; s=$(n); \
         a=$(n); run0 localectl set-x11-keymap --no-convert fr pc105; w $a; \
         a=$(n); run0 localectl set-x11-keymap --no-convert \"$XKBLAYOUT\" \"${XKBMODEL:-pc105}\" \
         \"$XKBVARIANT\" \"$XKBOPTIONS\"; w $a; sleep 0.5; echo BEIZE-APPLIED $(( $(n) - s )) )",
    )?;
    let said: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("BEIZE-LAYOUT "))
        .collect();
    ensure!(
        said.len() == 2 && said[0] == "keyboard layout fr" && said[1] != said[0],
        "the compositor did not follow two layouts set in a row: {lines:?}"
    );
    // localed's one write is two events (`State::reload_layout`).
    ensure!(
        lines.iter().any(|l| l == "BEIZE-APPLIED 2"),
        "the compositor applied two layouts other than twice: {lines:?}"
    );
    say!(
        "  ok: a keyboard layout set with localectl applied at once, and the next one too ({})",
        said[1]
    );
    Ok(())
}

/// No warnings, the still desktop does not wake, and signing out stops it.
pub(super) fn still_and_signed_out(
    shell: &Shell,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let (_, lines) =
        shell.capture("journalctl --user -b -u beize-compositor.service -p warning -o cat")?;
    let unexpected = unexpected_warnings(&lines);
    ensure!(
        unexpected.is_empty(),
        "the compositor warned: {unexpected:?}"
    );
    say!("  ok: no warnings from the compositor (but Smithay's known ones)");

    // Still, nothing of the desktop wakes (ADR 0027 §6: events, no
    // polling): for IDLE_SECONDS the compositor, the shell and the terminal
    // switch to no other task, not one of their threads – measured off the
    // minute, when the menu bar's clock ticks (measured again if one began
    // meanwhile: the run0 below lengthens the window by its password, and
    // once, 27.10., it ended past the minute; again if ntp-daemon set the
    // clock, which rightly wakes the shell's clock, CANCEL_ON_SET: 867 ms on
    // 31.10.), once the terminal's first
    // frames are done – and with no test program left, whose window going
    // by itself would wake them rightly (a test client did, 25.10.) – nor
    // one still stopping. Woken, each thread that switched is named
    // (`BEIZE-WOKE`) with what the journal says meanwhile (`BEIZE-JOURNAL`).
    // Meanwhile the system goes on: a service with DynamicUser= starts, for
    // which systemd takes /etc/.pwd.lock – while the compositor watched all
    // of /etc for its keyboard layout, that woke it (main thread once, in
    // ~1 of 9 runs 25.–27.10., when ntpd-rs or another such service started).
    let (_, left) = shell.capture(
        "systemctl --user list-units --state=active,activating,deactivating,reloading --plain \
         --no-legend 'beize-testclient*' 'beize-ui-test*' 'beize-load-test*' \
         | sed 's/^/BEIZE-LEFT /'",
    )?;
    let left: Vec<&String> = left
        .iter()
        .filter(|l| l.starts_with("BEIZE-LEFT "))
        .collect();
    ensure!(
        left.is_empty(),
        "test programs still run before the desktop is watched still: {left:?}"
    );
    thread::sleep(RUN_ENDED);
    let (_, lines) = shell.capture(&format!(
        "c() {{ cat /proc/$1/task/*/status | awk '/ctxt_switches/ {{s += $2}} END {{print s}}'; }}; \
         for try in 1 2; do \
         while s=$((10#$(date +%S))); [ $s -gt $((58 - {IDLE_SECONDS})) ] || [ $s -lt {AFTER_TICK} ]; \
         do sleep 0.5; done; \
         minute=$(date +%H%M); \
         : > /tmp/idle-threads; for n in beize-compositor beize-shell beize-terminal; do \
         p=$(pgrep -f \"^/usr/lib/beize/$n\" | head -n 1); echo \"$n $p $(c $p)\"; \
         for t in /proc/$p/task/*; do echo \"$n ${{t##*/}} $(tr ' ' _ < $t/comm) \
         $(awk '/ctxt_switches/ {{s += $2}} END {{print s}}' $t/status)\" >> /tmp/idle-threads; \
         done; done > /tmp/idle; since=$(date +%s.%N); \
         run0 systemd-run -q -p DynamicUser=yes true; \
         sleep {IDLE_SECONDS}; \
         [ \"$(date +%H%M)\" = \"$minute\" ] \
         && ! journalctl -b --since=@$since -q -o cat | grep -q 'Jumped offset' && break; \
         echo BEIZE-NEW-MINUTE; \
         sleep {settle}; done; \
         while read -r n p s; do echo \"BEIZE-IDLE $n $(( $(c $p) - s ))\"; done < /tmp/idle; \
         while read -r n t name s; do \
         now=$(awk '/ctxt_switches/ {{s += $2}} END {{print s}}' /proc/*/task/$t/status 2>/dev/null); \
         [ \"${{now:-$s}}\" != \"$s\" ] && echo \"BEIZE-WOKE $n $name/$t $(( ${{now:-$s}} - s ))\"; \
         done < /tmp/idle-threads; \
         journalctl -b --since=@$since -o short-monotonic --no-pager -q | sed 's/^/BEIZE-JOURNAL /'; \
         true",
        // Before measuring again, what the new minute drew settles, as
        // before the first.
        settle = RUN_ENDED.as_secs()
    ))?;
    let woke: Vec<(&str, u64)> = lines
        .iter()
        .filter_map(|l| {
            let (name, switches) = l.strip_prefix("BEIZE-IDLE ")?.split_once(' ')?;
            Some((name, switches.parse().ok()?))
        })
        .collect();
    ensure!(
        woke.len() == 3 && woke.iter().all(|&(_, n)| n == 0),
        "the still desktop woke: task switches in {IDLE_SECONDS} s {woke:?}; {lines:?}"
    );
    say!(
        "  ok: still, the compositor, the shell and the terminal did not wake once in \
         {IDLE_SECONDS} s"
    );

    // Signing out, from the Beize menu with its confirmation: logind ends the
    // session, beize-session stops the desktop and, last of all, takes the
    // environment back: waited for that, the rest is done too.
    let (beize, _) = bar_buttons(shell)?;
    click(qmp, screen, beize, 1)?;
    thread::sleep(Duration::from_millis(500));
    click(qmp, screen, beize_menu_row(beize, 3, true), 1)?;
    wait_for_shell_count(shell, "asking: LogOut", 1)?;
    thread::sleep(Duration::from_millis(500));
    keys(qmp, &["ret"])?;
    let (_, lines) = shell.capture(
        "for i in $(seq 300); do systemctl --user show-environment | grep -q '^XDG_SESSION_ID=' \
         || break; sleep 0.2; done; \
         systemctl --user is-active beize-session.target beize-compositor.service \
         beize-shell.service graphical-session.target; systemctl --user show-environment",
    )?;
    ensure!(
        lines.iter().filter(|l| *l == "inactive").count() == 4,
        "signing out did not stop the desktop: {lines:?}"
    );
    ensure!(
        !lines
            .iter()
            .any(|l| l.starts_with("WAYLAND_DISPLAY=") || l.starts_with("XDG_SESSION_ID=")),
        "the session's environment stayed with the service manager: {lines:?}"
    );
    let left = terminals(shell)?;
    ensure!(
        left.is_empty(),
        "terminals still run after signing out: {left:?}"
    );
    say!(
        "  ok: signing out stopped the desktop, its shell and terminals and took its environment \
         back"
    );
    Ok(())
}

/// The screen goes off without input (os/beize-compositor, `idle`), in a
/// session of its own with `SCREEN_OFF_AFTER` – the desktop's other
/// checks would see it go off. The owner signs in again on tty1. Without
/// input the screen fades to black, darker a while in, and a key takes
/// the fade back: the desktop as it was. The test client's window with an
/// idle inhibitor keeps the screen from fading for twice the time; once
/// the client ends, the screen fades, goes off – the kernel says the
/// connector is off (DPMS) – and a key turns it on with the desktop.
/// Nothing warns but Smithay's known warnings.
pub(super) fn screen_goes_off(shell: &Shell, machine: &Machine, qmp: &Path) -> Result<()> {
    let after = SCREEN_OFF_AFTER.as_secs();
    let fade = SCREEN_FADE.as_secs();
    let (_, lines) = shell.capture(&format!(
        "systemctl --user set-environment BEIZE_SCREEN_OFF_AFTER={after} \
         BEIZE_SCREEN_FADE={fade} && \
         for i in $(seq 300); do pgrep -t tty1 -x agetty > /dev/null && break; sleep 0.2; done; \
         echo BEIZE-SINCE-$(date +%s.%N)"
    ))?;
    let since = lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SINCE-"))
        .with_context(|| format!("no time to look from: {lines:?}"))?
        .to_owned();
    // The new getty's prompt, then as at the first sign-in.
    thread::sleep(Duration::from_secs(1));
    for (typed, pause) in [(SMOKE_USER, 1), (SMOKE_PASSWORD, 2), ("beize-session", 0)] {
        keys(qmp, &german_presses(typed)?)?;
        thread::sleep(Duration::from_secs(pause));
    }
    let said =
        |line: &str, count: usize, within: Duration| said_since(shell, &since, line, count, within);
    said(
        &format!("the screen goes off after {after} s without input"),
        1,
        SCREEN_TIMEOUT,
    )?;
    said(
        "desktop shown with the shell, [0-9]* ms after the screen",
        1,
        SCREEN_TIMEOUT,
    )?;
    ensure!(
        dpms(shell)? == "On",
        "the screen is not on after signing in"
    );
    // Once the desktop has faded in (its opening): the screen to compare with.
    wait_for_panels(machine)?;
    let desktop = screen(machine, "desktop-before-fade")?;

    // Without input it fades; darker a while in; a key takes it back.
    const FADES: &str = "no input for [0-9]* s: the screen fades to black";
    said(FADES, 1, DEADLINE)?;
    wait_until_screen(machine, "fading", |shown| {
        let darker = centre_mean(&desktop) - centre_mean(shown);
        ensure!(
            darker >= FADE_DARKER,
            "the screen is not darker than the desktop by {FADE_DARKER} but by {darker:.1}"
        );
        Ok(())
    })?;
    let faded = screen(machine, "faded")?;
    let darker = centre_mean(&desktop) - centre_mean(&faded);
    keys(qmp, &["shift"])?;
    said("input: the fade to black is taken back", 1, DEADLINE)?;
    wait_for_panels(machine)?;
    // The desktop as the fade left it: what the screen shows again later.
    let desktop = screen(machine, "fade-taken-back")?;
    say!(
        "  ok: without input for {after} s the screen faded to black, {darker:.0} darker, \
         and a key took the fade back"
    );

    // A window with an idle inhibitor keeps it from fading, for twice the
    // time; ended, the screen fades and goes off.
    let (_, lines) = shell.capture(
        "systemd-run --user --unit=beize-testclient-inhibit /run/t/beize-testclient --inhibit \
         --seconds 60 && for i in $(seq 300); do journalctl --user -u beize-testclient-inhibit \
         -o cat | grep -q '^configure ' && break; sleep 0.2; done; \
         journalctl --user -u beize-testclient-inhibit -o cat",
    )?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure ")),
        "the test client with an idle inhibitor got no window: {lines:?}"
    );
    thread::sleep(2 * SCREEN_OFF_AFTER + SCREEN_OFF_SLACK / 5);
    let fades = said_since(shell, &since, FADES, 0, Duration::ZERO)?;
    ensure!(
        fades == 1,
        "the screen faded with a window that inhibits idling on it ({fades} fades)"
    );
    shell.capture("systemctl --user stop beize-testclient-inhibit")?;
    said(FADES, 2, DEADLINE)?;
    said("the screen is off", 1, DEADLINE)?;
    let state = dpms(shell)?;
    ensure!(
        state == "Off",
        "the compositor said the screen is off, the kernel says {state}"
    );
    say!(
        "  ok: a window with an idle inhibitor kept the screen on for {} s; once it ended, the \
         screen faded and went off (DPMS Off)",
        2 * after
    );

    // Off, the screen is gone for clients: one drawing as Mesa's FIFO does
    // may draw as hidden windows do, not in the screen's pace.
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --wait --unit=beize-testclient-off /run/t/beize-testclient \
         --fifo 1000 --seconds {}; journalctl --user -u beize-testclient-off -o cat \
         | grep '^drawing: '",
        OFF_DRAWING.as_secs()
    ))?;
    let drawn: usize = lines
        .iter()
        .find_map(|l| l.strip_prefix("drawing: ")?.split_once(' ')?.0.parse().ok())
        .with_context(|| format!("the client said nothing of its drawing: {lines:?}"))?;
    ensure!(
        (1..=OFF_FRAMES_MOST).contains(&drawn),
        "with the screen off a client drew {drawn} frames in {} s, not between 1 and \
         {OFF_FRAMES_MOST}",
        OFF_DRAWING.as_secs()
    );
    ensure!(
        dpms(shell)? == "Off",
        "a client's window turned the screen on"
    );
    say!(
        "  ok: off, a client drew {drawn} frames in {} s, as hidden windows do",
        OFF_DRAWING.as_secs()
    );

    // A key turns it on, with the desktop as it was.
    keys(qmp, &["shift"])?;
    said("input: the screen is on again", 1, DEADLINE)?;
    wait_for_panels(machine)?;
    let state = dpms(shell)?;
    ensure!(
        state == "On",
        "after a key the kernel says the screen is {state}"
    );
    let back = screen(machine, "screen-on")?;
    ensure!(
        (centre_mean(&back) - centre_mean(&desktop)).abs() < 1.0,
        "the desktop did not come back as it was; the screens are in the logs as \
         fade-taken-back and screen-on"
    );
    let (_, lines) = shell.capture(&format!(
        "journalctl --user -u beize-compositor.service --since=@{since} -p warning -o cat"
    ))?;
    let unexpected = unexpected_warnings(&lines);
    ensure!(
        unexpected.is_empty(),
        "the compositor warned: {unexpected:?}"
    );
    say!("  ok: a key turned the screen on with the desktop as it was");

    // Signed out as in `still_and_signed_out`: the environment goes last.
    let (_, lines) = shell.capture(
        "s=$(systemctl --user show-environment | sed -n 's/^XDG_SESSION_ID=//p'); \
         loginctl terminate-session \"$s\"; \
         for i in $(seq 300); do systemctl --user show-environment | grep -q '^XDG_SESSION_ID=' \
         || break; sleep 0.2; done; systemctl --user unset-environment BEIZE_SCREEN_OFF_AFTER BEIZE_SCREEN_FADE; \
         systemctl --user is-active beize-compositor.service",
    )?;
    ensure!(
        lines.iter().any(|l| l == "inactive"),
        "signing out did not stop the desktop: {lines:?}"
    );
    Ok(())
}

/// Waits until the compositor has said `line` (a pattern, as grep's) at
/// least `count` times since `since` (seconds since the epoch), at most
/// `within`; how often it has. `count` 0: only how often.
pub(super) fn said_since(
    shell: &Shell,
    since: &str,
    line: &str,
    count: usize,
    within: Duration,
) -> Result<usize> {
    let deadline = Instant::now() + within;
    loop {
        let (_, lines) = shell.capture(&format!(
            "echo BEIZE-COUNT-$(journalctl --user -u beize-compositor.service --since=@{since} \
             -o cat | grep -cx '{line}')"
        ))?;
        let said: usize = lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT-"))
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("could not count \"{line}\": {lines:?}"))?;
        if said >= count {
            return Ok(said);
        }
        if Instant::now() >= deadline {
            let (_, last) = shell.capture(&format!(
                "journalctl --user -u beize-compositor.service --since=@{since} \
                 -o short-monotonic | grep -v ': pointer at ' | tail -n 30"
            ))?;
            bail!(
                "the compositor said \"{line}\" {said} times, not {count}, within {} s; last \
                 said:\n{}",
                within.as_secs(),
                last.join("\n")
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The VM screen's connector state as the kernel has it: `On` or `Off`.
pub(super) fn dpms(shell: &Shell) -> Result<String> {
    let (_, lines) = shell.capture(&format!(
        "echo BEIZE-DPMS-$(cat /sys/class/drm/card*-{VM_SCREEN}/dpms)"
    ))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-DPMS-"))
        .map(str::to_owned)
        .with_context(|| format!("no DPMS state: {lines:?}"))
}

/// The screen dumps of a passed run go (3 MB each, some 30 a run); a failed
/// run keeps them beside its logs, with the PNGs it wrote for the owner.
pub(super) fn remove_screens(logs: &Path) -> Result<()> {
    for entry in fs::read_dir(logs)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "ppm") {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Takes a screenshot with Super+Shift+3 of the still screen and compares
/// it with QEMU's screendump before and after (again if the clock's minute
/// changed in between); returns its size.
pub(super) fn screenshot(shell: &Shell, machine: &Machine) -> Result<(usize, usize)> {
    let qmp = machine.qmp();
    park(&qmp, screen_size(machine)?)?;
    thread::sleep(Duration::from_secs(1));
    let dump = |name: &str| -> Result<Image> {
        let file = machine.logs.join(format!("{name}.ppm"));
        screendump(&qmp, &file)?;
        Image::read_ppm(&fs::read(&file)?)
    };
    let wanted = "screenshot [0-9]*x[0-9]*: .*[.]png";
    let mut last: Option<String> = None;
    let mut taken = None;
    for _ in 0..2 {
        let before = dump("screenshot-before")?;
        keys(&qmp, &["meta_l+shift+3"])?;
        // The compositor's newest line, not the one of the last try.
        let deadline = Instant::now() + SCREEN_TIMEOUT;
        let said = loop {
            let said = wait_for_compositor_line(shell, wanted)?;
            if last.as_ref() != Some(&said) {
                break said;
            }
            ensure!(Instant::now() < deadline, "no second screenshot");
            thread::sleep(Duration::from_millis(200));
        };
        let after = dump("screenshot-after")?;
        if before.pixels == after.pixels {
            taken = Some((said, after));
            break;
        }
        last = Some(said);
    }
    let (said, after) = taken.context("the screen changed while each screenshot was taken")?;
    let file = said
        .split_once(": ")
        .map(|(_, f)| f)
        .context("no file in the compositor's line")?;
    let (status, lines) = shell.capture(&format!("base64 -w 76 '{file}'"))?;
    ensure!(status == 0, "could not read {file}: {lines:?}");
    // The kernel's console may cut into a line ("[   35.67] rcu: …"): base64
    // has no `[`, the rest of the line is the kernel's; the PNG's checksums
    // catch anything else.
    let text: String = lines
        .iter()
        .map(|l| l.split('[').next().unwrap_or_default())
        .collect();
    let shot = Image::read_png(&base64(&text)?).context("reading the screenshot")?;
    ensure!(
        (shot.width, shot.height) == (after.width, after.height),
        "the screenshot is {}x{}, the screen {}x{}",
        shot.width,
        shot.height,
        after.width,
        after.height
    );
    shot.write_png(&machine.logs.join("screenshot.png"))?;
    compare(
        &after,
        |x, y| shot.pixel(x, y),
        "the screenshot",
        &machine.logs.join("screenshot-difference.ppm"),
    )?;
    Ok((shot.width, shot.height))
}

/// Decodes base64 (RFC 4648), as `base64` writes it.
pub(super) fn base64(text: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Result<u32> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("not base64: {:?}", char::from(c)),
        }
        .into())
    };
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        let mut n = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Ok(out)
}
