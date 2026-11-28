//! `beize-shell` – Beize's shell (ADR 0028): the menu bar along the top
//! of the screen with the Beize menu and the control center, and the dock
//! along the bottom. A client of the compositor with Beize UI, the only one
//! it lets draw panels (wlr-layer-shell) and see the windows (beize-shell-v1,
//! ADR 0028 §1): both panels and their popups are windows of one app. Runs
//! as `beize-shell.service` in the user's desktop, restarted if it fails;
//! it ends with the session.
//!
//! The bar: macOS' menu bar in Beize's measures (`measures`) – Beize's mark
//! on the left, which opens the Beize menu, the active app's name beside it
//! in bold; on the right the network's symbol, the control center's, and
//! the date and time. As macOS' since 26 it has no background: white ink on
//! the scrim the compositor lays behind it, dark only where the background
//! stays very light, as the compositor measures it (ADR 0028 §3). With the
//! background switched on (`BarLook::Glass`, macOS' "Show menu bar
//! background") the bar lies on base glass instead, its ink for the
//! appearance. An open menu's button lies on a faint pill, as macOS marks
//! it. The dock lies on live glass, which the compositor draws below it
//! (§6).
//!
//! The Beize menu (`menu`, as macOS' Apple menu): sleep, restart, shut down
//! and log out, through logind (`beize_systemd::login`). Locking comes with
//! the lock screen (its own ADR); "About This Computer" and the settings
//! with their windows. Restart, Shut Down and Log Out ask first, as macOS'
//! with their ellipses (`beize_ui::dialog`), without its countdown: nothing
//! happens unattended. Sleep acts at once.
//!
//! The shell is polkit's agent for the user's session: an action that needs
//! the password asks in its dialog (`auth`, ADR 0031).

mod apps;
mod auth;
mod bar;
mod clock;
mod control_center;
mod dock;
mod menus;
mod network;
mod shell;
mod switcher;

use std::collections::HashMap;
use std::process::ExitCode;

use beize_systemd::login::{self, Power};
use beize_ui::dialog::{self, Dialog};
use beize_ui::draw::Cap;
use beize_ui::input::{Button, KeyState, Pointer};
use beize_ui::menu::{Entry, Menu, Outcome};
use beize_ui::{
    Align, App, Appearance, Color, DesktopPlace, Edge, Family, Glass, Input, Material, Overlay,
    Panel, Popup, Rect, Scene, Style, Text, Watch, WindowId, measures,
};
use tracing::{info, warn};

use bar::*;
use control_center::ControlCenter;
use dock::{Action, Dock};
use menus::*;
use switcher::Switcher;

/// Which of the bar's menus is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    BeizeMenu,
    ControlCenter,
    DockMenu,
    /// The active app's menu by its name, the system's for every app.
    AppName,
    /// A menu the active app describes, by its place.
    AppMenu(usize),
    /// The Window menu, the system's for every app.
    WindowMenu,
}

impl Open {
    /// Whether the bar's left side has it: moving onto another's button
    /// goes there (macOS).
    fn along_bar(self) -> bool {
        matches!(
            self,
            Open::BeizeMenu | Open::AppName | Open::AppMenu(_) | Open::WindowMenu
        )
    }
}

/// The Beize menu's commands, in its entries' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Power(Power),
    LogOut,
}

fn main() -> ExitCode {
    beize_ui::log::init("BEIZE_SHELL_LOG");
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut shell = shell::Shell::new()?;
    loop {
        shell.turn()?;
    }
}
