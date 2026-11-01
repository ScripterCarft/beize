//! Glass: the dock's live and liquid glass, the control center's clear glass
//! and smoke.

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
    // The dock's glass is live (ADR 0028 §6): dragged behind it, the window
    // shows through, blurred – base glass would stay as it was. Then back.
    // Left of the middle, over the window's blue quarter: in the middle its
    // blue and white blur into about what the background was there; between
    // the dots of the dock's two apps.
    let glass_at = (
        screen.0 / 2 - 20,
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
    // (30 px, ADR 0028 §6). The dock of two apps ends 80 px from the middle.
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

/// The control center's liquid glass over the light background (ADR 0028
/// §6), where nothing else lies: the network capsule's tone is the one its
/// curve gives for what lies behind – about as light as that over midtones,
/// not grey smoke (0.5.118's glass came out at a third of it) – and white
/// ink keeps 3:1 on it (Apple's tiles ~3.4:1, measured); the round tile's
/// line of light at its top is never darker than either side of it (a grey
/// stroke) and brighter than at its side, which has none. Its screen is in
/// the logs for the owner (`control-center-liquid.png`).
pub(super) fn liquid_glass(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    centre: (i32, i32),
) -> Result<()> {
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let top = MENU_BAR + CONTROL_CENTER_GAP;
    let capsule = (
        left..left + CONTROL_CENTER_WIDTH,
        top..top + CONTROL_CENTER_TILE,
    );
    // Right of the capsule's text, more than its curved edge inside.
    let clear_of_ink = (
        left + CONTROL_CENTER_WIDTH - 28..left + CONTROL_CENTER_WIDTH - 18,
        top + 30..top + 45,
    );
    let closed = self::screen(machine, "liquid-closed")?;
    let behind = mean_luminance(&closed, capsule.0.clone(), capsule.1.clone());
    ensure!(
        behind > LIQUID_KNEE + 0.05,
        "behind the control center the screen is not light ({behind:.3}): the check would prove \
         nothing"
    );
    let expected = liquid_tone(behind);
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 3)?;
    let radius = CONTROL_CENTER_TILE as f64 / 2.0;
    let round = (
        left as f64 + radius,
        (top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP) as f64 + radius,
    );
    // Where the edge's outward normal points at `degrees` (0 right, 90
    // down): the brightest pixel across the edge – the line lies ~1.3 px
    // in, so every pixel from 3.5 px inside to half a pixel out –, and how
    // far it stands above the lighter of the glass 8 px inside and the
    // screen 4 px outside (the mean of the channels).
    let line_at = |image: &Image, degrees: f64| {
        let (dx, dy) = (degrees.to_radians().cos(), degrees.to_radians().sin());
        let at = |r: f64| {
            let p = image.pixel((round.0 + dx * r) as usize, (round.1 + dy * r) as usize);
            (p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3) as i32
        };
        let peak = (0..=16)
            .map(|i| at(radius + 0.5 - 0.25 * f64::from(i)))
            .max()
            .unwrap_or(0);
        (peak, peak - at(radius - 8.0).max(at(radius + 4.0)))
    };
    let seen = std::cell::Cell::new((0.0, 0, 0));
    wait_until_screen(machine, "liquid", |shown| {
        let tile = mean_luminance(shown, clear_of_ink.0.clone(), clear_of_ink.1.clone());
        let contrast = 1.05 / (tile + 0.05);
        ensure!(
            (tile - expected).abs() <= expected * LIQUID_TOLERANCE,
            "the capsule's tone is {tile:.3} over {behind:.3} behind it, not {expected:.3} \
             (its curve)"
        );
        ensure!(
            contrast >= 3.0,
            "white ink keeps only {contrast:.2}:1 on the capsule"
        );
        let ((lit, above), (across, across_above)) = (line_at(shown, -90.0), line_at(shown, 0.0));
        ensure!(
            above >= LINE_ABOVE,
            "the round tile's line of light at its top is darker than a side of it ({lit}, \
             {above} over the lighter): a stroke, not light"
        );
        ensure!(
            above >= across_above + LINE_LIT_BY,
            "the round tile's line of light is not brighter at its top than at its side: {lit} \
             at the top, {across} at the side"
        );
        let (edge, outside) = (along_top(shown, left, top), along_top(shown, left, top - 3));
        ensure!(
            edge >= outside - TOP_EDGE_BELOW,
            "the capsule's outermost pixel at its top is {edge}, {outside} outside it: the \
             glass's grey above the line of light"
        );
        seen.set((tile, lit, across));
        Ok(())
    })?;
    self::screen(machine, "liquid-open")?
        .write_png(&machine.logs.join("control-center-liquid.png"))?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 3)?;
    let (tile, lit, across) = seen.get();
    say!(
        "  ok: the control center's liquid glass: over {behind:.3} behind it the capsule came \
         out at {tile:.3} ({expected:.3} by its curve, white ink {:.2}:1), its line of light \
         {lit} at the top, {across} at the side",
        1.05 / (tile + 0.05)
    );
    Ok(())
}

