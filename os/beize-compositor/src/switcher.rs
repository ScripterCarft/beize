//! Alt+Tab (ADR 0028 §7): the compositor keeps the keys – the release of Alt
//! ends it, which a client's keyboard could miss, as KWin and GNOME keep them
//! – and the shell shows the windows (`shell_v1`), drawing an overlay with
//! their tiles; the compositor draws each window's live picture into it
//! (`Thumbnails`, as DwmRegisterThumbnail gives a window another's).
//!
//! The order is the windows' from front to back, then the minimized ones, the
//! last minimized first. It starts on the first one not active. Nothing is
//! raised until the choice. A quick Alt+Tab shows nothing: the shell hears of
//! it after `SHOW_AFTER`. Without a shell the keys still switch.
//!
//! Only keys that are no modifiers are kept from the window with the
//! keyboard: a modifier's press or release stays whole for it, so none stays
//! held when the keyboard moves on.

use std::time::Duration;

use smithay::desktop::{PopupUngrabStrategy, Window};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::{Cacheable, with_states};
use tracing::{info, warn};

use crate::state::State;

/// gnome-shell's POPUP_DELAY_TIMEOUT (altTab.js).
const SHOW_AFTER: Duration = Duration::from_millis(150);

pub struct Switcher {
    pub entries: Vec<Window>,
    pub selected: usize,
    /// The shell was told.
    pub shown: bool,
    timer: Option<RegistrationToken>,
}

/// What a key does while the switcher is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitcherKey {
    Step(isize),
    Choose,
    Cancel,
}

/// The windows' pictures the shell wants on a surface of its own: which by
/// their number (`shell_v1`) and where. Double-buffered, applied with the
/// surface's commit, so tile and picture show together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thumbnails(pub Vec<(u64, Rectangle<i32, Logical>)>);

impl Cacheable for Thumbnails {
    fn commit(&mut self, _dh: &DisplayHandle) -> Thumbnails {
        self.clone()
    }

    fn merge_into(self, into: &mut Thumbnails, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Changes the pictures `surface` gets with its next commit.
pub fn set_pending(surface: &WlSurface, change: impl FnOnce(&mut Thumbnails)) {
    if surface.is_alive() {
        with_states(surface, |states| {
            change(states.cached_state.get::<Thumbnails>().pending());
        });
    }
}

/// The pictures `surface` shows.
pub fn current(surface: &WlSurface) -> Vec<(u64, Rectangle<i32, Logical>)> {
    with_states(surface, |states| {
        states.cached_state.get::<Thumbnails>().current().0.clone()
    })
}

/// `selected` moved by `by`, round.
fn stepped(selected: usize, by: isize, count: usize) -> usize {
    (selected as isize + by).rem_euclid(count.max(1) as isize) as usize
}

impl State {
    /// Alt+Tab, or Alt+Shift+Tab (`by` -1).
    pub fn switcher_start(&mut self, by: isize) {
        let entries: Vec<Window> = self
            .space
            .elements()
            .rev()
            .chain(self.minimized.iter().rev())
            .cloned()
            .collect();
        let active = self.active_window();
        let first = entries
            .iter()
            .position(|w| Some(w) != active.as_ref())
            .filter(|_| !entries.is_empty());
        let Some(first) = first else {
            return;
        };
        // A menu open keeps the keyboard from what comes to the front.
        if let Some(mut grab) = self.popup_grab.take() {
            grab.ungrab(PopupUngrabStrategy::All);
            self.let_go_of(&grab);
            self.popup_grab = Some(grab);
        }
        let selected = if by < 0 {
            stepped(0, by, entries.len())
        } else {
            first
        };
        let timer = self
            .handle
            .insert_source(Timer::from_duration(SHOW_AFTER), |_, _, state| {
                state.switcher_show();
                TimeoutAction::Drop
            });
        let timer = match timer {
            Ok(timer) => Some(timer),
            Err(e) => {
                warn!("the switcher's timer: {}", e.error);
                None
            }
        };
        self.switcher = Some(Switcher {
            entries,
            selected,
            shown: false,
            timer,
        });
    }

    /// The shell hears of it now, unless it is over already.
    fn switcher_show(&mut self) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        switcher.timer = None;
        switcher.shown = true;
        let (entries, selected) = (switcher.entries.clone(), switcher.selected);
        info!("switcher shown");
        self.tell_switcher_show(&entries, &entries[selected]);
    }

    /// A key while it is open.
    pub fn switcher_key(&mut self, key: SwitcherKey) {
        match key {
            SwitcherKey::Step(by) => {
                let Some(switcher) = &self.switcher else {
                    return;
                };
                let to = stepped(switcher.selected, by, switcher.entries.len());
                self.switcher_select(to);
            }
            SwitcherKey::Choose => self.switcher_choose(),
            SwitcherKey::Cancel => self.switcher_end(),
        }
    }

    fn switcher_select(&mut self, to: usize) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        if switcher.selected == to {
            return;
        }
        switcher.selected = to;
        if switcher.shown {
            let window = switcher.entries[to].clone();
            info!("switcher selected {to}");
            self.tell_switcher_select(&window);
        } else {
            info!("switcher selected {to}");
        }
    }

    /// The pointer over a window's tile (`shell_v1`).
    pub fn switcher_hover(&mut self, window: &Window) {
        let to = self
            .switcher
            .as_ref()
            .and_then(|s| s.entries.iter().position(|w| w == window));
        if let Some(to) = to {
            self.switcher_select(to);
        }
    }

    /// A click on a window's tile (`shell_v1`).
    pub fn switcher_click(&mut self, window: &Window) {
        self.switcher_hover(window);
        self.switcher_choose();
    }

    /// The selected window comes to the front (Alt let go, Return, a click).
    pub fn switcher_choose(&mut self) {
        let chosen = self
            .switcher
            .as_ref()
            .and_then(|s| s.entries.get(s.selected).cloned());
        self.switcher_end();
        if let Some(window) = chosen {
            info!("switcher chose {}", crate::windows::app_id(&window));
            self.bring_back(window);
        }
    }

    /// It is over, with no window chosen unless `switcher_choose` says.
    pub fn switcher_end(&mut self) {
        let Some(switcher) = self.switcher.take() else {
            return;
        };
        if let Some(timer) = switcher.timer {
            self.handle.remove(timer);
        }
        if switcher.shown {
            info!("switcher hidden");
            self.tell_switcher_end();
        }
    }

    /// `window` is gone: off the list; the shell hears that from the window.
    pub fn switcher_window_gone(&mut self, window: &Window) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        let Some(i) = switcher.entries.iter().position(|w| w == window) else {
            return;
        };
        switcher.entries.remove(i);
        if switcher.entries.is_empty() {
            self.switcher_end();
            return;
        }
        let selected = match switcher.selected {
            s if s > i => s - 1,
            s if s >= switcher.entries.len() => 0,
            s => s,
        };
        let moved = selected != switcher.selected || i == switcher.selected;
        switcher.selected = selected;
        if moved && switcher.shown {
            let window = switcher.entries[selected].clone();
            self.tell_switcher_select(&window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_round() {
        assert_eq!(stepped(0, 1, 3), 1);
        assert_eq!(stepped(2, 1, 3), 0);
        assert_eq!(stepped(0, -1, 3), 2);
    }
}
