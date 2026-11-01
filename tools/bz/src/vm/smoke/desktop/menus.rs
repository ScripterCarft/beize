//! Menus: the Beize menu, context menus, the confirmation dialog, the app
//! menus in the bar and the system's own.

use super::*;

/// Waits until the journal of the user's session has `line` at least `count`
/// times.
pub(super) fn wait_for_said(shell: &Shell, line: &str, count: usize) -> Result<()> {
    let mut seen = 0;
    for _ in 0..50 {
        let (_, lines) = shell.capture(&format!(
            "journalctl --user -b -o cat | grep -c -x '{line}' | sed 's/^/BEIZE-COUNT /'"
        ))?;
        seen = lines
            .iter()
            .find_map(|l| l.strip_prefix("BEIZE-COUNT ")?.trim().parse().ok())
            .unwrap_or(0);
        if seen >= count {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("the session said {line:?} {seen} times, not {count}")
}

/// The Beize menu's row `i` of its commands, below the bar (os/beize-ui's
/// measures): its 6 px, rows of 30 and the line of 14 before the last.
pub(super) fn beize_menu_row(beize: (i32, i32), rows: i32, line: bool) -> (i32, i32) {
    let y = i32::try_from(MENU_BAR).unwrap_or(32) + 6 + rows * 30 + 15 + if line { 14 } else { 0 };
    (beize.0 + 40, y)
}

/// Restart asks first (ADR 0028 §3). Escape says no. A click on a window takes
/// the keyboard, so Return there answers nothing (the review's case: typing
/// must not restart the computer); a click on the dialog gives it back. On
/// Log Out, `still_and_signed_out` says yes with Return.
pub(super) fn confirmation(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let (beize, _) = bar_buttons(shell)?;
    let restart = beize_menu_row(beize, 1, false);
    let middle = (
        screen.0 / 2,
        MENU_BAR + (screen.1 - MENU_BAR - DOCK_ZONE) / 2,
    );
    let spot = |image: &Image| image.pixel(middle.0, middle.1);
    let before = spot(&self::screen(machine, "confirm-before")?);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    let ask = |n: usize| -> Result<()> {
        let opened = shell_count(shell, "BeizeMenu open")?;
        click(qmp, screen, beize, 1)?;
        wait_for_shell_count(shell, "BeizeMenu open", opened + 1)?;
        thread::sleep(Duration::from_millis(300));
        click(qmp, screen, restart, 1)?;
        wait_for_shell_count(shell, "asking: Power(Restart)", n)?;
        wait_until_screen(machine, "confirm-open", |shown| {
            let p = spot(shown);
            ensure!(
                brightness(p) > brightness(before) + 8,
                "no dialog on glass at {middle:?}: {p:?}, {before:?} before"
            );
            Ok(())
        })
    };
    ask(1)?;
    thread::sleep(Duration::from_millis(300));
    let asked = self::screen(machine, "confirm")?;
    asked.write_png(&machine.logs.join("confirm.png"))?;
    // Stacked, as macOS' alerts: the action in the accent colour, Cancel below
    // it not, left of the labels (os/beize-ui's dialog, 189 px high).
    let beside = middle.0 - 100;
    let (action, cancel) = (
        asked.pixel(beside, middle.1 + 14),
        asked.pixel(beside, middle.1 + 57),
    );
    ensure!(
        leads(action, 2) && !leads(cancel, 2),
        "the dialog's buttons are not stacked: {action:?} above {cancel:?}"
    );
    // The first arrow leaves the action: Return then says no, with an outline.
    keys(qmp, &["down"])?;
    let outlined = (middle.0, middle.1 + 57 - 35 / 2 - 3);
    wait_until_screen(machine, "confirm-picked", |shown| {
        ensure!(
            !same(
                shown.pixel(outlined.0, outlined.1),
                asked.pixel(outlined.0, outlined.1)
            ),
            "no outline around Cancel at {outlined:?} after an arrow"
        );
        Ok(())
    })?;
    keys(qmp, &["ret"])?;
    wait_for_shell_count(shell, "asked: Power(Restart), no", 1)?;

    ask(2)?;
    // Alt+F4 does not reach the window behind a dialog with the keyboard.
    let open_terminals = terminals(shell)?.len();
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["alt+f4"])?;
    thread::sleep(Duration::from_millis(500));
    ensure!(
        terminals(shell)?.len() == open_terminals,
        "Alt+F4 closed the window behind the dialog"
    );
    // In the terminal where the compositor placed it, near its top left, far
    // from the dialog in the middle: Return goes there. A fixed point hit the
    // terminal only as long as the windows before it happened to stack so.
    let ((x, y), _) = last_placed(shell, "beize-terminal")?;
    click(
        qmp,
        screen,
        (i32::try_from(x + 40)?, i32::try_from(y + TITLE_BAR + 40)?),
        1,
    )?;
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["ret"])?;
    thread::sleep(Duration::from_millis(500));
    ensure!(
        shell_count(shell, "asked: Power(Restart), yes")? == 0
            && shell_count(shell, "asked: Power(Restart), no")? == 1,
        "Return with a window active answered the dialog"
    );
    // Above the buttons, on the dialog: the keyboard is its again.
    let on_dialog = (i32::try_from(middle.0)?, i32::try_from(middle.1)? - 40);
    click(qmp, screen, on_dialog, 1)?;
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "asked: Power(Restart), no", 2)?;
    // An app's window ending, the active one, under the open dialog: Return must
    // not go past it to what lies behind (it did).
    let shown = shell_count(shell, "menu bar: beize-testclient")?;
    shell.capture(
        "systemd-run --user --unit=beize-testclient-under /run/t/beize-testclient \
         --frames 100000 --seconds 60",
    )?;
    wait_for_shell_count(shell, "menu bar: beize-testclient", shown + 1)?;
    ask(3)?;
    thread::sleep(Duration::from_millis(300));
    shell.capture("systemctl --user stop beize-testclient-under")?;
    thread::sleep(Duration::from_millis(500));
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "asked: Power(Restart), no", 3)?;
    park(qmp, screen)?;
    say!(
        "  ok: Restart asked first; an arrow and Return said no; with a window clicked Return did not answer \
         the dialog, a click on it gave it the keyboard back, and a window ending left it there"
    );
    Ok(())
}

