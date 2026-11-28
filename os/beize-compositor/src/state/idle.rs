//! Idle: fading the screen out, turning it off, and the inhibitors that
//! keep it on.

use super::*;

impl State {
    /// While fading, draws when the black next gets darker
    /// (`Idle::next_step`); a fade taken back meanwhile draws nothing.
    pub(super) fn fade_step(&mut self) {
        if self.fade_timer.is_some() {
            return;
        }
        let now = self.now();
        let Some(at) = self.idle.next_step(now) else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(at.saturating_sub(now)),
            |_, _, state| {
                state.fade_timer = None;
                if state.idle.black(state.now()).is_some() {
                    state.changed();
                }
                TimeoutAction::Drop
            },
        );
        match token {
            Ok(token) => self.fade_timer = Some(token),
            Err(e) => warn!("fade timer: {}", e.error),
        }
    }

    /// Input came (`input`), or the session came back: the time without
    /// input starts anew, a fade is taken back, a screen gone off comes on
    /// – drawn in full, which sets its mode.
    pub fn input_came(&mut self) {
        let now = self.now();
        match self.idle.input(now) {
            Woken::Nothing => {}
            Woken::Fade => {
                info!("input: the fade to black is taken back");
                self.changed();
            }
            Woken::Screen => {
                info!("input: the screen is on again");
                if let Some(gpu) = &mut self.gpu {
                    gpu.wake();
                }
                self.changed();
            }
        }
        self.set_idle_timer();
    }

    /// Sets the timer for when the screen fades without input, unless one
    /// is set: it runs out at the time the last use allows, looks whether
    /// there was use since and, if so, runs on for the rest – one wake per
    /// such time, none per input event.
    pub(super) fn set_idle_timer(&mut self) {
        if self.idle_timer.is_some() {
            return;
        }
        let Some(due) = self.idle.due() else {
            return;
        };
        let token = self.handle.insert_source(
            Timer::from_duration(due.saturating_sub(self.now())),
            |_, _, state| {
                let now = state.now();
                // Another session's time is not ours to count.
                if !state.session.is_active() {
                    state.idle.inhibited(now);
                }
                state.look_at_inhibitors();
                if state.idle.fade(now) {
                    state.idle_timer = None;
                    state.fade_out();
                    return TimeoutAction::Drop;
                }
                match state.idle.due() {
                    Some(due) => TimeoutAction::ToDuration(due.saturating_sub(now)),
                    None => {
                        state.idle_timer = None;
                        TimeoutAction::Drop
                    }
                }
            },
        );
        match token {
            Ok(token) => self.idle_timer = Some(token),
            Err(e) => warn!("idle timer: {}", e.error),
        }
    }

    /// No input for the time: the screen fades to black, then goes off
    /// (`frame_done`); without a screen to fade, off at once.
    pub(super) fn fade_out(&mut self) {
        let seconds = self.idle.after().unwrap_or_default().as_secs();
        if self.gpu.as_ref().is_some_and(Gpu::shows) {
            info!("no input for {seconds} s: the screen fades to black");
            self.fade_step();
        } else {
            info!("no input for {seconds} s: no screen to fade, off");
            self.screen_off();
        }
    }

    /// The fade is done: the screen goes off. A frame still waiting for its
    /// vblank defers it to that vblank's `frame_done`.
    pub(super) fn screen_off(&mut self) {
        let slept = match &mut self.gpu {
            Some(gpu) => gpu.sleep(),
            None => Ok(true),
        };
        match slept {
            Ok(false) => return,
            Ok(true) => info!("the screen is off"),
            // Not drawn on either way until input comes.
            Err(e) => warn!("{e:#}; nothing is drawn until input comes"),
        }
        self.idle.sleep();
    }

    /// The idle inhibitors (`idle`): those of surfaces gone are dropped; one
    /// on a surface the last frame showed – until now, also if it went
    /// since – counts as use, and takes a fade back.
    pub(super) fn look_at_inhibitors(&mut self) {
        if self.inhibitors.is_empty() {
            return;
        }
        let drawn = &self.drawn;
        let mut seen = false;
        self.inhibitors.retain(|surface| {
            seen |= drawn.element_was_presented(surface);
            surface.is_alive()
        });
        let now = self.now();
        if seen && self.idle.inhibited(now) {
            info!("an idle inhibitor shows: the fade to black is taken back");
            self.changed();
            self.set_idle_timer();
        }
    }
}
