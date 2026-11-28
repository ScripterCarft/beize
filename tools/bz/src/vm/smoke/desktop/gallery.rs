//! The gallery (`bz smoke --only gallery`, never in the gate): every state
//! of the desktop as a picture, light and dark, into `build/gallery/` – the
//! evidence the design auditor judges each design step by, the same states
//! each time instead of screenshots taken by hand. It checks nothing beyond
//! what it needs to get there; the scenarios test.

use super::*;

/// How long a state settles before its picture: the longest spring the
/// shell's popups and dialogs open with is 0.25 s (ADR 0032 §5), drawn on
/// the VM's CPU at a few frames a second. A picture, not a check: a state
/// caught a frame early shows it, nothing fails.
const SETTLE: Duration = Duration::from_millis(1500);

pub(in crate::vm::smoke) fn gallery(paths: &Paths, installed: &Installed) -> Result<()> {
    let mut machine = installed.machine.copy_as(paths, "smoke-gallery")?;
    machine.test_medium = test_medium(paths)?;
    machine.tablet = true;
    machine.memory_mib = DESKTOP_MEMORY_MIB;
    // run0 asks for the owner's password: to unplug the screen before the
    // login and plug it in after, and to mount the test medium.
    let shell = Shell::boot(
        &machine,
        &Answers::passwords(&[SMOKE_PASSWORD; 3]),
        &OWNER_CONSOLE_CREDENTIALS,
        false,
    )?;
    let qmp = machine.qmp();
    sign_in(&shell, &machine, &qmp)?;
    let screen = screen_size(&machine)?;
    let dir = paths.root.join("build/gallery");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    let mut taken = 0;
    let mut shot = |name: &str| -> Result<()> {
        thread::sleep(SETTLE);
        self::screen(&machine, "gallery")?.write_png(&dir.join(format!("{name}.png")))?;
        taken += 1;
        Ok(())
    };

    park(&qmp, screen)?;
    shot("01-desktop-light")?;
    set_dark(&shell, &qmp, screen, true)?;
    park(&qmp, screen)?;
    shot("01-desktop-dark")?;
    set_dark(&shell, &qmp, screen, false)?;

    // Two terminals: the second in front, active; the first behind it.
    keys(&qmp, &["ctrl+alt+t"])?;
    wait_for_terminals(&shell, 1)?;
    keys(&qmp, &["ctrl+alt+t"])?;
    wait_for_terminals(&shell, 2)?;
    let front = placed(&shell, "beize-terminal", 2)?;

    for look in ["light", "dark"] {
        if look == "dark" {
            set_dark(&shell, &qmp, screen, true)?;
        }
        park(&qmp, screen)?;
        shot(&format!("02-windows-{look}"))?;
        let (beize, centre) = bar_buttons(&shell)?;

        open(&shell, &qmp, screen, beize, "BeizeMenu open")?;
        shot(&format!("03-beize-menu-{look}"))?;
        close(&shell, &qmp, "BeizeMenu closed")?;

        // The bar's menus: the Beize menu, the app's name, File, Edit, Window.
        let menus = bar_menus(&shell)?;
        if let Some(&file) = menus.get(2) {
            open(&shell, &qmp, screen, file, "AppMenu(0) open")?;
            shot(&format!("04-app-menu-{look}"))?;
            close(&shell, &qmp, "AppMenu(0) closed")?;
        }
        if let Some(&window) = menus.get(4) {
            open(&shell, &qmp, screen, window, "WindowMenu open")?;
            shot(&format!("05-window-menu-{look}"))?;
            close(&shell, &qmp, "WindowMenu closed")?;
        }

        open(&shell, &qmp, screen, centre, "ControlCenter open")?;
        shot(&format!("06-control-center-{look}"))?;
        close(&shell, &qmp, "ControlCenter closed")?;

        // The terminal's context menu, inside its text.
        let ((x, y), (w, h)) = front;
        let inside = (i32::try_from(x + w / 2)?, i32::try_from(y + h / 2)?);
        right_click(&qmp, screen, inside)?;
        shot(&format!("07-context-menu-{look}"))?;
        keys(&qmp, &["esc"])?;

        // The dock and its tile's menu.
        let apps = last_dock(&shell)?;
        let tile = dock_tile(screen, apps.len(), 0)?;
        let opened = shell_count(&shell, "DockMenu open")?;
        right_click(&qmp, screen, tile)?;
        wait_for_shell_count(&shell, "DockMenu open", opened + 1)?;
        shot(&format!("08-dock-menu-{look}"))?;
        close(&shell, &qmp, "DockMenu closed")?;

        // The confirmation: Restart… in the Beize menu.
        open(&shell, &qmp, screen, beize, "BeizeMenu open")?;
        let asked = shell_count(&shell, "asking: Power(Restart)")?;
        // As `confirmation`: the menu takes a click once it is drawn.
        thread::sleep(Duration::from_millis(300));
        click(&qmp, screen, beize_menu_row(beize, 1, false), 1)?;
        wait_for_shell_count(&shell, "asking: Power(Restart)", asked + 1)?;
        park(&qmp, screen)?;
        shot(&format!("09-confirmation-{look}"))?;
        let answered = shell_count(&shell, "asked: Power(Restart), no")?;
        keys(&qmp, &["esc"])?;
        wait_for_shell_count(&shell, "asked: Power(Restart), no", answered + 1)?;

        // Alt+Tab, held.
        hold(&qmp, "alt", true)?;
        keys(&qmp, &["tab"])?;
        shot(&format!("10-switcher-{look}"))?;
        keys(&qmp, &["esc"])?;
        hold(&qmp, "alt", false)?;

        // A window minimized: the dock's menu lists it.
        let minimize = (
            i32::try_from(x + CLOSE_X + CIRCLE_STEP)?,
            i32::try_from(y + TITLE_BAR / 2)?,
        );
        let minimized = compositor_count(&shell, "minimized beize-terminal")?;
        click(&qmp, screen, minimize, 1)?;
        wait_for_compositor_count(&shell, "minimized beize-terminal", minimized + 1)?;
        park(&qmp, screen)?;
        shot(&format!("11-minimized-{look}"))?;
        let opened = shell_count(&shell, "DockMenu open")?;
        right_click(&qmp, screen, tile)?;
        wait_for_shell_count(&shell, "DockMenu open", opened + 1)?;
        shot(&format!("12-dock-menu-minimized-{look}"))?;
        close(&shell, &qmp, "DockMenu closed")?;
        // Back by Alt+Tab: it starts on the minimized window, the only
        // one not active (a click on the tile would not, while the other
        // shows: the gap the plan's B4 closes).
        let back = compositor_count(&shell, "brought back beize-terminal")?;
        hold(&qmp, "alt", true)?;
        keys(&qmp, &["tab"])?;
        hold(&qmp, "alt", false)?;
        wait_for_compositor_count(&shell, "brought back beize-terminal", back + 1)?;
        park(&qmp, screen)?;
    }
    set_dark(&shell, &qmp, screen, false)?;
    shell.stop()?;
    remove_screens(&machine.logs)?;
    machine.remove()?;
    say!("  ok: {taken} pictures in {}", dir.display());
    Ok(())
}

