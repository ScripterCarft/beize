//! What only the shell asks of the compositor (`beize_shell_v1`, ADR
//! 0028 §1): the desktop's windows and what to do with them, Alt+Tab,
//! the appearance, the dock's icons, marks for animations. Other apps
//! never see that global.

use super::*;

impl App {
    /// Hears of the desktop's windows from now on: only the shell may
    /// (beize-shell-v1; the compositor offers it to no other client).
    pub fn watch_desktop(&mut self) -> Result<()> {
        if self.state.desktop.is_none() {
            let shell = self
                .globals
                .bind(&self.queue.handle(), 1..=1, ())
                .context("the compositor offers this program no list of windows")?;
            self.state.desktop = Some(Desktop::new(shell));
        }
        Ok(())
    }

    /// The desktop's windows, in the order they opened (`watch_desktop`).
    pub fn desktop_windows(&self) -> Vec<DesktopWindow> {
        self.state
            .desktop
            .as_ref()
            .map(|d| d.windows().cloned().collect())
            .unwrap_or_default()
    }

    /// How light it is behind the menu bar (`watch_desktop`): light wants
    /// dark ink. The appearance's until the compositor has measured it.
    /// Whether the Beize menu's key was pressed since the last asked.
    pub fn take_beize_menu_key(&mut self) -> bool {
        self.state
            .desktop
            .as_mut()
            .is_some_and(|d| std::mem::take(&mut d.beize_menu_key))
    }

    pub fn menu_bar_backdrop(&self) -> Appearance {
        self.state
            .desktop
            .as_ref()
            .and_then(|d| d.menu_bar_backdrop)
            .unwrap_or_else(|| self.appearance())
    }

    /// Brings a window of the desktop to the front, back on the screen if
    /// it was minimized.
    pub fn activate_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.activate(id);
        }
    }

    /// The switcher (Alt+Tab) while the compositor shows it (`watch_desktop`).
    pub fn desktop_switcher(&self) -> Option<SwitcherNews> {
        self.state.desktop.as_ref()?.switcher.clone()
    }

    /// The pointer moved onto a window's tile in the switcher.
    pub fn switcher_hover(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.switcher_hover(id);
        }
    }

    /// A click on a window's tile in the switcher: it is chosen.
    pub fn switcher_choose(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.switcher_choose(id);
        }
    }

    /// The compositor draws these windows' live pictures on `id`, each in its
    /// rectangle; applies with its next draw.
    /// The shell's window `id` is a dialog: the compositor shows it
    /// appearing and going (ADR 0032). Before its first content.
    pub fn mark_dialog(&mut self, id: WindowId) {
        if let (Some(wl), Some(desktop)) = (
            self.state.surface(id).map(|s| s.wl.clone()),
            &self.state.desktop,
        ) {
            desktop.dialog(&wl);
        }
    }

    /// The shell's popup `id` grows out of the rectangle it is anchored
    /// to and goes back into it (the control center, ADR 0032). Before its
    /// first content.
    pub fn mark_grows(&mut self, id: WindowId) {
        if let (Some(wl), Some(desktop)) = (
            self.state.surface(id).map(|s| s.wl.clone()),
            &self.state.desktop,
        ) {
            desktop.grows(&wl);
        }
    }

    /// The dialog `id` shakes: a wrong answer.
    pub fn shake(&mut self, id: WindowId) {
        if let (Some(wl), Some(desktop)) = (
            self.state.surface(id).map(|s| s.wl.clone()),
            &self.state.desktop,
        ) {
            desktop.shake(&wl);
        }
    }

    /// Where the apps' symbols lie in the window `id` (the dock), by app
    /// id; applies with its next draw.
    pub fn show_dock_icons(&mut self, id: WindowId, icons: &[(String, beize_draw::Rect)]) {
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(desktop) = &self.state.desktop {
            desktop.show_dock_icons(&wl, icons);
        }
    }

    pub fn show_thumbnails(
        &mut self,
        id: WindowId,
        pictures: &[(DesktopWindowId, beize_draw::Rect)],
    ) {
        let Some(wl) = self.state.surface(id).map(|s| s.wl.clone()) else {
            return;
        };
        if let Some(desktop) = &self.state.desktop {
            desktop.show_thumbnails(&wl, pictures);
        }
    }

    /// The user chose command `command` of a window's menus (the shell).
    pub fn activate_menu(&mut self, id: DesktopWindowId, command: u32) {
        if let Some(desktop) = &self.state.desktop {
            desktop.menu_activate(id, command);
        }
    }

    /// Takes a window of the desktop off the screen.
    pub fn minimize_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.minimize(id);
        }
    }

    /// Maximizes a window of the desktop, or brings it back.
    pub fn zoom_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.zoom(id);
        }
    }

    /// Moves a window of the desktop to `place`.
    pub fn move_desktop_window(&mut self, id: DesktopWindowId, place: DesktopPlace) {
        if let Some(desktop) = &self.state.desktop {
            desktop.move_to(id, place);
        }
    }

    /// Asks a window of the desktop to close, as its own close button does.
    pub fn close_desktop_window(&mut self, id: DesktopWindowId) {
        if let Some(desktop) = &self.state.desktop {
            desktop.close(id);
        }
    }

    /// Switches the system's appearance (the control center; only the
    /// shell, `watch_desktop`): the compositor keeps it and tells the shell
    /// back (`appearance`).
    pub fn choose_appearance(&mut self, appearance: Appearance) {
        if let Some(desktop) = &self.state.desktop {
            desktop.choose_appearance(appearance);
        }
    }

    /// Tells the compositor the shell's panels are drawn, so the desktop
    /// appears in one piece (ADR 0028 §6): once, after the first draw of
    /// each (`watch_desktop`).
    pub fn desktop_ready(&mut self) {
        if let Some(desktop) = &self.state.desktop {
            desktop.ready();
        }
    }
}
