//! Glass: the dock's live glass, the control center's dense glass (ADR 0033
//! §6).

use super::*;

/// The dock's live glass shows the client's window dragged behind it.
pub(super) fn dock_glass(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
    // The dock's glass is live (ADR 0033 §6): dragged behind it, the window
    // shows through, blurred. Then back.
    // Left of the middle, over the window's blue quarter, below the first
    // app's icon: nearer the middle its blue and white blur, through the
    // glass's tint, into about what the background was there (20 px off it
    // the two differed by 3 in red); clear of the dock's round corner.
    let glass_at = (
        screen.0 / 2 - 60,
        screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE + 1,
    );
    let file = machine.logs.join("dock-glass-screen.ppm");
    screendump(&machine.qmp(), &file)?;
    let before = Image::read_ppm(&fs::read(&file)?)?.pixel(glass_at.0, glass_at.1);
    let grip = point(
        moved,
        (
            i32::try_from(grown.0 / 2)?,
            i32::try_from(CLIENT_TITLE / 2)?,
        ),
    )?;
    let behind = (
        i32::try_from(screen.0 / 2)? - i32::try_from(grown.0 / 2)? - i32::try_from(moved.0)?,
        i32::try_from(screen.1)? - i32::try_from(grown.1)? - i32::try_from(moved.1)?,
    );
    drag(qmp, screen, grip, by(grip, behind), true)?;
    park(qmp, screen)?;
    wait_for_live_glass(machine, glass_at, before)?;
    // For the owner: the dock over the window's blue and white quarters.
    self::screen(machine, "dock-over-window")?
        .write_png(&machine.logs.join("dock-over-window.png"))?;
    // For comparing glass before and after a change to its rendering: the
    // window's hard edge beside the dock, within and past the blur's reach
    // (ADR 0033 §6). The dock of two apps ends 80 px from the middle.
    let mut there = by(grip, behind);
    for (name, past) in [("dock-edge-20", 20), ("dock-edge-36", 36)] {
        // Its left edge lay half its width left of the middle.
        let next = (
            by(grip, behind).0 + i32::try_from(80 + past + grown.0 / 2)?,
            there.1,
        );
        drag(qmp, screen, there, next, true)?;
        there = next;
        park(qmp, screen)?;
        thread::sleep(Duration::from_secs(1));
        self::screen(machine, name)?.write_png(&machine.logs.join(format!("{name}.png")))?;
    }
    drag(qmp, screen, there, grip, true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "back from the dock")?;
    // With the window gone again it shows what it showed before: it blurs
    // what lies behind it, never what the screen held there – itself and
    // the dock's icons, tinted again with each blur.
    wait_for_glass_back(machine, glass_at, before)?;
    say!(
        "  ok: the dock's glass showed the window dragged behind it, blurred, and what it showed \
         before once it was gone"
    );

    Ok(())
}

