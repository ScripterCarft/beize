//! The frame schedule and the protocols that pace clients, the load test,
//! and what a client may send (menus that grab, protocol errors).

use super::*;

/// The appearance the compositor kept (~/.config/beize/appearance).
pub(super) fn keep_says(shell: &Shell) -> Result<String> {
    let (_, lines) = shell.capture("cat ~/.config/beize/appearance | sed 's/^/BEIZE-SAID /'")?;
    Ok(lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SAID "))
        .unwrap_or_default()
        .to_owned())
}

/// The compositor sums up a client's run of frames, alone and in full.
pub(super) fn frames(shell: &Shell) -> Result<()> {
    // Frames in a row: a second client draws 120, each on "frame done";
    // once they stop, the compositor sums up how they met their vblanks –
    // the next summary, alone: as many frames as it drew, not more. The
    // drag's frames end their run first; a short run is summed up only for
    // debugging, so there is nothing to wait for but its end.
    thread::sleep(RUN_ENDED);
    let summed = pacing_summaries(shell)?;
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit=beize-testclient-frames /run/t/beize-testclient --frames 120 \
         --seconds 20 && for i in $(seq 300); do [ \"$({PACING} | wc -l)\" -gt {summed} ] \
         && break; sleep 0.2; done; sleep 1; {PACING} | tail -n +{}",
        summed + 1
    ))?;
    let Some(pacing) = lines.iter().find(|l| l.starts_with("pacing: ")) else {
        let said = last_said(shell, &["beize-testclient-frames", "beize-compositor"])?;
        bail!(
            "no pacing summary (exit status {status}): {lines:?}; the client and the compositor \
             last said: {said:?}"
        );
    };
    // A stand of the VM's vCPUs (#41) ends a run and starts another: all of
    // them since, summed.
    let frames: u32 = lines
        .iter()
        .filter_map(|l| l.strip_prefix("pacing: "))
        .filter_map(|s| s.split_once(' ')?.0.parse::<u32>().ok())
        .sum();
    // A frame or two more for the menu bar (its minute) is no mix.
    ensure!(
        (100..=125).contains(&frames),
        "the compositor counted {frames} frames of the client's 120: {pacing}"
    );
    say!("  ok: the compositor measured the client's frames: {pacing}");
    presentation(shell)?;
    // Its window would go by itself later, in the middle of another check.
    shell.capture("systemctl --user stop beize-testclient-frames")?;
    fifo(shell)?;
    timed(shell)?;
    syncobj(shell)?;

    Ok(())
}

/// The client of `frames` heard for each frame when it showed
/// (wp_presentation): at its vblank, on CLOCK_MONOTONIC (clock 1), timed
/// by the hardware – flags only a vblank's time gets, not the "changed
/// nothing" path's –, times and sequence rising, each before it arrived.
/// And none was discarded: a client drawing on "frame done" hears it only
/// once its last frame is drawn, so it never replaces one not yet shown
/// (until 0.5.104 one in 120 was, when its commit came after drawing
/// began; three runs of three). And it hears it then, each before its frame
/// showed, as Weston answers at repaint: answered at the vblank (until
/// 0.5.107), a client at 240 Hz had ~1 ms left to draw its next.
pub(super) fn presentation(shell: &Shell) -> Result<()> {
    let (_, lines) = shell.capture(
        "for i in $(seq 25); do journalctl --user -u beize-testclient-frames -o cat \
         | grep -q '^presentation: ' && break; sleep 0.2; done; \
         journalctl --user -u beize-testclient-frames -o cat \
         | grep -e '^presentation: ' -e '^frame done: '",
    )?;
    let line = lines
        .iter()
        .find(|l| l.starts_with("presentation: "))
        .context("the client said nothing of when its frames showed")?;
    for part in [
        "presentation: 120 of 120 shown, 0 discarded",
        "clock 1,",
        "on all: vsync hw_clock hw_completion,",
        "rising yes,",
        "before arrival yes",
    ] {
        ensure!(line.contains(part), "not \"{part}\": {line}");
    }
    ensure!(
        !line.contains("refresh 0 ns"),
        "no refresh with the frames: {line}"
    );
    let done = lines
        .iter()
        .find(|l| l.starts_with("frame done: "))
        .context("the client said nothing of when it heard \"frame done\"")?;
    ensure!(
        done == "frame done: 120 of 120 before they showed",
        "\"frame done\" not as soon as each frame was drawn, before it showed: {done}"
    );
    say!("  ok: the client heard when each frame showed: {line}; {done}");
    Ok(())
}