/// The active terminal's menus in the bar (ADR 0028 §3): moving from one open
/// menu to the next opens that, as do the arrows; Select All reaches the app;
/// File's New Window and Close Window work. A shell started anew shows them
/// again: they are kept with the window.
pub(super) fn app_menus(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let menus = |line: String| -> Result<Vec<(i32, i32)>> {
        line.strip_prefix("menu bar menus: ")
            .context("not the bar's menus")?
            .split(' ')
            .map(|p| {
                let (x, y) = p.split_once(',').context("not a place")?;
                Ok((x.parse()?, y.parse()?))
            })
            .collect()
    };
    let places = |least: usize| -> Result<Vec<(i32, i32)>> {
        let (_, lines) = shell.capture(
            "journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: ' \
             | tail -n 1",
        )?;
        let found = lines
            .iter()
            .find(|l| l.starts_with("menu bar menus: "))
            .cloned()
            .context("the shell named no menus")?;
        let found = menus(found)?;
        ensure!(
            found.len() >= least,
            "the bar has {} menus: {found:?}",
            found.len()
        );
        Ok(found)
    };
    // The Beize menu, the app's name, File, Edit, Window.
    let bar = places(5)?;
    self::screen(machine, "app-menus-bar")?.write_png(&machine.logs.join("app-menus-bar.png"))?;
    let (file, edit) = (bar[2], bar[3]);
    let row = |at: (i32, i32), i: i32| {
        (
            at.0 + 30,
            i32::try_from(MENU_BAR).unwrap_or(32) + 6 + i * 30 + 15,
        )
    };
    let edit_open = "AppMenu(1) open";
    let n = shell_count(shell, edit_open)?;
    click(qmp, screen, edit, 1)?;
    wait_for_shell_count(shell, edit_open, n + 1)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "app-menu")?.write_png(&machine.logs.join("app-menu.png"))?;
    // Onto File's title: File opens, Edit closes.
    let m = shell_count(shell, "AppMenu(0) open")?;
    drag(qmp, screen, edit, file, false)?;
    wait_for_shell_count(shell, "AppMenu(0) open", m + 1)?;
    // The right arrow: Edit again.
    keys(qmp, &["right"])?;
    wait_for_shell_count(shell, edit_open, n + 2)?;
    // Select All, its third row, reaches the app.
    let selected = 1 + usize::from(true);
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(edit, 2), 1)?;
    wait_for_said(shell, "menu: SelectAll", selected)?;
    // New Window, then Close Window on that new window.
    let count = terminals(shell)?.len();
    let n = shell_count(shell, "AppMenu(0) open")?;
    click(qmp, screen, file, 1)?;
    wait_for_shell_count(shell, "AppMenu(0) open", n + 1)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(file, 0), 1)?;
    wait_for_terminals(shell, count + 1)?;
    thread::sleep(Duration::from_millis(500));
    let file_now = places(5)?[2];
    let n = shell_count(shell, "AppMenu(0) open")?;
    click(qmp, screen, file_now, 1)?;
    wait_for_shell_count(shell, "AppMenu(0) open", n + 1)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(file_now, 1), 1)?;
    wait_for_terminals(shell, count)?;
    park(qmp, screen)?;
    say!(
        "  ok: the terminal's menus showed in the bar; moving and the arrows went from menu to \
         menu; Select All, New Window and Close Window reached the app"
    );

    // A new shell hears them again.
    let said = |lines: Vec<String>| {
        lines
            .into_iter()
            .filter(|l| l.starts_with("menu bar menus: "))
            .count()
    };
    let (_, before) = shell
        .capture("journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: '")?;
    let before = said(before);
    let drawn = "ready: menu bar and dock drawn";
    let ready = shell_count(shell, drawn)?;
    shell.capture("systemctl --user kill -s KILL beize-shell.service")?;
    let mut now = before;
    for _ in 0..50 {
        let (_, lines) = shell.capture(
            "journalctl --user -b -u beize-shell.service -o cat | grep '^menu bar menus: '",
        )?;
        now = said(lines);
        if now > before && places(5).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    ensure!(
        now > before && places(5).is_ok(),
        "a shell started anew did not show the app's menus"
    );
    say!("  ok: a shell started anew showed the terminal's menus again");
    // Until it is ready the compositor shows the old panels, which hear no click.
    wait_for_shell_count(shell, drawn, ready + 1)?;
    thread::sleep(Duration::from_millis(300));
    system_menus(shell, machine, qmp, screen, places(5)?)
}