/// The control center's dense glass (ADR 0033 §6) over the terminal
/// maximized behind it and full of text – the hardest case of the light
/// appearance: the text behind it is gone (blurred and tinted), the glass
/// comes out at the tone of its recipe for what lies behind, and the
/// network tile's title is dark ink holding 4.5:1 on it; in the dark
/// appearance the same with light ink. Its screens are in the logs for the
/// owner (`control-center-light.png`, `control-center-dark.png`). The
/// terminal's text cursor sits below the last line, far from where it
/// compares.
pub(super) fn control_center_glass(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    centre: (i32, i32),
) -> Result<()> {
    let (_, lines) = shell.capture(PLACED)?;
    let ((x, y), (w, _)) = lines
        .iter()
        .rev()
        .find_map(|l| placement(l, "beize-terminal"))
        .ok_or_else(|| anyhow::anyhow!("no terminal's window was placed"))?;
    // Where the drag with the menu open left its title bar (`menus`).
    let grip = by(
        (i32::try_from(x + w / 2)?, i32::try_from(y + TITLE_BAR / 2)?),
        MENU_DRAG_BY,
    );
    let (left, top) = control_center_at(screen);
    // The network capsule, the first row.
    let capsule = (
        left..left + CONTROL_CENTER_WIDTH,
        top..top + CONTROL_CENTER_TILE,
    );
    // Glass alone: in the capsule's round right end, right of its text, as
    // high as two of the terminal's rows (every other one is short there).
    let right = left + CONTROL_CENTER_WIDTH;
    let glass = (right - 20..right - 12, top + 16..top + 60);
    // Its title and state, right of its round button.
    let title = (
        left + 70..right - 22,
        top + 16..top + CONTROL_CENTER_TILE - 16,
    );
    let dark = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3 < WINDOW_DARK;
    click(qmp, screen, grip, 2)?;
    park(qmp, screen)?;
    let behind_at = (screen.0 - BAR_MARGIN - 4, top + CONTROL_CENTER_TILE / 2);
    wait_until_screen(machine, "glass-maximized", |shown| {
        let p = shown.pixel(behind_at.0, behind_at.1);
        ensure!(
            dark(p),
            "the terminal is not maximized behind the control center: {p:?}"
        );
        Ok(())
    })?;
    let (unit, _) = terminals(shell)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no terminal runs"))?;
    // Lines of text across the screen, written to the shell's terminal.
    let (_, said) = shell.capture(&format!(
        "d=/sys/fs/cgroup$(systemctl --user show -P ControlGroup '{unit}'); t=; \
         for p in $(cat \"$d/cgroup.procs\"); do [ \"$(cat /proc/$p/comm)\" = bash ] && \
         t=/proc/$p/fd/1; done; l=$(printf 'Wm#8%.0s' $(seq 60)); \
         [ -n \"$t\" ] && for i in $(seq 40); do echo \"$l\"; done > \"$t\"; \
         echo BEIZE-TEXT-${{t:+written}}"
    ))?;
    ensure!(
        said.iter().any(|l| l == "BEIZE-TEXT-written"),
        "no text went to the terminal's shell: {said:?}"
    );
    thread::sleep(Duration::from_secs(1));
    let closed = self::screen(machine, "glass-closed")?;
    let sharp = detail(&closed, glass.0.clone(), glass.1.clone());
    ensure!(
        sharp >= TEXT_DETAIL,
        "the terminal shows no text behind the control center ({sharp:.1}): the check would \
         prove nothing"
    );
    let behind = mean_luminance(&closed, capsule.0.clone(), capsule.1.clone());
    let opened = shell_count(shell, "ControlCenter open")?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", opened + 1)?;
    wait_for_control_center_grown(shell, opened + 1)?;
    let measured = std::cell::Cell::new((0.0, 0.0, 0.0));
    let check = |shown: &Image, tint: f64, density: f64, light: bool| -> Result<()> {
        let tone = mean_luminance(shown, glass.0.clone(), glass.1.clone());
        let expected = tint * density + behind * (1.0 - density);
        let left_of_text = detail(shown, glass.0.clone(), glass.1.clone());
        ensure!(
            left_of_text < sharp * GLASS_KEEPS,
            "the text behind the control center shows through: detail {left_of_text:.1}, \
             {sharp:.1} without"
        );
        ensure!(
            (tone - expected).abs() <= GLASS_TOLERANCE,
            "the control center's glass is {tone:.3}, not {expected:.3} (dense over {behind:.3})"
        );
        // The title's strongest ink: its darkest pixel in the light, its
        // lightest in the dark.
        let ink = extreme_luminance(shown, title.0.clone(), title.1.clone(), light);
        let contrast = (tone.max(ink) + 0.05) / (tone.min(ink) + 0.05);
        ensure!(
            contrast >= 4.5 && (ink < tone) == light,
            "the network tile's title is {ink:.3} on glass of {tone:.3}: {contrast:.2}:1, not \
             {} ink holding 4.5:1",
            if light { "dark" } else { "light" }
        );
        measured.set((tone, expected, contrast));
        Ok(())
    };
    wait_until_screen(machine, "glass-light", |shown| {
        check(shown, GLASS_TINT_LIGHT, GLASS_DENSE_LIGHT, true)
    })?;
    self::screen(machine, "glass-open")?
        .write_png(&machine.logs.join("control-center-light.png"))?;
    let (light_tone, light_expected, light_contrast) = measured.get();
    let tile = appearance_tile(screen)?;
    let (darks, lights) = (
        shell_count(shell, "appearance Dark")?,
        shell_count(shell, "appearance Light")?,
    );
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Dark", darks + 1)?;
    wait_until_screen(machine, "glass-dark", |shown| {
        check(shown, GLASS_TINT_DARK, GLASS_DENSE_DARK, false)
    })?;
    self::screen(machine, "glass-dark-open")?
        .write_png(&machine.logs.join("control-center-dark.png"))?;
    let (dark_tone, dark_expected, dark_contrast) = measured.get();
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Light", lights + 1)?;
    let closed_before = shell_count(shell, "ControlCenter closed")?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", closed_before + 1)?;
    say!(
        "  ok: over a terminal full of text the control center's dense glass hid the text and \
         came out at {light_tone:.3} light ({light_expected:.3} by its recipe), {dark_tone:.3} \
         dark ({dark_expected:.3}); its title held {light_contrast:.1}:1 and \
         {dark_contrast:.1}:1"
    );
    Ok(())
}

/// The darkest (`darkest`) or lightest pixel's relative luminance in `xs` ×
/// `ys`.
pub(super) fn extreme_luminance(
    screen: &Image,
    xs: std::ops::Range<usize>,
    ys: std::ops::Range<usize>,
    darkest: bool,
) -> f64 {
    let mut found = if darkest { f64::MAX } else { 0.0 };
    for y in ys {
        for x in xs.clone() {
            let l = mean_luminance(screen, x..x + 1, y..y + 1);
            found = if darkest { found.min(l) } else { found.max(l) };
        }
    }
    found
}