/// A client drawing as Mesa's Vulkan FIFO mode does (wp_fifo): without
/// waiting for "frame done", each commit waits in the compositor until the
/// one before it showed. What wp_fifo promises, without exception: all 120
/// show, none discarded, each at a later vblank than the one before.
///
/// How many show at the very next vblank is how fast the frames are drawn,
/// not the protocol: in the VM lavapipe draws on the guest's CPUs and the
/// compositor waits for it, so under load a frame takes longer than a
/// refresh and shows a vblank later (27.10.: 4 of 4 red runs under host
/// load had 12–35 such frames, the client late for none, drawing up to
/// 123 ms; the barrier lifted at the vblank, the frame drawn at once).
/// Until then a tolerance of two such frames was checked, which no run under
/// load could meet. What is checked of it is what an error of the
/// compositor's would do to all frames – the barrier lifted a vblank late,
/// the redraw after it not scheduled: then none shows at the next vblank.
/// How many do is measured on hardware (`measure/hw-test.sh`).
pub(super) fn fifo(shell: &Shell) -> Result<()> {
    let said = client_summary(shell, "beize-testclient-fifo", "--fifo 120", "fifo: ")?;
    let presented = said.iter().find(|l| l.starts_with("presentation: "));
    let apart = said.iter().find(|l| l.starts_with("fifo: "));
    let (Some(presented), Some(apart)) = (presented, apart) else {
        bail!("no summary of the fifo client: {said:?}");
    };
    ensure!(
        presented.contains("presentation: 120 of 120 shown, 0 discarded"),
        "frames of the fifo client were lost: {presented}"
    );
    ensure!(
        presented.contains("rising yes"),
        "frames of the fifo client showed at the same vblank or out of turn: {presented}"
    );
    let count = apart
        .strip_prefix("fifo: ")
        .and_then(|s| s.split_once(' '))
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("no count in: {apart}"))?;
    ensure!(
        count > 0,
        "no frame of the fifo client showed at the vblank after the last: {apart}"
    );
    let starved = said
        .iter()
        .find(|l| l.ends_with("committed after the frame before showed"))
        .map_or("", String::as_str);
    say!("  ok: a fifo client's frames each showed, in turn: {apart}; {starved}");
    Ok(())
}

/// A client giving each frame a time (wp_commit_timing), 2.5 refreshes after
/// the last one showed: never before its time, though it was ready long
/// before. At the first vblank at or after it, as with `fifo`: what the
/// compositor would get wrong for all (the time let go a vblank late), not
/// how fast the VM draws.
pub(super) fn timed(shell: &Shell) -> Result<()> {
    let said = client_summary(shell, "beize-testclient-timed", "--timed 30", "timed: ")?;
    let timed = said
        .iter()
        .find(|l| l.starts_with("timed: "))
        .with_context(|| format!("no summary of the timed client: {said:?}"))?;
    ensure!(
        timed.contains(" 0 before it,"),
        "frames of the timed client showed before their time: {timed}"
    );
    let on_time = timed
        .strip_prefix("timed: ")
        .and_then(|s| s.split_once(" of 29 "))
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("no count of 29 in: {timed}"))?;
    ensure!(
        on_time > 0,
        "no frame of the timed client showed at the first vblank at its time: {timed}"
    );
    say!("  ok: a client's frames showed at their time: {timed}");
    Ok(())
}

/// Explicit sync (linux-drm-syncobj): the test client's dmabufs (dumb
/// buffers of virtio-gpu; the VM's lavapipe takes them through udmabuf)
/// with timeline points it signals itself. Its checks are about order,
/// not time: a commit shows only after its acquire point signalled; a
/// buffer's release point signals (the kernel's time) no earlier than the
/// vblank of the frame that showed it, also when the next one replaced it
/// while that frame was on its way – which must have happened, or the
/// check caught nothing; a buffer without points is a protocol error.
pub(super) fn syncobj(shell: &Shell) -> Result<()> {
    let said = client_summary(
        shell,
        "beize-testclient-syncobj",
        "--syncobj",
        "syncobj protocol: ",
    )?;
    let line = |name: &str| {
        said.iter()
            .find(|l| l.starts_with(name))
            .with_context(|| format!("no {name} line of the syncobj client: {said:?}"))
    };
    let acquire = line("syncobj acquire: ")?;
    ensure!(
        acquire == "syncobj acquire: held until signalled, shown after",
        "a commit showed before its acquire point signalled: {acquire}"
    );
    let release = line("syncobj release: ")?;
    let counts: Vec<u32> = release
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect();
    let [after, of, replaced] = counts[..] else {
        bail!("no counts in: {release}");
    };
    ensure!(
        release.contains(" after their vblank began,") && after == of && of > 0,
        "a release point signalled while the compositor could still read its buffer: {release}"
    );
    ensure!(
        replaced > 0,
        "no buffer was replaced while its frame was on its way, the release check caught nothing: {release}"
    );
    let protocol = line("syncobj protocol: ")?;
    ensure!(
        protocol == "syncobj protocol: no_acquire_point raised",
        "a buffer without sync points was taken: {protocol}"
    );
    say!("  ok: explicit sync: {acquire}; {release}; {protocol}");
    Ok(())
}

