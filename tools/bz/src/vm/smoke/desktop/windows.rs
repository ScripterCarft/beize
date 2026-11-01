//! Windows: snap, fullscreen, Alt+Tab, frames of other apps, viewports,
//! activation and who comes to the front, hidden windows, resizing.

use super::*;

/// Alt+Tab (ADR 0028 §7), with the client's window in front of the two
/// terminals: a tap goes to the window before and back, whatever shows. Held,
/// the switcher shows the client's live picture in its first tile; Escape
/// changes nothing; Tab, Tab, Shift+Tab selects, the release of Alt chooses; a
/// click on a tile chooses.
pub(super) fn switcher(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let compositor_count = |line: &str| -> Result<usize> {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -b -u beize-compositor.service -o cat | grep -c -x '{line}' \
             | sed 's/^/BEIZE-COUNT /'"
        ))?;
        Ok(lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .unwrap_or(0))
    };
    let chose = |app: &str| format!("switcher chose {app}");
    let tap = |app: &str| -> Result<()> {
        let before = compositor_count(&chose(app))?;
        keys(qmp, &["alt+tab"])?;
        wait_for_said(shell, &chose(app), before + 1)
    };
    tap("beize-terminal")?;
    tap("beize-testclient")?;
    say!("  ok: Alt+Tab went to the window used before, and again back");

    // Held: shown, the client's picture in the first tile.
    let shown = compositor_count("switcher shown")?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab"])?;
    wait_for_said(shell, "switcher shown", shown + 1)?;
    let line = wait_for_shell_line(shell, |l| l.starts_with("switcher: "))?;
    // "switcher: WxH with N windows, the first picture at Some((x, y, w, h))"
    let numbers: Vec<f64> = line
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(|n| n.parse().ok())
        .collect();
    ensure!(numbers.len() == 7, "the switcher said {line:?}");
    let (w, h) = (numbers[0] as usize, numbers[1] as usize);
    ensure!(
        numbers[2] == 3.0,
        "the switcher has not 3 windows: {line:?}"
    );
    let area_h = screen.1 - MENU_BAR - DOCK_ZONE;
    let (left, top) = ((screen.0 - w) / 2, MENU_BAR + (area_h - h) / 2);
    let picture = (
        left + numbers[3] as usize,
        top + numbers[4] as usize,
        numbers[5] as usize,
        numbers[6] as usize,
    );
    let (cx, cy) = (picture.0 + picture.2 / 2, picture.1 + picture.3 / 2);
    // Two thirds out from the middle of the picture lie the client's four
    // colours; the window's own height keeps them inside however it fits.
    let (dx, dy) = (picture.2 / 8, picture.3 / 8);
    wait_until_screen(machine, "switcher", |shown| {
        for (at, colour) in [
            ((cx - dx, cy - dy), QUADRANTS[0]),
            ((cx + dx, cy - dy), QUADRANTS[1]),
            ((cx - dx, cy + dy), QUADRANTS[2]),
            ((cx + dx, cy + dy), QUADRANTS[3]),
        ] {
            let p = shown.pixel(at.0, at.1);
            ensure!(
                same(p, colour),
                "the switcher's first tile does not show the client: {p:?} at {at:?}, not {colour:?}"
            );
        }
        Ok(())
    })?;
    self::screen(machine, "switcher-open")?.write_png(&machine.logs.join("switcher.png"))?;
    // Escape: nothing chosen, nothing raised.
    let chosen = compositor_count(&chose("beize-terminal"))?;
    keys(qmp, &["esc"])?;
    wait_for_said(shell, "switcher hidden", shown + 1)?;
    hold(qmp, "alt", false)?;
    thread::sleep(Duration::from_millis(300));
    ensure!(
        compositor_count(&chose("beize-terminal"))? == chosen,
        "Escape chose a window"
    );
    // A window that opens while it shows joins it; the selection stays: Alt let
    // go chooses the terminal it was on, not the new client.
    let shown = compositor_count("switcher shown")?;
    let gained = "switcher gained beize-testclient";
    let joined = compositor_count(gained)?;
    let chosen = compositor_count(&chose("beize-terminal"))?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab"])?;
    wait_for_said(shell, "switcher shown", shown + 1)?;
    shell.capture(
        "systemd-run --user --unit=beize-switcher-late /run/t/beize-testclient --seconds 30",
    )?;
    wait_for_said(shell, gained, joined + 1)?;
    wait_for_shell_line(shell, |l| {
        l.starts_with("switcher: ") && l.contains(" with 4 windows")
    })?;
    self::screen(machine, "switcher-joined")?
        .write_png(&machine.logs.join("switcher-joined.png"))?;
    hold(qmp, "alt", false)?;
    wait_for_said(shell, &chose("beize-terminal"), chosen + 1)?;
    shell.capture("systemctl --user stop beize-switcher-late")?;
    thread::sleep(Duration::from_millis(500));
    tap("beize-testclient")?;
    // Tab, Tab, Shift+Tab: on the second window; Alt let go chooses it.
    let chosen = compositor_count(&chose("beize-terminal"))?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab", "tab", "shift+tab"])?;
    hold(qmp, "alt", false)?;
    wait_for_said(shell, &chose("beize-terminal"), chosen + 1)?;
    tap("beize-testclient")?;
    // A click on the last tile chooses its terminal.
    let shown = compositor_count("switcher shown")?;
    hold(qmp, "alt", true)?;
    keys(qmp, &["tab"])?;
    wait_for_said(shell, "switcher shown", shown + 1)?;
    thread::sleep(Duration::from_millis(300));
    let step = picture.2 + 16 + 10;
    let tile = (i32::try_from(cx + 2 * step)?, i32::try_from(cy)?);
    let chosen = compositor_count(&chose("beize-terminal"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_said(shell, &chose("beize-terminal"), chosen + 1)?;
    hold(qmp, "alt", false)?;
    park(qmp, screen)?;
    tap("beize-testclient")?;
    say!(
        "  ok: held, the switcher showed the client's live picture in its first tile; Escape \
         chose nothing, a window opening meanwhile joined it, the release of Alt chose the \
         selected window, a click its tile"
    );
    Ok(())
}

/// Snap (ADR 0028 §2, §7), on the client's window at `moved`, `grown`:
/// Super+arrows take the halves from anywhere and maximize, and only down goes
/// back to where it was. Held on the screen's edge in
/// a drag a preview shows, inset and with round corners, and the release takes
/// the half, the margin (os/beize-compositor's `SNAP_MARGIN`) all round;
/// dragged off, the window is its size from before under the pointer; into
/// the menu bar's strip it is maximized.
pub(super) fn snap(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
    let area_h = screen.1 - MENU_BAR - DOCK_ZONE;
    const MARGIN: usize = 10;
    let room = screen.0 - 3 * MARGIN;
    let (top, high) = (MENU_BAR + MARGIN, area_h - 2 * MARGIN);
    let left = ((MARGIN, top), (room / 2, high));
    let right = ((2 * MARGIN + room / 2, top), (room - room / 2, high));
    let whole = ((0, MENU_BAR), (screen.0, area_h));
    let steps: [(&str, Window, &str); 8] = [
        ("meta_l+left", left, "snap-left"),
        ("meta_l+right", right, "snap-right"),
        ("meta_l+down", (moved, grown), "snap-back"),
        ("meta_l+right", right, "snap-right-again"),
        ("meta_l+up", whole, "snap-maximized"),
        ("meta_l+left", left, "snap-left-maximized"),
        ("meta_l+down", (moved, grown), "snap-down"),
        ("meta_l+down", (moved, grown), "snap-down-again"),
    ];
    for (key, (at, size), name) in steps {
        keys(qmp, &[key])?;
        wait_for_pattern(shell, machine, at, size, name)?;
    }
    say!(
        "  ok: Super+arrows took each half, also from the other, maximized, from maximized \
         took a half, and only Super+Down went back"
    );

    // The drag: over the title strip, to the left edge, and held there.
    let grip = (
        i32::try_from(moved.0 + grown.0 / 2)?,
        i32::try_from(moved.1 + CLIENT_TITLE / 2)?,
    );
    // High in the left half, far above the window and its shadow.
    let inside = (screen.0 / 4, MENU_BAR + 40);
    // The preview's corner, and beside its edge.
    let corner = (MARGIN + 1, top + 1);
    let beside = (MARGIN / 2, inside.1);
    // Just right of the dragged window, level with its green quarter, on the
    // preview: what lies behind the glass, not the window in front of it.
    let window_right = usize::try_from(edge_of_drag(grip, moved, grown)?)?;
    let by_green = (window_right + 14, moved.1 + 60);
    let before = self::screen(machine, "snap-before")?;
    let edge = (0, grip.1);
    button(qmp, screen, grip, true)?;
    drag(qmp, screen, grip, edge, false)?;
    wait_until_screen(machine, "snap-preview", |shown| {
        let (lit, was) = (
            shown.pixel(inside.0, inside.1),
            before.pixel(inside.0, inside.1),
        );
        ensure!(
            !same(lit, was),
            "no preview at {inside:?} with the pointer on the screen's edge: {lit:?}, {was:?} before"
        );
        let (out, was) = (
            shown.pixel(beside.0, beside.1),
            before.pixel(beside.0, beside.1),
        );
        ensure!(
            same(out, was),
            "the preview reaches the screen's edge: {out:?} at {beside:?}, {was:?} before"
        );
        let by = shown.pixel(by_green.0, by_green.1);
        ensure!(
            by[1] <= by[2],
            "the preview blurred the window in front of it: {by:?} at {by_green:?}"
        );
        let (tip, was) = (
            shown.pixel(corner.0, corner.1),
            before.pixel(corner.0, corner.1),
        );
        ensure!(
            same(tip, was),
            "the preview's corner is not round: {tip:?} at {corner:?}, {was:?} before"
        );
        Ok(())
    })?;
    // Off the edge again it goes, and the release leaves the window floating.
    let off = (120, grip.1);
    drag(qmp, screen, edge, off, false)?;
    wait_until_screen(machine, "snap-preview-gone", |shown| {
        let (now, was) = (
            shown.pixel(inside.0, inside.1),
            before.pixel(inside.0, inside.1),
        );
        ensure!(
            same(now, was),
            "the preview stayed: {now:?}, {was:?} before"
        );
        Ok(())
    })?;
    drag(qmp, screen, off, edge, false)?;
    button(qmp, screen, edge, false)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, left.0, left.1, "snap-dragged")?;
    // Off the half by its title strip: the size from before, under the pointer.
    let grip = (
        i32::try_from(left.0.0 + left.1.0 / 2)?,
        i32::try_from(left.0.1 + CLIENT_TITLE / 2)?,
    );
    // Where all of it stays clear of the dock.
    let middle = (i32::try_from(screen.0 / 2)?, i32::try_from(MENU_BAR + 100)?);
    drag(qmp, screen, grip, middle, true)?;
    park(qmp, screen)?;
    // As far across it as the pointer was across the half (`leave_maximized`).
    let across = f64::from(grip.0 - i32::try_from(left.0.0)?) / left.1.0 as f64;
    let start = (f64::from(grip.0) - across * grown.0 as f64).round() as i32;
    let centred = (
        usize::try_from(start + middle.0 - grip.0)?,
        MENU_BAR + 100 - CLIENT_TITLE / 2,
    );
    wait_for_pattern(shell, machine, centred, grown, "snap-off")?;
    // Into the menu bar's strip: maximized; down, back to where that drag began.
    drag(qmp, screen, middle, (middle.0, 4), true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, whole.0, whole.1, "snap-top")?;
    keys(qmp, &["meta_l+down"])?;
    wait_for_pattern(shell, machine, centred, grown, "snap-top-back")?;
    // Back to where the test goes on from, by its strip.
    let home = (
        middle.0 + i32::try_from(moved.0)? - i32::try_from(centred.0)?,
        i32::try_from(moved.1 + CLIENT_TITLE / 2)?,
    );
    drag(qmp, screen, middle, home, true)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "snap-end")?;
    say!(
        "  ok: on the screen's edge in a drag a round preview showed and went with the pointer, \
         the release took the half; dragged off, the window had its size from before; into the \
         menu bar's strip it was maximized"
    );
    Ok(())
}

