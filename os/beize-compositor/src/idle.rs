//! The screen goes off when no one uses the computer: after `after`
//! without input (macOS turns a desktop's display off after 10 minutes),
//! it fades to black over `FADE`, as GNOME's shell dims before it blanks
//! (its idle lightbox, `STANDARD_FADE_TIME`), then the CRTC goes off
//! (`Gpu::sleep`), as niri powers its monitors off. Any input during the
//! fade takes it back at once, any input while off turns the screen on –
//! and reaches its window, as in Weston, Mutter and niri.
//!
//! A visible surface with an idle inhibitor (zwp_idle_inhibit_v1: a video,
//! a presentation) keeps the screen on; its time counts as use, so the
//! screen goes off `after` from when it was last seen, as Mutter resets
//! its idle time when inhibition ends. Visible is what the last frame
//! showed (the damage tracker skips what is covered), as niri decides.
//!
//! Input only notes its time: one timer, set for the moment the time runs
//! out and only then looking whether input came since (`due`) – nothing
//! is woken per input event, nothing at all while still.

use std::time::Duration;

/// macOS' "Turn display off when inactive" on a desktop. Until Beize's
/// settings hold the choice, `BEIZE_SCREEN_OFF_AFTER` (seconds, 0 for
/// never) in the desktop's environment sets it (tests).
const AFTER: Duration = Duration::from_secs(10 * 60);

/// GNOME's fade to black before the screen blanks.
pub const FADE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Awake,
    /// Fading to black, since then.
    Fading(Duration),
    Off,
}

/// What input changed.
#[derive(Debug, PartialEq)]
pub enum Woken {
    /// The screen was on and stays so.
    Nothing,
    /// The fade is taken back: drawn without it.
    Fade,
    /// The screen was off: on again.
    Screen,
}

#[derive(Debug)]
pub struct Idle {
    /// None: never off.
    after: Option<Duration>,
    /// The last use: input, or an inhibitor seen.
    used: Duration,
    phase: Phase,
}

impl Idle {
    /// From the desktop's environment (`AFTER`), counting from `now`.
    pub fn from_environment(now: Duration) -> Idle {
        let after = match std::env::var("BEIZE_SCREEN_OFF_AFTER") {
            Ok(seconds) => match seconds.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(seconds) => Some(Duration::from_secs(seconds)),
                Err(_) => Some(AFTER),
            },
            Err(_) => Some(AFTER),
        };
        Idle::new(after, now)
    }

    fn new(after: Option<Duration>, now: Duration) -> Idle {
        Idle {
            after,
            used: now,
            phase: Phase::Awake,
        }
    }

    pub fn after(&self) -> Option<Duration> {
        self.after
    }

    /// Input at `now`.
    pub fn input(&mut self, now: Duration) -> Woken {
        self.used = now;
        match std::mem::replace(&mut self.phase, Phase::Awake) {
            Phase::Awake => Woken::Nothing,
            Phase::Fading(_) => Woken::Fade,
            Phase::Off => Woken::Screen,
        }
    }

    /// An inhibitor was seen at `now`: as input, but it does not turn the
    /// screen on (the protocol only keeps it from going off).
    pub fn inhibited(&mut self, now: Duration) -> bool {
        match self.phase {
            Phase::Awake => {
                self.used = now;
                false
            }
            Phase::Fading(_) => {
                self.used = now;
                self.phase = Phase::Awake;
                true
            }
            Phase::Off => false,
        }
    }

    /// When the screen fades, if still unused then; None while it fades,
    /// is off or never goes off.
    pub fn due(&self) -> Option<Duration> {
        match self.phase {
            Phase::Awake => self.after.map(|after| self.used + after),
            _ => None,
        }
    }

    /// The timer ran out at `now`: fades, if `due` has come (true).
    pub fn fade(&mut self, now: Duration) -> bool {
        if self.due().is_some_and(|due| due <= now) {
            self.phase = Phase::Fading(now);
            return true;
        }
        false
    }

    /// The black over the screen at `now`, while fading: linear, as the
    /// lightbox's opacity; 1 once done.
    pub fn black(&self, now: Duration) -> Option<f32> {
        let Phase::Fading(since) = self.phase else {
            return None;
        };
        let t = now.saturating_sub(since).as_secs_f64() / FADE.as_secs_f64();
        Some(t.min(1.0) as f32)
    }

    /// The fade is all black at `now`: time for the screen to go off.
    pub fn faded(&self, now: Duration) -> bool {
        self.black(now) == Some(1.0)
    }

    /// The screen is off: at the fade's end, or at once without a screen
    /// to fade.
    pub fn sleep(&mut self) {
        self.phase = Phase::Off;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn fades_after_the_time_without_input_then_goes_off() {
        let mut idle = Idle::new(Some(s(600)), s(0));
        assert_eq!(idle.due(), Some(s(600)));
        // Input meanwhile moves the time on; the timer, run out, waits on.
        assert_eq!(idle.input(s(100)), Woken::Nothing);
        assert!(!idle.fade(s(600)));
        assert_eq!(idle.due(), Some(s(700)));
        assert!(idle.fade(s(700)));
        assert_eq!(idle.due(), None);
        assert_eq!(idle.black(s(700)), Some(0.0));
        assert_eq!(idle.black(s(705)), Some(0.5));
        assert!(!idle.faded(s(709)));
        assert!(idle.faded(s(710)));
        idle.sleep();
        assert_eq!(idle.phase, Phase::Off);
        assert_eq!(idle.black(s(711)), None);
    }

    #[test]
    fn input_takes_the_fade_back_and_turns_the_screen_on() {
        let mut idle = Idle::new(Some(s(600)), s(0));
        assert!(idle.fade(s(600)));
        assert_eq!(idle.input(s(603)), Woken::Fade);
        assert_eq!(idle.black(s(603)), None);
        assert_eq!(idle.due(), Some(s(1203)));
        assert!(idle.fade(s(1203)));
        assert!(idle.faded(s(1213)));
        idle.sleep();
        assert_eq!(idle.input(s(2000)), Woken::Screen);
        assert_ne!(idle.phase, Phase::Off);
        assert_eq!(idle.due(), Some(s(2600)));
    }

    #[test]
    fn an_inhibitor_counts_as_use_but_wakes_no_screen() {
        let mut idle = Idle::new(Some(s(600)), s(0));
        assert!(!idle.inhibited(s(500)));
        assert_eq!(idle.due(), Some(s(1100)));
        assert!(idle.fade(s(1100)));
        // Seen while fading: the fade is taken back.
        assert!(idle.inhibited(s(1101)));
        assert_eq!(idle.black(s(1101)), None);
        assert!(idle.fade(s(1701)));
        assert!(idle.faded(s(1711)));
        idle.sleep();
        assert!(!idle.inhibited(s(1800)));
        assert_eq!(idle.phase, Phase::Off);
    }

    #[test]
    fn never_without_a_time() {
        let mut idle = Idle::new(None, s(0));
        assert_eq!(idle.due(), None);
        assert!(!idle.fade(s(100_000)));
    }
}