/// Runs the test client as `unit` with `args` until it printed a line
/// starting with `last`, then stops it; its summary lines (`name: …`).
pub(super) fn client_summary(
    shell: &Shell,
    unit: &str,
    args: &str,
    last: &str,
) -> Result<Vec<String>> {
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit={unit} /run/t/beize-testclient {args} --seconds 20 \
         && for i in $(seq 300); do journalctl --user -u {unit} -o cat | grep -q '^{last}' \
         && break; sleep 0.2; done; journalctl --user -u {unit} -o cat \
         | grep -E '^(presentation|fifo|timed|(activation|syncobj) [a-z ]+): '; \
         systemctl --user stop {unit}"
    ))?;
    Ok(lines)
}

/// The load test's scene (ADR 0028 §9): its top window, dragged by its bar
/// over the dock, lies half behind it, and the dock's live glass changes
/// with what moves there – base glass, or live glass blurred only once,
/// would stay. Every window draws throughout, the compositor sums the
/// frames up (`pacing:`, said for the record: lavapipe's times say nothing
/// of a GPU's).
pub(super) fn load(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    const UNIT: &str = "beize-load-test";
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-load-test --windows {LOAD_WINDOWS} \
         --seconds {LOAD_SECONDS}"
    ))?;
    ensure!(
        status == 0,
        "could not start the load test (exit status {status}): {lines:?}"
    );
    let ((x, y), _) = placed(shell, UNIT, LOAD_WINDOWS)?;
    let grip = (
        i32::try_from(x + BAR_GRIP_X)?,
        i32::try_from(y + TITLE_BAR / 2)?,
    );
    let to = (
        i32::try_from(screen.0 / 2)?,
        i32::try_from(screen.1 - DOCK_ZONE - LOAD_ABOVE_DOCK)?,
    );
    drag(qmp, screen, grip, to, true)?;
    park(qmp, screen)?;
    // Settled behind the dock, then compared with itself a moment later.
    thread::sleep(Duration::from_secs(1));
    let glass_at = (
        screen.0 / 2,
        screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE + 1,
    );
    let file = machine.logs.join("load-glass-screen.ppm");
    screendump(qmp, &file)?;
    let before = Image::read_ppm(&fs::read(&file)?)?.pixel(glass_at.0, glass_at.1);
    wait_for_live_glass(machine, glass_at, before)?;

    let (_, lines) = shell.capture(&format!(
        "for i in $(seq {}); do systemctl --user -q is-active {UNIT} || break; sleep 0.2; \
         done; systemctl --user is-active {UNIT}; journalctl --user -u {UNIT} -o cat",
        (LOAD_SECONDS + 15) * 5
    ))?;
    let drawn: Vec<usize> = lines
        .iter()
        .filter_map(|l| {
            l.strip_prefix("window ")?
                .split_once(": ")?
                .1
                .split_once(' ')
        })
        .filter_map(|(frames, _)| frames.parse().ok())
        .collect();
    ensure!(
        lines.iter().any(|l| l.starts_with("load: "))
            && drawn.len() == LOAD_WINDOWS
            && drawn.iter().all(|&f| f >= LOAD_LEAST_FRAMES),
        "the load test's {LOAD_WINDOWS} windows did not each draw {LOAD_LEAST_FRAMES} frames \
         in {LOAD_SECONDS} s: {lines:?}"
    );
    // Drawing all the time, each window waited for its last frame to show
    // before the next (Beize UI's clock): never two on their way. Drawing
    // ahead, they queue behind the compositor's frames – what made resizing
    // trail and keys come late on hardware (0.5.105, Vulkan FIFO with
    // wp_fifo).
    let ahead = lines
        .iter()
        .find_map(|l| {
            l.strip_prefix("load: ")?
                .split_once("at most ")?
                .1
                .split_once(' ')
        })
        .and_then(|(n, _)| n.parse::<u32>().ok())
        .with_context(|| format!("the load test said nothing of frames on their way: {lines:?}"))?;
    ensure!(
        ahead <= 1,
        "a window of the load test had {ahead} frames on their way at once, more than one: it \
         drew ahead of the compositor"
    );
    // The compositor sums a run up a quarter second after its last frame.
    let (_, lines) = shell.capture(
        "sleep 1; journalctl --user -b -u beize-compositor.service -o cat \
         | grep '^pacing: ' | tail -n 1",
    )?;
    let pacing = lines
        .iter()
        .find(|l| l.starts_with("pacing: "))
        .context("the compositor summed up no frames")?;
    // The load's run, not an earlier one: as long as the scene, near enough.
    let seconds = pacing
        .split_once(" frames in ")
        .and_then(|(_, rest)| rest.split_once(" s")?.0.parse::<f64>().ok())
        .with_context(|| format!("no length in {pacing:?}"))?;
    // Only said: a stand of the VM splits the run, and its pace counts on
    // hardware (`measure/hw-test.sh`).
    let _ = seconds;
    let least = drawn.iter().min().copied().unwrap_or(0);
    say!(
        "  ok: the load test's {LOAD_WINDOWS} windows each drew {least} frames or more in \
         {LOAD_SECONDS} s, at most {ahead} on its way, the dock's live glass followed the one \
         behind it; {pacing}"
    );
    Ok(())
}