/// Where the right edge of a window `grown` wide at `moved` lies with the
/// pointer, which held it at `grip`, on the screen's left edge.
pub(super) fn edge_of_drag(
    grip: (i32, i32),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<i32> {
    Ok(i32::try_from(moved.0 + grown.0)? - grip.0)
}

/// Super+F gives the client's window (at `moved`, `grown`) the whole screen,
/// the panels come at its edges, and again, it is back.
pub(super) fn fullscreen(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    moved: (usize, usize),
    grown: (usize, usize),
) -> Result<()> {
    // Super+F (ADR 0028 §2): the window fills the whole screen, menu bar and
    // dock hidden. The pointer in the middle; at the top edge it shows the
    // menu bar on live glass (its ink dark, as on light glass), which takes
    // on the window's red and green above – base glass would show the
    // background –, at the bottom the dock. Super+F again, and the window
    // is back in its place.
    let middle = (screen.0 / 2, screen.1 / 2);
    let to = |(x, y): (usize, usize)| -> Result<(i32, i32)> {
        Ok((i32::try_from(x)?, i32::try_from(y)?))
    };
    drag(qmp, screen, to(middle)?, to(middle)?, false)?;
    keys(qmp, &["meta_l+f"])?;
    let difference = machine.logs.join("fullscreen-difference.ppm");
    wait_until_screen(machine, "fullscreen", |shown| {
        // The pointer lies over the window there.
        let pointer = |x: usize, y: usize| x.abs_diff(middle.0) < 32 && y.abs_diff(middle.1) < 32;
        compare(
            shown,
            |x, y| {
                if pointer(x, y) {
                    shown.pixel(x, y)
                } else {
                    expected(x, y, screen.0, screen.1)
                }
            },
            "the fullscreen window",
            &difference,
        )
    })?;
    let bar_at = (middle.0 + 100, MENU_BAR / 2);
    // Below the bar's text, a quarter in from either side: the blur there
    // takes in one quadrant only.
    let red_at = (screen.0 / 4, MENU_BAR - 3);
    let green_at = (screen.0 * 3 / 4, MENU_BAR - 3);
    drag(qmp, screen, to(middle)?, to((middle.0, 0))?, false)?;
    wait_until_screen(machine, "fullscreen-menu-bar", |shown| {
        ensure!(
            !same(shown.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
            "the menu bar did not come at the top edge over the fullscreen window"
        );
        let (red, green) = (
            shown.pixel(red_at.0, red_at.1),
            shown.pixel(green_at.0, green_at.1),
        );
        ensure!(
            leads(red, 0) && leads(green, 1),
            "the menu bar's glass does not show the fullscreen window: {red:?} over its red, \
             {green:?} over its green"
        );
        Ok(())
    })?;
    wait_for_shell_line(shell, |l| l == "menu bar on a Light background")?;
    // Super+R over it: the menu, then the pointer far from the bar: both stay
    // until the menu closes.
    let opened = shell_count(shell, "BeizeMenu open")?;
    keys(qmp, &["meta_l+r"])?;
    wait_for_shell_count(shell, "BeizeMenu open", opened + 1)?;
    drag(qmp, screen, to((middle.0, 0))?, to(middle)?, false)?;
    thread::sleep(Duration::from_millis(300));
    let stays = self::screen(machine, "fullscreen-menu-stays")?;
    ensure!(
        !same(stays.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
        "the menu bar went with the pointer while its menu was open over the fullscreen window"
    );
    let closed = shell_count(shell, "BeizeMenu closed")?;
    keys(qmp, &["meta_l+r"])?;
    wait_for_shell_count(shell, "BeizeMenu closed", closed + 1)?;
    wait_until_screen(machine, "fullscreen-menu-gone", |shown| {
        ensure!(
            same(shown.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
            "the menu bar stayed after its menu closed, the pointer away"
        );
        Ok(())
    })?;
    drag(qmp, screen, to(middle)?, to((middle.0, 0))?, false)?;
    let apps = last_dock(shell)?.len();
    let tile = dock_tile(screen, apps, 0)?;
    let tile = (usize::try_from(tile.0)?, usize::try_from(tile.1)?);
    drag(
        qmp,
        screen,
        to((middle.0, 0))?,
        to((middle.0, screen.1 - 1))?,
        false,
    )?;
    wait_until_screen(machine, "fullscreen-dock", |shown| {
        ensure!(
            !same(
                shown.pixel(tile.0, tile.1),
                expected(tile.0, tile.1, screen.0, screen.1)
            ),
            "the dock did not come at the bottom edge over the fullscreen window"
        );
        ensure!(
            same(shown.pixel(bar_at.0, bar_at.1), QUADRANTS[1]),
            "the menu bar stayed over the fullscreen window with the pointer at the bottom"
        );
        Ok(())
    })?;
    drag(
        qmp,
        screen,
        to((middle.0, screen.1 - 1))?,
        to(middle)?,
        false,
    )?;
    keys(qmp, &["meta_l+f"])?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, grown, "back from fullscreen")?;
    say!(
        "  ok: Super+F gave the window the whole screen; the menu bar came at the top edge on \
         live glass showing the window's colours, the dock at the bottom; again, and it was \
         back in its place"
    );

    Ok(())
}

/// A client that asks for the compositor's frame (xdg-decoration, ADR
/// 0028 §2): it is given it, the frame is centred below the menu bar with
/// the bar above the client's pattern, its buttons the same as Beize UI's
/// to the pixel; the bar moves the window by exactly the drag, a double
/// click maximizes it below the bar and back, its close button ends the
/// client.
pub(super) fn framed(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    ui_buttons: &Image,
) -> Result<()> {
    const UNIT: &str = "beize-testclient-framed";
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --server-side --seconds 60 \
         && for i in $(seq 300); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; journalctl --user -u {UNIT} -o cat"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "decoration server_side"),
        "the client that asked for the compositor's frame was not given it: {lines:?}"
    );
    // The test client's sixth window: the first moved, the second joined
    // Alt+Tab, the third drew frames, the fourth and fifth on fifo and on time.
    let (at, size) = placed(shell, "beize-testclient", 6)?;
    let frame_height = size.1 + TITLE_BAR;
    let centred = (
        (screen.0 - size.0) / 2,
        MENU_BAR + (screen.1 - MENU_BAR - DOCK_ZONE - frame_height) / 2 + TITLE_BAR,
    );
    ensure!(
        at == centred,
        "the framed client's content is at {},{}, not below its bar in a frame centred below \
         the menu bar, at {},{}",
        at.0,
        at.1,
        centred.0,
        centred.1
    );
    wait_for_pattern(shell, machine, at, size, "framed")?;
    let buttons = |(x, y): (usize, usize)| {
        (
            (x + BUTTONS.0, y - TITLE_BAR),
            (BUTTONS.1 - BUTTONS.0, TITLE_BAR),
        )
    };
    wait_for_image(
        machine,
        buttons(at),
        Some(ui_buttons),
        "Beize UI's buttons",
        "framed-buttons",
    )?;
    say!(
        "  ok: a client that asked for the compositor's frame got it, centred below the menu bar, \
         its pattern exactly below the bar, the buttons the same as Beize UI's to the pixel"
    );

    let on_bar = |(x, y): (usize, usize)| -> Result<(i32, i32)> {
        Ok((
            i32::try_from(x + BAR_GRIP_X)?,
            i32::try_from(y - TITLE_BAR / 2)?,
        ))
    };
    let grip = on_bar(at)?;
    let to = (grip.0 + FRAMED_MOVE_BY.0, grip.1 + FRAMED_MOVE_BY.1);
    drag(qmp, screen, grip, to, true)?;
    park(qmp, screen)?;
    let moved = (
        usize::try_from(i32::try_from(at.0)? + FRAMED_MOVE_BY.0)?,
        usize::try_from(i32::try_from(at.1)? + FRAMED_MOVE_BY.1)?,
    );
    wait_for_pattern(shell, machine, moved, size, "framed-moved")?;
    click(qmp, screen, on_bar(moved)?, 2)?;
    park(qmp, screen)?;
    let full = (screen.0, screen.1 - MENU_BAR - DOCK_ZONE - TITLE_BAR);
    wait_for_configure(shell, UNIT, full)?;
    let below_bar = (0, MENU_BAR + TITLE_BAR);
    wait_for_pattern(shell, machine, below_bar, full, "framed-maximized")?;
    click(qmp, screen, on_bar(below_bar)?, 2)?;
    park(qmp, screen)?;
    wait_for_pattern(shell, machine, moved, size, "framed-restored")?;
    let close = (
        i32::try_from(moved.0 + CLOSE_X)?,
        i32::try_from(moved.1 - TITLE_BAR / 2)?,
    );
    click(qmp, screen, close, 1)?;
    park(qmp, screen)?;
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 300); do systemctl --user -q is-active {UNIT} || break; sleep 0.2; done; \
         systemctl --user is-active {UNIT}; journalctl --user -u {UNIT} -o cat | tail -n 1"
    ))?;
    ensure!(
        lines.iter().any(|l| l == "inactive") && lines.iter().any(|l| l == "done"),
        "the close button of the compositor's frame did not end the client: {lines:?}"
    );
    say!(
        "  ok: the compositor's bar moved the window by exactly the drag, a double click \
         maximized it below the bar and back, its close button closed it"
    );
    Ok(())
}