/// Liquid glass's tone with white ink on it (`quad.vert`, `liquid_tone`):
/// what lies behind as it is up to KNEE, above eased towards CAP.
pub(super) fn liquid_tone(behind: f64) -> f64 {
    if behind <= LIQUID_KNEE {
        return behind;
    }
    LIQUID_KNEE + (LIQUID_CAP - LIQUID_KNEE) * (1.0 - (-(behind - LIQUID_KNEE) / 0.15).exp())
}

/// As `quad.vert`'s KNEE and CAP. The screen's tile differs from the curve
/// by its saturation, the milk and the frost's mean against the screen's:
/// within a quarter of it.
pub(super) const LIQUID_KNEE: f64 = 0.22;
pub(super) const LIQUID_CAP: f64 = 0.27;
pub(super) const LIQUID_TOLERANCE: f64 = 0.25;
/// The line of light, in 8-bit steps (the mean of the channels): at the
/// top never below the lighter of its two sides, and above them by more
/// than at the side, which has no line (Apple's: light at the top and the
/// bottom, a dark edge at the sides). Over the light background, where the
/// glass is pulled down for its ink, it lies on what is outside it: some
/// 2 steps above it at the top (Apple's over white: 3), while a line on the
/// glass would come out ~15 below it.
pub(super) const LINE_ABOVE: i32 = 0;
pub(super) const LINE_LIT_BY: i32 = 6;
/// How far the outermost pixel along a capsule's top may lie below the
/// screen outside it, in the same steps: the frost's mean against the
/// screen's (the owner saw the glass's grey there, 30.10.).
pub(super) const TOP_EDGE_BELOW: i32 = 2;