/// The errors Beize's own protocols promise (`--protocol-errors`): a second
/// menu or glass, a menu without a window, glass for a toplevel.
pub(super) fn protocol_errors(shell: &Shell) -> Result<()> {
    let (_, lines) = shell.capture(
        "systemd-run --user --wait --unit=protocol-errors /run/t/beize-testclient \
         --protocol-errors; journalctl --user -u protocol-errors -o cat | grep '^protocol '",
    )?;
    for case in [
        "second menu: error 0",
        "menu without window: error 1",
        "glass for a toplevel: error 1",
        "second glass: error 0",
    ] {
        ensure!(
            lines.iter().any(|l| *l == format!("protocol {case}")),
            "not \"protocol {case}\": {lines:?}"
        );
    }
    say!(
        "  ok: a second menu or glass, a menu without a window and glass for a toplevel were \
         protocol errors"
    );
    Ok(())
}

/// Menus that grab, and what an app may send (the test client's `--grabs`): no
/// menu with the keys unless it answers input; it ends with its window; and a
/// flood of titles leaves the shell standing.
pub(super) fn grabs(shell: &Shell, qmp: &Path) -> Result<()> {
    const UNIT: &str = "beize-testclient-grabs";
    let restarts = "echo BEIZE-RESTARTS-$(systemctl --user show -P NRestarts beize-shell.service)";
    let count = |lines: &[String]| {
        lines
            .iter()
            .find(|l| l.starts_with("BEIZE-RESTARTS-"))
            .cloned()
    };
    let (_, before) = shell.capture(&format!(
        "{restarts}; systemd-run --user --unit={UNIT} /run/t/beize-testclient --grabs; \
         for i in $(seq 300); do journalctl --user -u {UNIT} -o cat \
         | grep -q '^grabs without input: ' && break; sleep 0.2; done"
    ))?;
    keys(qmp, &["a"])?;
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 150); do journalctl --user -u {UNIT} -o cat \
         | grep -q '^grabs flood: ' && break; sleep 0.2; done; \
         journalctl --user -u {UNIT} -o cat | grep '^grabs '; {restarts}; \
         systemctl --user is-active beize-shell.service; systemctl --user stop {UNIT}"
    ))?;
    for wanted in [
        "grabs without input: refused",
        "grabs with a key: open",
        "grabs minimized: closed",
        "grabs unmapped: keyboard gone",
        "grabs flood: sent",
        "active",
    ] {
        ensure!(
            lines.iter().any(|l| l == wanted),
            "not \"{wanted}\": {lines:?}"
        );
    }
    ensure!(
        count(&before).is_some() && count(&before) == count(&lines),
        "the flood of titles restarted the shell: {before:?} before, {lines:?}"
    );
    say!(
        "  ok: no menu grabbed without input, one answering a key closed with its window, and a \
         flood of titles left the shell standing"
    );
    Ok(())
}

/// How many runs of frames the compositor has summed up in this boot.
pub(super) fn pacing_summaries(shell: &Shell) -> Result<usize> {
    let (_, lines) = shell.capture(&format!("echo BEIZE-SUMMED-$({PACING} | wc -l)"))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SUMMED-")?.parse().ok())
        .with_context(|| format!("no count of the compositor's summaries: {lines:?}"))
}