/// A client crops and scales its buffer (wp_viewporter): its window has
/// the size it named and shows the part of its pattern it named – the
/// quadrants split at three quarters, not at the halves of the whole
/// buffer – but where scaling blends the seams.
pub(super) fn viewport(shell: &Shell, machine: &Machine) -> Result<()> {
    const UNIT: &str = "beize-testclient-viewport";
    let (status, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --viewport --seconds 60 \
         && for i in $(seq 300); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; journalctl --user -u {UNIT} -o cat"
    ))?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure ")),
        "the viewport client got no window (exit status {status}): {lines:?}"
    );
    // The test client's seventh window, after the framed one.
    let (at, size) = placed(shell, "beize-testclient", 7)?;
    ensure!(
        size == VIEWPORT_SIZE,
        "the viewport client's window is {}x{}, not the {}x{} it named",
        size.0,
        size.1,
        VIEWPORT_SIZE.0,
        VIEWPORT_SIZE.1
    );
    let difference = machine.logs.join("viewport-difference.ppm");
    let looked = poll_screen(machine, "viewport", |screen| {
        let window = screen.crop(at, size)?;
        compare(
            &window,
            |x, y| viewported(&window, x, y),
            "the part of the pattern the client named",
            &difference,
        )
    })?;
    if let Err((_, e)) = looked {
        let said = last_said(shell, &[UNIT, "beize-compositor"])?;
        return Err(e.context(format!(
            "the viewport client's window, {}x{} at {},{}, on {}; the client and the \
             compositor last said: {said:?}",
            size.0,
            size.1,
            at.0,
            at.1,
            screen_file(machine, "viewport").display()
        )));
    }
    shell.capture(&format!("systemctl --user stop {UNIT}"))?;
    say!(
        "  ok: a client's viewport cropped and scaled its buffer: {}x{}, the part it named",
        size.0,
        size.1
    );
    Ok(())
}