/// Clicks `at` and waits until the shell says `opened` once more.
fn open(
    shell: &Shell,
    qmp: &Path,
    screen: (usize, usize),
    at: (i32, i32),
    opened: &str,
) -> Result<()> {
    let before = shell_count(shell, opened)?;
    click(qmp, screen, at, 1)?;
    wait_for_shell_count(shell, opened, before + 1)?;
    park(qmp, screen)
}

/// Escape, and waits until the shell says `closed` once more.
fn close(shell: &Shell, qmp: &Path, closed: &str) -> Result<()> {
    let before = shell_count(shell, closed)?;
    keys(qmp, &["esc"])?;
    wait_for_shell_count(shell, closed, before + 1)
}

/// The appearance: dark or light, by the control center's tile.
fn set_dark(shell: &Shell, qmp: &Path, screen: (usize, usize), dark: bool) -> Result<()> {
    let wanted = if dark {
        "appearance Dark"
    } else {
        "appearance Light"
    };
    let (_, lines) = shell.capture(&format!("{SHELL_SAID} | grep '^appearance ' | tail -n 1"))?;
    let now = lines
        .iter()
        .rfind(|l| l.starts_with("appearance "))
        .map_or("appearance Light", String::as_str);
    if now == wanted {
        return Ok(());
    }
    let (_, centre) = bar_buttons(shell)?;
    open(shell, qmp, screen, centre, "ControlCenter open")?;
    // The light/dark tile: the second row's first (as `menus`).
    let tile = appearance_tile(screen)?;
    let before = shell_count(shell, wanted)?;
    click(qmp, screen, tile, 1)?;
    wait_for_shell_count(shell, wanted, before + 1)?;
    close(shell, qmp, "ControlCenter closed")
}

/// Where the bar's menus are, as the shell said last.
fn bar_menus(shell: &Shell) -> Result<Vec<(i32, i32)>> {
    let line = wait_for_shell_line(shell, |l| l.starts_with("menu bar menus: "))?;
    let (_, lines) = shell.capture(&format!(
        "{SHELL_SAID} | grep '^menu bar menus: ' | tail -n 1"
    ))?;
    let line = lines
        .iter()
        .rfind(|l| l.starts_with("menu bar menus: "))
        .cloned()
        .unwrap_or(line);
    line.strip_prefix("menu bar menus: ")
        .context("not the bar's menus")?
        .split(' ')
        .map(|p| {
            let (x, y) = p.split_once(',').context("not a place")?;
            Ok((x.parse()?, y.parse()?))
        })
        .collect()
}