/// The menus the system adds to every app's (ADR 0028 §3): Window's Move &
/// Resize opens beside its row, by the pointer or the keys, and moves the
/// window; Zoom and Minimize; the app's name has About.
pub(super) fn system_menus(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
    bar: Vec<(i32, i32)>,
) -> Result<()> {
    let (name, window) = (bar[1], bar[4]);
    let top = i32::try_from(MENU_BAR)? + 6;
    let open = |at: (i32, i32), line: &str| -> Result<()> {
        let n = shell_count(shell, line)?;
        click(qmp, screen, at, 1)?;
        wait_for_shell_count(shell, line, n + 1)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    };
    let open_window = || open(window, "WindowMenu open");
    // Its fourth row, past the line of 14; beside it, over the menu's 225 px.
    let move_row = (window.0, top + 3 * 30 + 14 + 15);
    let beside = |row: i32| (window.0 + 260, move_row.1 + row * 30);
    let open_move = || -> Result<()> {
        open_window()?;
        let n = shell_count(shell, "submenu open")?;
        drag(qmp, screen, window, move_row, false)?;
        wait_for_shell_count(shell, "submenu open", n + 1)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    };
    let dark = |at: (usize, usize), name: &str, want: bool| -> Result<()> {
        wait_until_screen(machine, name, |shown| {
            let p = shown.pixel(at.0, at.1);
            let brightness = p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
            ensure!(
                (brightness < WINDOW_DARK) == want,
                "{name}: {p:?} at {at:?}, the terminal {} there",
                if want { "should be" } else { "should not be" }
            );
            Ok(())
        })
    };
    let area_h = screen.1 - MENU_BAR - DOCK_ZONE;
    let right_half = (screen.0 * 3 / 4, MENU_BAR + area_h / 2);
    // Low in the left half, beside the prompt: no text of the terminal.
    let low_left = (320, MENU_BAR + area_h - 40);

    // By the keys: Right opens it, Escape closes all.
    open_window()?;
    let n = shell_count(shell, "submenu open")?;
    let closed = shell_count(shell, "WindowMenu closed")?;
    keys(qmp, &["down", "down", "down", "down", "right"])?;
    wait_for_shell_count(shell, "submenu open", n + 1)?;
    thread::sleep(Duration::from_millis(300));
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "WindowMenu closed", closed + 1)?;

    // By the pointer: Left, then back.
    open_move()?;
    self::screen(machine, "window-menu")?.write_png(&machine.logs.join("window-menu.png"))?;
    let snapped = "snapped beize-terminal Left";
    let n = shell_count(shell, snapped)?;
    click(qmp, screen, beside(0), 1)?;
    wait_for_said(shell, snapped, n + 1)?;
    park(qmp, screen)?;
    dark(right_half, "window-left", false)?;
    dark(low_left, "window-left-low", true)?;
    open_move()?;
    click(qmp, screen, beside(2), 1)?;
    park(qmp, screen)?;
    dark(low_left, "window-back", false)?;
    // Zoom, its second row.
    open_window()?;
    click(qmp, screen, (window.0, top + 30 + 15), 1)?;
    park(qmp, screen)?;
    dark(right_half, "window-zoom", true)?;
    // Minimize, its first; the dock's tile brings it back.
    open_window()?;
    let minimized = "minimized beize-terminal";
    let n = shell_count(shell, minimized)?;
    click(qmp, screen, (window.0, top + 15), 1)?;
    wait_for_said(shell, minimized, n + 1)?;
    park(qmp, screen)?;
    dark(right_half, "window-minimized", false)?;
    let tile = dock_tile(screen, last_dock(shell)?.len(), 0)?;
    click(qmp, screen, tile, 1)?;
    park(qmp, screen)?;
    dark(right_half, "window-unminimized", true)?;
    say!(
        "  ok: the Window menu's Move & Resize opened by keys and pointer and moved the window, \
         back again; Zoom, Minimize"
    );

    // The app's name: About, closed with Escape.
    open(name, "AppName open")?;
    let about = shell_count(shell, "about")?;
    click(qmp, screen, (name.0 + 30, top + 15), 1)?;
    wait_for_shell_count(shell, "about", about + 1)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "about")?.write_png(&machine.logs.join("about.png"))?;
    let closed = shell_count(shell, "about closed")?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "about closed", closed + 1)?;
    park(qmp, screen)?;
    say!("  ok: the app's name menu's About showed the dialog, Escape closed it");
    Ok(())
}