/// What the viewport client's `window` should show at `x`, `y`: the
/// quadrants split at `VIEWPORT_SPLIT`; beside a split, what it shows.
pub(super) fn viewported(window: &Image, x: usize, y: usize) -> [u8; 3] {
    let (sx, sy) = VIEWPORT_SPLIT;
    if x.abs_diff(sx) <= VIEWPORT_SEAM || y.abs_diff(sy) <= VIEWPORT_SEAM {
        return window.pixel(x, y);
    }
    QUADRANTS[usize::from(x >= sx) + 2 * usize::from(y >= sy)]
}

/// Apps hand on activation with a token (xdg-activation) while the focus
/// it was made of lasts: a client's second window asks for its first with
/// a token of its own focus and gets it; a token of that focus once the
/// first has the keyboard, and one without a serial, activate nothing.
pub(super) fn activation(shell: &Shell) -> Result<()> {
    let said = client_summary(
        shell,
        "beize-testclient-activation",
        "--activation",
        "activation no serial: ",
    )?;
    for wanted in [
        "activation active app: granted",
        "activation focus moved: refused",
        "activation no serial: refused",
    ] {
        ensure!(
            said.iter().any(|l| l == wanted),
            "not \"{wanted}\": {said:?}"
        );
    }
    say!(
        "  ok: an app handed on activation with a token of its focus; one of a focus gone, \
         and one without a serial, activated nothing"
    );
    Ok(())
}