/// How much of the text's detail dense glass keeps (`detail`): blurred and
/// four fifths tinted, nearly nothing.
pub(super) const GLASS_KEEPS: f64 = 0.1;
/// The glass's tone against its recipe (relative luminance): the blur's
/// mean against the surface's, the grain.
pub(super) const GLASS_TOLERANCE: f64 = 0.04;
/// Dense Beize glass's tint and its alpha, as `beize-draw`'s tokens (the
/// set in use): #F2F2F2 at 80 %, #2D2D2D at 94 %, the tint's relative
/// luminance.
pub(super) const GLASS_TINT_LIGHT: f64 = 0.8879;
pub(super) const GLASS_DENSE_LIGHT: f64 = 0.80;
pub(super) const GLASS_TINT_DARK: f64 = 0.0262;
pub(super) const GLASS_DENSE_DARK: f64 = 0.94;

/// The control center's measures in the VM, as the shell's (`tokens`): its
/// tiles below the bar and a gap, cells of 76 with the inset between them;
/// as wide as its columns used, two while network and light/dark are its
/// only tiles; its right edge where the menu bar's margin ends.
pub(super) const CONTROL_CENTER_GAP: usize = 12;
pub(super) const CONTROL_CENTER_TILE: usize = 76;
pub(super) const BAR_MARGIN: usize = 20;
pub(super) const CONTROL_CENTER_WIDTH: usize = 2 * CONTROL_CENTER_TILE + CONTROL_CENTER_GAP;

/// The control center's first tile's top left corner.
pub(super) fn control_center_at(screen: (usize, usize)) -> (usize, usize) {
    (
        screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH,
        MENU_BAR + CONTROL_CENTER_GAP,
    )
}

/// The light/dark tile's centre: the second row's first
/// (os/beize-shell/src/control_center.rs).
pub(super) fn appearance_tile(screen: (usize, usize)) -> Result<(i32, i32)> {
    let (left, top) = control_center_at(screen);
    Ok((
        i32::try_from(left + CONTROL_CENTER_TILE / 2)?,
        i32::try_from(top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP + CONTROL_CENTER_TILE / 2)?,
    ))
}

/// The mean relative luminance (linear light) of `xs` × `ys`.
pub(super) fn mean_luminance(
    screen: &Image,
    xs: std::ops::Range<usize>,
    ys: std::ops::Range<usize>,
) -> f64 {
    let decoded = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let [r, g, b] = screen.pixel(x, y);
            sum += 0.2126 * decoded(r) + 0.7152 * decoded(g) + 0.0722 * decoded(b);
            n += 1.0;
        }
    }
    sum / f64::max(n, 1.0)
}

/// How much the screen changes from pixel to pixel in `xs` × `ys`: the
/// mean step in brightness (the mean of the channels) to the right
/// neighbour – text has many, a blur few.
pub(super) fn detail(
    screen: &Image,
    xs: std::ops::Range<usize>,
    ys: std::ops::Range<usize>,
) -> f64 {
    let brightness = |p: [u8; 3]| p.iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0;
    let (mut sum, mut n) = (0.0, 0.0);
    for y in ys {
        for x in xs.start..xs.end.saturating_sub(1) {
            sum += (brightness(screen.pixel(x + 1, y)) - brightness(screen.pixel(x, y))).abs();
            n += 1.0;
        }
    }
    sum / f64::max(n, 1.0)
}

/// Text shows at least this much detail (`detail`): a terminal's glyphs on
/// its dark ground, many times more than its ground alone.
pub(super) const TEXT_DETAIL: f64 = 8.0;
/// Waits until the dock's glass at `at` shows something else than
/// `before`, by more than `GLASS_SHOWS` in a channel: a window behind it.
/// The screen then is in the logs as `live-glass-screen.png`.
pub(super) fn wait_for_live_glass(
    machine: &Machine,
    at: (usize, usize),
    before: [u8; 3],
) -> Result<()> {
    wait_until_screen(machine, "live-glass", |screen| {
        let shown = screen.pixel(at.0, at.1);
        ensure!(
            shown
                .iter()
                .zip(before)
                .any(|(&a, b)| a.abs_diff(b) > GLASS_SHOWS),
            "the dock's glass shows {shown:?} at {},{} with a window behind it, as {before:?} \
             without: it is not live",
            at.0,
            at.1
        );
        Ok(())
    })
}

/// Waits until the dock's glass at `at` shows `before` again, to the
/// rounding (`CHANNEL_TOLERANCE`): the window gone from behind it. The
/// screen then is in the logs as `glass-back-screen.png`.
pub(super) fn wait_for_glass_back(
    machine: &Machine,
    at: (usize, usize),
    before: [u8; 3],
) -> Result<()> {
    wait_until_screen(machine, "glass-back", |screen| {
        let shown = screen.pixel(at.0, at.1);
        ensure!(
            shown
                .iter()
                .zip(before)
                .all(|(&a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE),
            "the dock's glass shows {shown:?} at {},{} with the window gone, not {before:?} as \
             before it came: it blurred what the screen held there",
            at.0,
            at.1
        );
        Ok(())
    })
}