/// Context menus (ADR 0028 §3, §4): a right click in the terminal opens a
/// menu on the compositor's glass, over the dark terminal light as a menu is;
/// Escape closes it. A disabled command chooses nothing; with something
/// selected it does. A drag on its title bar closes the menu and moves the
/// window, as beside the shell's. The dock's tile has a menu above it: New
/// Window opens one, and Alt+F4 closes it.
pub(super) fn context_menus(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    // Maximized, as the control center's test left it.
    let (x, y, w) = (0, i32::try_from(MENU_BAR)?, i32::try_from(screen.0)?);
    let at = (x + 200, y + i32::try_from(TITLE_BAR)? + 200);
    // A row of the menu, by the entries' places in os/beize-ui's measures.
    let row = |i: i32| (at.0 + 60, at.1 + 6 + i * 30 + 15);
    let inside = (at.0 + 180, at.1 + 6 + 30 + 15);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    let spot = |image: &Image| image.pixel(inside.0 as usize, inside.1 as usize);
    let before = spot(&self::screen(machine, "context-before")?);
    ensure!(
        brightness(before) < WINDOW_DARK,
        "the terminal is not at {inside:?} ({before:?}): the check would prove nothing"
    );
    right_click(qmp, screen, at)?;
    wait_until_screen(machine, "context-open", |shown| {
        let p = spot(shown);
        ensure!(
            brightness(p) > brightness(before) + 8,
            "no menu on glass over the terminal: {p:?}, {before:?} before"
        );
        Ok(())
    })?;
    self::screen(machine, "context-menu")?.write_png(&machine.logs.join("context-menu.png"))?;
    // Copy with nothing selected: nothing; the menu stays.
    click(qmp, screen, row(0), 1)?;
    thread::sleep(Duration::from_millis(300));
    ensure!(
        brightness(spot(&self::screen(machine, "context-disabled")?)) > brightness(before) + 8,
        "a disabled command closed the menu"
    );
    keys(qmp, &["esc"])?;
    wait_until_screen(machine, "context-closed", |shown| {
        ensure!(same(spot(shown), before), "Escape left the menu open");
        Ok(())
    })?;
    // Select All, then Copy is enabled.
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(2), 1)?;
    wait_for_said(shell, "menu: SelectAll", 1)?;
    park(qmp, screen)?;
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    click(qmp, screen, row(0), 1)?;
    wait_for_said(shell, "menu: Copy", 1)?;
    // With the menu open, a drag on the title bar closes it and moves the
    // window (`press_ends_popup_grab`), off its maximized place; maximized
    // again after.
    let grip = (x + w / 2, y + i32::try_from(TITLE_BAR / 2)?);
    let moved = by(grip, (0, 60));
    right_click(qmp, screen, at)?;
    thread::sleep(Duration::from_millis(300));
    let over_bar = (x + w - 40, grip.1);
    let was = self::screen(machine, "context-bar-before")?
        .pixel(usize::try_from(over_bar.0)?, usize::try_from(over_bar.1)?);
    drag(qmp, screen, grip, moved, true)?;
    park(qmp, screen)?;
    wait_until_screen(machine, "context-dragged", |shown| {
        let p = shown.pixel(usize::try_from(over_bar.0)?, usize::try_from(over_bar.1)?);
        ensure!(
            !same(p, was),
            "the window did not move with the drag that closed its menu: {p:?} where its title bar was"
        );
        Ok(())
    })?;
    keys(qmp, &["meta_l+up"])?;
    say!(
        "  ok: a right click opened the terminal's menu on glass; a disabled command did nothing, \
         Escape closed it, with a selection Copy chose; a drag on the title bar closed it and \
         moved the window"
    );

    // The dock's tile: New Window, then Alt+F4 closes that window.
    let count = terminals(shell)?.len();
    let apps = last_dock(shell)?;
    let tile = dock_tile(screen, apps.len(), 0)?;
    right_click(qmp, screen, tile)?;
    wait_for_shell_count(shell, "DockMenu open", 1)?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "DockMenu closed", 1)?;
    right_click(qmp, screen, tile)?;
    wait_for_shell_count(shell, "DockMenu open", 2)?;
    thread::sleep(Duration::from_millis(300));
    self::screen(machine, "dock-menu")?.write_png(&machine.logs.join("dock-menu.png"))?;
    // Above the tile: its window, a line, New Window, Quit.
    // From the tile's top, the panel's padding above it: Quit, then New Window.
    let new_window = (tile.0 + 40, tile.1 - 28 - 6 - 30 - 15);
    click(qmp, screen, new_window, 1)?;
    wait_for_shell_count(shell, "DockMenu closed", 2)?;
    wait_for_terminals(shell, count + 1)?;
    keys(qmp, &["alt+f4"])?;
    wait_for_terminals(shell, count)?;
    park(qmp, screen)?;
    say!("  ok: the dock's tile opened its menu, New Window a window, and Alt+F4 closed it");
    Ok(())
}