/// A window hidden behind another hears "frame done" once a second (Mutter's
/// throttle), also when nothing on the screen changes: a still desktop drew
/// nothing that would wake it, and it waited 3 s and more (0.5.127, VM).
pub(super) fn hidden_window(shell: &Shell, qmp: &Path) -> Result<()> {
    const UNIT: &str = "beize-hidden-window";
    // Its window opens, then Alt+Tab brings back the terminal before it, on the
    // left half as the window tests left it, and Super+Up gives it the screen.
    shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --frames 100000 --seconds 8; \
         for i in $(seq 300); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done"
    ))?;
    keys(qmp, &["alt+tab"])?;
    thread::sleep(Duration::from_millis(500));
    keys(qmp, &["meta_l+up"])?;
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 300); do journalctl --user -u {UNIT} -o cat | grep -q '^frames: ' \
         && break; sleep 0.2; done; journalctl --user -u {UNIT} -o cat | grep '^frames: '; \
         systemctl --user stop {UNIT}"
    ))?;
    let line = lines
        .iter()
        .find(|l| l.starts_with("frames: "))
        .with_context(|| format!("the hidden client said nothing: {lines:?}"))?;
    let most: f64 = line
        .rsplit("at most ")
        .next()
        .and_then(|t| t.trim_end_matches(" ms").parse().ok())
        .with_context(|| format!("no longest wait in {line:?}"))?;
    // A second each; 10 s leaves room for the VM's vCPUs standing (#41). Without a
    // timer it waits for the next change on the screen, here none for the run.
    // Shown, it would draw at the screen's pace: then nothing was tested.
    ensure!(
        most >= 500.0,
        "the client drew at the screen's pace (at most {most} ms between frames): it was not \
         hidden behind the terminal, or once hidden it heard \"frame done\" no more: {line}"
    );
    ensure!(
        most < 10_000.0,
        "a hidden window waited {most} ms for \"frame done\", not about a second: {line}"
    );
    // Back to the left half, where the tests after it look for the terminal.
    keys(qmp, &["meta_l+left"])?;
    say!(
        "  ok: a window behind the maximized terminal heard \"frame done\" at least every {most:.0} ms"
    );
    Ok(())
}