/// A row's mean of the channels along the top of the capsule at `left`.
pub(super) fn along_top(image: &Image, left: usize, y: usize) -> i32 {
    let mid = left + CONTROL_CENTER_WIDTH / 2;
    let sum: u32 = (mid - 10..mid + 10)
        .map(|x| image.pixel(x, y).iter().map(|&c| u32::from(c)).sum::<u32>())
        .sum();
    (sum / 60) as i32
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

/// The control center's clear glass and the smoke around it (ADR 0028 §5,
/// §6), over the terminal maximized behind it and full of text: beside and
/// between its tiles the text blurs; where the smoke ends – 112 to 130 px
/// from them, a blur below a hundredth of a pixel – the screen is as it was,
/// so no seam, no grain and never the background shows there; the bar's ink
/// stays sharp over it (the smoke lies behind the bar). Its screens are in
/// the logs for the owner (`control-center-smoke-*.png`). The terminal's
/// text cursor sits below the last line, far from where it compares.
pub(super) fn control_center_smoke(
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
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let top = MENU_BAR + CONTROL_CENTER_GAP;
    let behind = (screen.0 - BAR_MARGIN - 4, top + CONTROL_CENTER_TILE / 2);
    let dark = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3 < WINDOW_DARK;
    click(qmp, screen, grip, 2)?;
    park(qmp, screen)?;
    wait_until_screen(machine, "smoke-maximized", |shown| {
        let p = shown.pixel(behind.0, behind.1);
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
    let closed = self::screen(machine, "smoke-closed")?;
    // Beside the round tile, right of it; the bar's clock, left of the
    // control center's pill (its half, 27 px, and 8 px between); where the
    // smoke ends, left of the tiles.
    let beside = (
        left + CONTROL_CENTER_TILE + 10..screen.0 - BAR_MARGIN - 10,
        {
            let round = top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP;
            round + 5..round + CONTROL_CENTER_TILE - 5
        },
    );
    let clock_end = usize::try_from(centre.0)? - 35;
    let clock = (clock_end - 100..clock_end, 8..24);
    let fading = (left - 128..left - 112, MENU_BAR + TITLE_BAR + 8..330);
    for (what, (xs, ys)) in [("beside the tiles", &beside), ("where it ends", &fading)] {
        let there = detail(&closed, xs.clone(), ys.clone());
        ensure!(
            there >= TEXT_DETAIL,
            "the terminal shows no text {what} ({there:.1}): the check would prove nothing"
        );
    }
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 4)?;
    let sharp = detail(&closed, beside.0.clone(), beside.1.clone());
    wait_until_screen(machine, "smoke", |shown| {
        let blurred = detail(shown, beside.0.clone(), beside.1.clone());
        ensure!(
            blurred < sharp * SMOKE_KEEPS,
            "the text beside the tiles is not blurred: detail {blurred:.1}, {sharp:.1} without"
        );
        Ok(())
    })?;
    thread::sleep(Duration::from_millis(500));
    let open = self::screen(machine, "smoke-open")?;
    open.write_png(&machine.logs.join("control-center-smoke-light.png"))?;
    // Over the dark window the glass is lighter than what is outside: above
    // the top line it would stand as a grey row, more than half way up to it.
    let (edge, outside, lit) = (
        along_top(&open, left, top),
        along_top(&open, left, top - 3),
        along_top(&open, left, top + 1),
    );
    ensure!(
        (edge - outside) * 2 < lit - outside,
        "over the dark window the capsule's outermost pixel at its top is {edge}, between \
         {outside} outside it and {lit} on the line: the glass's grey above the line of light"
    );
    let (xs, ys) = &fading;
    let (mut most, mut differ) = (0, 0);
    for py in ys.clone() {
        for px in xs.clone() {
            let (a, b) = (closed.pixel(px, py), open.pixel(px, py));
            let d = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
            most = most.max(d);
            differ += usize::from(d > 0);
        }
    }
    let all = xs.len() * ys.len();
    ensure!(
        most <= 1 && differ * 50 <= all,
        "where the smoke ends the screen changed: by up to {most}, in {differ} of {all} pixels \
         (a seam, grain or the background)"
    );
    let (ink_before, ink) = (
        detail(&closed, clock.0.clone(), clock.1.clone()),
        detail(&open, clock.0.clone(), clock.1.clone()),
    );
    ensure!(
        ink > ink_before * 0.9,
        "the bar's clock is blurred over the smoke: detail {ink:.1}, {ink_before:.1} without"
    );
    // For the owner: the same in dark.
    let tile = (
        i32::try_from(left + CONTROL_CENTER_TILE / 2)?,
        i32::try_from(top + CONTROL_CENTER_TILE + CONTROL_CENTER_GAP + CONTROL_CENTER_TILE / 2)?,
    );
    let (darks, lights) = (
        shell_count(shell, "appearance Dark")?,
        shell_count(shell, "appearance Light")?,
    );
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Dark", darks + 1)?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "smoke-dark")?
        .write_png(&machine.logs.join("control-center-smoke-dark.png"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, "appearance Light", lights + 1)?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 4)?;
    say!(
        "  ok: over a terminal the control center's smoke blurred the text beside its tiles \
         (detail {sharp:.1} -> {:.1}), left it as it was where it ends ({differ} of {all} \
         pixels by 1 at most) and the bar's clock sharp",
        detail(&open, beside.0.clone(), beside.1.clone())
    );
    Ok(())
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
/// The smoke beside the tiles keeps less than this share of the text's
/// detail (a Gaussian of 1.5 to 3 px there).
pub(super) const SMOKE_KEEPS: f64 = 0.5;

/// The control center's measures in the VM, as the shell's (macOS' 12 and
/// 64 pt at Beize's 1.25, 6.5 % smaller): as wide as its columns used, two
/// while network and light/dark are its only tiles; its right edge where
/// the menu bar's margin ends.
pub(super) const CONTROL_CENTER_GAP: usize = 14;
pub(super) const CONTROL_CENTER_TILE: usize = 75;
pub(super) const BAR_MARGIN: usize = 20;
pub(super) const CONTROL_CENTER_WIDTH: usize = 2 * CONTROL_CENTER_TILE + CONTROL_CENTER_GAP;

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