/// The bar's menus (ADR 0028 §3, §5). The Beize menu opens on its glass –
/// light where the background showed before –, and logind lets the session
/// act; Escape closes it (the keyboard is the menu's), a click elsewhere
/// does too (the pointer is), and with it open a press on the control
/// center's button opens that. Its light/dark tile switches the appearance
/// and back; the compositor keeps the choice. With the menu open, a drag
/// on a window's title bar closes it and moves the window (owner, 28.10.:
/// the press went to the menu's grab, the move was refused). The release of
/// an Escape that closed the menu leaves the control center open. The screens of
/// the open
/// menus are in the logs for the owner (`beize-menu.png`,
/// `control-center-*.png`).
pub(super) fn menus(
    shell: &Shell,
    machine: &Machine,
    qmp: &Path,
    screen: (usize, usize),
) -> Result<()> {
    let (beize, centre) = bar_buttons(shell)?;
    // Inside the menu, below its first row's text.
    let inside = (usize::try_from(beize.0)? + 40, MENU_BAR + 10);
    let before = self::screen(machine, "menu-before")?.pixel(inside.0, inside.1);
    let brightness = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
    ensure!(
        brightness(before) < MENU_LIGHT,
        "the background at {},{} is {before:?}, as light as a menu: the check would prove nothing",
        inside.0,
        inside.1
    );
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 1)?;
    let may = wait_for_shell_line(shell, |l| l.starts_with("Beize menu may: "))?;
    let allowed = may.contains("restart yes") && may.contains("shut down yes");
    // polkit takes the user's display session for the shell, which runs
    // outside any session (sd_uid_get_display). logind elects it when a
    // session starts, not when beize-session makes tty1 a Wayland session
    // (SetType): the serial console, signed in first, stays the display –
    // without a seat, not local to polkit, which asks for a password. The
    // fix is upstream's to take (~/upstream/systemd-logind-display, sent by
    // the owner); until then exactly that case is said, any other fails.
    let (_, said) = shell.capture(
        "d=$(loginctl show-user $(id -u) -p Display --value); loginctl show-session $d \
         -p TTY -p Seat | paste -sd ' ' | sed 's/^/BEIZE-SAID /'",
    )?;
    let display = said
        .iter()
        .find_map(|l| l.strip_prefix("BEIZE-SAID "))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let fields: Vec<&str> = display.split_whitespace().collect();
    let upstream = fields.contains(&"TTY=ttyS0") && fields.contains(&"Seat=");
    ensure!(
        allowed || upstream,
        "logind does not let the session act: {may} (display session: {display})"
    );
    if !allowed {
        say!(
            "  known: logind keeps the serial console as the display ({display}), polkit asks \
             for a password ({may}) – logind's SetType elects no display, upstream patch \
             prepared"
        );
    }
    wait_until_screen(machine, "beize-menu", |shown| {
        let p = shown.pixel(inside.0, inside.1);
        ensure!(
            brightness(p) >= MENU_LIGHT,
            "the menu at {},{} shows {p:?}, not its light glass",
            inside.0,
            inside.1
        );
        Ok(())
    })?;
    self::screen(machine, "beize-menu")?.write_png(&machine.logs.join("beize-menu.png"))?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "BeizeMenu closed", 1)?;
    // Elsewhere: the screen's middle (the terminal), far from the shell.
    let elsewhere = (i32::try_from(screen.0 / 2)?, i32::try_from(screen.1 / 2)?);
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 2)?;
    click(qmp, screen, elsewhere, 1)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 2)?;
    say!(
        "  ok: the Beize menu opened on its glass, logind answered ({}), and Escape or a click \
         elsewhere closed it",
        may.trim_start_matches("Beize menu may: ")
    );

    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 3)?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 3)?;
    wait_for_shell_count(shell, "ControlCenter open", 1)?;
    // The light/dark tile: the second row's first, below the bar and a gap
    // (os/beize-shell/src/control_center.rs).
    let left = screen.0 - BAR_MARGIN - CONTROL_CENTER_WIDTH;
    let tile = (
        i32::try_from(left + CONTROL_CENTER_TILE / 2)?,
        i32::try_from(
            MENU_BAR
                + CONTROL_CENTER_GAP
                + CONTROL_CENTER_TILE
                + CONTROL_CENTER_GAP
                + CONTROL_CENTER_TILE / 2,
        )?,
    );
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "control-center")?
        .write_png(&machine.logs.join("control-center-light.png"))?;
    let light = left_mean(&self::screen(machine, "light")?);
    click(qmp, screen, tile, 1)?;
    wait_for_compositor_line(shell, "appearance Dark, chosen")?;
    wait_for_shell_line(shell, |l| l == "appearance Dark")?;
    let kept = keep_says(shell)?;
    ensure!(kept == "dark", "the compositor kept {kept:?}, not dark");
    wait_until_screen(machine, "dark", |shown| {
        let dark = left_mean(shown);
        ensure!(
            dark < light,
            "the background is no darker in dark ({dark:.0}) than in light ({light:.0})"
        );
        Ok(())
    })?;
    thread::sleep(Duration::from_millis(500));
    self::screen(machine, "control-center")?
        .write_png(&machine.logs.join("control-center-dark.png"))?;
    click(qmp, screen, tile, 1)?;
    wait_for_compositor_line(shell, "appearance Light, chosen")?;
    let kept = keep_says(shell)?;
    ensure!(kept == "light", "the compositor kept {kept:?}, not light");
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 1)?;
    say!(
        "  ok: a press on the control center's button closed the Beize menu and opened it; its \
         tile switched to dark and back, and the compositor kept the choice"
    );

    // With the menu open, a press on a window's title bar closes the menu
    // and drags the window at once (owner, 28.10.): the press is not the
    // menu's grab's but the window's, which asks to move with it.
    let (_, lines) = shell.capture(PLACED)?;
    let ((x, y), (w, _)) = lines
        .iter()
        .rev()
        .find_map(|l| placement(l, "beize-terminal"))
        .ok_or_else(|| anyhow::anyhow!("no terminal's window was placed"))?;
    let (x, y, w) = (i32::try_from(x)?, i32::try_from(y)?, i32::try_from(w)?);
    let grip = (x + w / 2, y + i32::try_from(TITLE_BAR / 2)?);
    // In the title bar's right end before, the desktop once moved off; left
    // of the window before, in its title bar once moved.
    let left_behind = (x + w - 40, grip.1);
    let arrived = (x - 40, grip.1 + MENU_DRAG_BY.1);
    let dark = |p: [u8; 3]| p.iter().map(|&c| u32::from(c)).sum::<u32>() / 3 < WINDOW_DARK;
    let at = |image: &Image, (px, py): (i32, i32)| -> Result<[u8; 3]> {
        Ok(image.pixel(usize::try_from(px)?, usize::try_from(py)?))
    };
    let before = self::screen(machine, "drag-before")?;
    ensure!(
        dark(at(&before, left_behind)?) && !dark(at(&before, arrived)?),
        "the terminal's title bar is not where it was placed ({}x{} at {x},{y}): the check \
         would prove nothing",
        w,
        TITLE_BAR
    );
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 4)?;
    drag(qmp, screen, grip, by(grip, MENU_DRAG_BY), true)?;
    park(qmp, screen)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 4)?;
    wait_until_screen(machine, "dragged-from-menu", |shown| {
        let (gone, there) = (at(shown, left_behind)?, at(shown, arrived)?);
        ensure!(
            !dark(gone) && dark(there),
            "the terminal did not move with the drag that closed the menu: {gone:?} where its \
             title bar was, {there:?} where it should be"
        );
        Ok(())
    })?;
    say!(
        "  ok: with the Beize menu open, a drag on a window's title bar closed it and moved the window"
    );

    // Escape closes the menu, the control center opens while it is still
    // held: its release goes to the control center, which stays (owner,
    // 29.10.: it closed on the release).
    click(qmp, screen, beize, 1)?;
    wait_for_shell_count(shell, "BeizeMenu open", 5)?;
    hold(qmp, "esc", true)?;
    wait_for_shell_count(shell, "BeizeMenu closed", 5)?;
    click(qmp, screen, centre, 1)?;
    wait_for_shell_count(shell, "ControlCenter open", 2)?;
    hold(qmp, "esc", false)?;
    thread::sleep(Duration::from_secs(1));
    let closed = shell_count(shell, "ControlCenter closed")?;
    ensure!(
        closed == 1,
        "the control center closed when the Escape that closed the Beize menu was let go"
    );
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, "ControlCenter closed", 2)?;
    say!(
        "  ok: the control center, opened while the Escape that closed the Beize menu was held, \
         stayed when it was let go"
    );
    liquid_glass(shell, machine, qmp, screen, centre)?;
    control_center_smoke(shell, machine, qmp, screen, centre)
}