/// A window nobody asked for opens behind the active one, without the keyboard.
pub(super) fn not_forward(shell: &Shell) -> Result<()> {
    const UNIT: &str = "unasked-window";
    let (_, lines) = shell.capture(&format!(
        "systemd-run --user --unit={UNIT} /run/t/beize-testclient --seconds 30; \
         for i in $(seq 300); do journalctl --user -u {UNIT} -o cat | grep -q '^configure ' \
         && break; sleep 0.2; done; sleep 1; journalctl --user -u {UNIT} -o cat \
         | grep -E '^(configure|keyboard focus)'; systemctl --user stop {UNIT}; \
         journalctl --user -b -u beize-compositor.service -o cat \
         | grep -c 'opens behind the active window'"
    ))?;
    ensure!(
        lines.iter().any(|l| l.starts_with("configure "))
            && !lines.iter().any(|l| l.starts_with("keyboard focus")),
        "a window nobody asked for got the keyboard: {lines:?}"
    );
    ensure!(
        lines.last().is_some_and(|l| l.trim() == "1"),
        "the compositor did not say it opened behind: {lines:?}"
    );
    say!("  ok: a window nobody asked for opened behind the active one, without the keyboard");
    Ok(())
}

/// Waits until the compositor said `line` (a basic regular expression for
/// the whole line), in this boot; the last line it took. If it does not,
/// the error has what the compositor and the shell said last.
/// Resizes Beize UI's window by its right edge as fast as a 1000 Hz mouse
/// reports, out by `FAST_RESIZE_BY` and back: it has to take the sizes its
/// frames can show, not draw the hundreds in between one frame after another
/// (0.5.105 on the owner's PC: the window trailed the pointer ever more, until
/// its buffers filled the GPU). Counted, not timed: here the frames are drawn
/// on the VM's CPU; how soon it settles is `hw-test`'s, on the hardware.
pub(super) fn fast_resize(
    shell: &Shell,
    qmp: &Path,
    screen: (usize, usize),
    window: Window,
) -> Result<()> {
    let ((x, y), (w, h)) = window;
    let edge = (i32::try_from(x + w + 2)?, i32::try_from(y + h / 2)?);
    let out = (edge.0 + FAST_RESIZE_BY, edge.1);
    let sizes = || -> Result<usize> {
        let (_, lines) = shell.capture(
            "journalctl --user -b -u beize-ui-test -o cat | grep -c '^configure ' \
             | sed 's/^/BEIZE-COUNT /'",
        )?;
        lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .context("the sizes Beize UI's test program took were not counted")
    };
    let before = sizes()?;
    swipe(qmp, screen, &[edge, out, edge], Duration::from_millis(1))?;
    park(qmp, screen)?;
    let wanted = format!("resized beize-ui-test to {w}x{h}, ");
    let (_, lines) = shell.capture(&format!(
        "for i in $(seq 300); do journalctl --user -b -u beize-compositor.service -o cat \
         | grep -q '^{wanted}' && break; sleep 0.2; done; \
         journalctl --user -b -u beize-compositor.service -o cat | grep '^resized ' | tail -n 1"
    ))?;
    let said = lines
        .iter()
        .find(|l| l.starts_with("resized "))
        .with_context(|| format!("the compositor ended no resize: {lines:?}"))?;
    let ms: u64 = said
        .strip_prefix(&wanted)
        .and_then(|rest| rest.split_once(" ms"))
        .and_then(|(ms, _)| ms.parse().ok())
        .with_context(|| format!("the window did not settle in {w}x{h}: \"{said}\""))?;
    let took = sizes()?.saturating_sub(before);
    let dragged = 2 * usize::try_from(FAST_RESIZE_BY)?;
    ensure!(
        took < dragged / 4,
        "resized fast, Beize UI's test program took {took} of the {dragged} sizes it was dragged \
         through: it drew them one by one"
    );
    say!(
        "  ok: resized as fast as a 1000 Hz mouse, out by {FAST_RESIZE_BY} px and back, the \
         window took {took} of {dragged} sizes and settled, {ms} ms after the button was released"
    );
    Ok(())
}

/// How many pixels of the arrow's body (`ARROW_BODY`) show with the
/// pointer at `at`: the screen then against the screen with the pointer
/// parked. The pointer is drawn into the frame in the VM. Parks it.
pub(super) fn arrow_body(
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    at: (i32, i32),
    name: &str,
) -> Result<usize> {
    let with = machine.logs.join(format!("{name}-screen.ppm"));
    let without = machine.logs.join(format!("{name}-parked.ppm"));
    // A frame for the pointer's last move.
    thread::sleep(Duration::from_millis(300));
    screendump(qmp, &with)?;
    park(qmp, screen)?;
    thread::sleep(Duration::from_millis(300));
    screendump(qmp, &without)?;
    let (a, b) = (
        Image::read_ppm(&fs::read(&with)?)?,
        Image::read_ppm(&fs::read(&without)?)?,
    );
    let (x, y) = (usize::try_from(at.0)?, usize::try_from(at.1)?);
    let mut differ = 0;
    for dy in ARROW_BODY.1.clone() {
        for dx in ARROW_BODY.0.clone() {
            if a.pixel(x + dx, y + dy) != b.pixel(x + dx, y + dy) {
                differ += 1;
            }
        }
    }
    if differ < ARROW_SHOWS {
        a.write_png(&machine.logs.join(format!("{name}-screen.png")))?;
    }
    Ok(differ)
}