/// How far the drag with the Beize menu open moves the terminal: clear of
/// where its title bar's right end was, over 40 px left of where it began.
pub(super) const MENU_DRAG_BY: (i32, i32) = (-160, 100);
/// A terminal's title bar is darker than this (mean of the channels); the
/// desktop in light is lighter.
pub(super) const WINDOW_DARK: u32 = 90;

/// The mean of the channels at the screen's left edge, 100 pixels a side
/// at half its height: the background, beside the windows in the middle.
pub(super) fn left_mean(screen: &Image) -> f64 {
    let y = screen.height / 2;
    let rows = y.saturating_sub(50)..(y + 50).min(screen.height);
    let n = rows.len().max(1) as f64;
    rows.map(|row| row_mean(screen, row, 8..108.min(screen.width)))
        .sum::<f64>()
        / n
}

/// How light a menu's glass is at least, as the mean of a pixel's channels:
/// its tint (#F2F2F2 at 85 %) over anything.
pub(super) const MENU_LIGHT: u32 = 200;
/// Where the bar's buttons are, as the shell said last: the Beize menu's
/// and the control center's (the clock's width moves it).
pub(super) fn bar_buttons(shell: &Shell) -> Result<((i32, i32), (i32, i32))> {
    let line = wait_for_shell_line(shell, |l| l.starts_with("menu bar buttons: "))?;
    let (_, lines) = shell.capture(&format!("{SHELL_SAID} | grep '^menu bar buttons: '"))?;
    let line = lines
        .iter()
        .rfind(|l| l.starts_with("menu bar buttons: "))
        .unwrap_or(&line);
    let points: Vec<(i32, i32)> = line
        .split("Some((")
        .skip(1)
        .filter_map(|part| {
            let (x, rest) = part.split_once(", ")?;
            let (y, _) = rest.split_once("))")?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect();
    match points[..] {
        [beize, centre] => Ok((beize, centre)),
        _ => bail!("the shell said no buttons: {line}"),
    }
}
