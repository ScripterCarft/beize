//! When a frame is drawn (ADR 0027 §6 "Zeitplan"): late enough before its
//! vblank that a client drawing after its "frame done" (sent at the vblank)
//! is on the screen at the very next one, early enough that the frame is
//! ready in time. KWin's model (`RenderLoop`, `DrmCommitThread`), each part
//! measured or taken from the mode:
//!
//! - The display takes a new frame where the vblank begins; KMS stamps the
//!   vblank where it ends, so a frame must be ready the vblank's duration
//!   earlier (from the mode: blank lines × line time; Mutter and KWin both).
//! - Ready is committed to KMS and done on the GPU (the kernel's own time
//!   of its fence): measured from when drawing was planned to begin, so a
//!   late wakeup counts too. The longest of the recent frames, at least
//!   2 ms: below that timings are unpredictable when CPU or GPU change
//!   their power state (KWin).
//! - Ready a margin before the vblank begins, for the kernel to program the
//!   display: 1 ms (KWin), plus what frames that were ready later than that
//!   lacked, learnt and slowly given back, at most 3 ms or half a refresh.
//!   Learnt only from frames planned for a vblank, against that vblank (as
//!   KWin's commit thread against its target): a change that came after
//!   the deadline, or while a frame waited for its vblank, is drawn at once
//!   and says nothing about the margin. Learning from it (0.5.99) fed
//!   itself: a late change raised the margin, which moved the deadline next
//!   to the vblank, before clients that heard "frame done" there commit,
//!   so their next change came late too – every frame a vblank behind.
//! - After an idle stretch (100 refreshes, KWin) the GPU clocks down: the
//!   next frame is drawn at once.
//! - The next vblank is one after the last known: KMS reports a vblank
//!   (and clients hear "frame done") where it begins, stamped where it
//!   ends, so until then the reported one lies ahead and is not the next
//!   (KWin counts from its last presentation). Taking it for the next
//!   (0.5.100) drew at its event, its deadline long past, at once – before
//!   the clients that heard "frame done" there committed: every frame a
//!   vblank behind.
//!
//! A change drawn after its vblank's deadline is drawn at once and may
//! still make it. Until 0.5.98 the lead was the drawing and GPU time alone
//! and a millisecond, against the vblank's end: on nouveau most frames
//! missed their vblank (`docs/hardware.md`).

use std::collections::VecDeque;
use std::time::Duration;

/// Ready this long before the vblank begins, for the kernel to program the
/// display (KWin's `s_safetyMarginMinimum`).
const MARGIN: Duration = Duration::from_millis(1);
/// The least a frame's drawing counts for (KWin's `minimumTime`).
const FLOOR: Duration = Duration::from_millis(2);
/// The most that is learnt on top of `MARGIN`; also at most half a refresh
/// (KWin's `maximumReasonableMargin`).
const MOST_LEARNT: Duration = Duration::from_millis(3);
/// What is learnt goes back by this part of the room a frame left.
const GIVE_BACK: u32 = 10;
/// Without a frame for this many refreshes, the next is drawn at once.
const IDLE_REFRESHES: u32 = 100;
/// The frames whose times set the lead: a second at 120 Hz.
const RECENT: usize = 120;
/// A vblank time older than this is asked of KMS anew before it is used:
/// the display's clock drifts from the mode's nominal refresh (50 ppm are
/// 50 µs a second).
const STALE: Duration = Duration::from_secs(1);

/// When a frame is to be drawn, the vblank (where it ends) it is planned
/// for if its deadline is not past, and the lead it was planned with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub at: Duration,
    pub aim: Option<Duration>,
    pub lead: Duration,
}

pub struct Schedule {
    refresh: Duration,
    /// How long the vblank lasts, from the mode.
    blank: Duration,
    /// A recent vblank: with `refresh`, where every vblank lies.
    phase: Option<Duration>,
    /// When the last frame was ready.
    last: Option<Duration>,
    /// From planned start to ready, of the recent frames.
    drawing: VecDeque<Duration>,
    /// Learnt on top of `MARGIN`.
    learnt: Duration,
}

impl Schedule {
    pub fn new(refresh: Duration, blank: Duration) -> Schedule {
        Schedule {
            refresh,
            blank,
            phase: None,
            last: None,
            drawing: VecDeque::with_capacity(RECENT),
            learnt: Duration::ZERO,
        }
    }

    /// A vblank ended at `time`.
    pub fn vblank(&mut self, time: Duration) {
        self.phase = Some(time);
    }

    /// The vblanks' phase is unknown or old at `now`.
    pub fn stale(&self, now: Duration) -> bool {
        self.phase
            .is_none_or(|phase| now.saturating_sub(phase) > STALE)
    }

    /// The screen was another session's: its vblanks are unknown.
    pub fn reset(&mut self) {
        self.phase = None;
        self.last = None;
    }

    /// A frame planned to begin at `planned`, for vblank `aim` if it was
    /// planned for one, was ready (committed and done on the GPU) at
    /// `ready`.
    pub fn ready(&mut self, planned: Duration, ready: Duration, aim: Option<Duration>) {
        if self.drawing.len() == RECENT {
            self.drawing.pop_front();
        }
        self.drawing.push_back(ready.saturating_sub(planned));
        self.last = Some(ready);
        let Some(aim) = aim else {
            return;
        };
        let deadline = aim.saturating_sub(self.blank + MARGIN);
        if ready > deadline {
            self.learnt += ready - deadline;
        } else {
            self.learnt = self.learnt.saturating_sub((deadline - ready) / GIVE_BACK);
        }
        self.learnt = self.learnt.min(MOST_LEARNT.min(self.refresh / 2));
    }

    /// How long before its vblank (where it ends) a frame is drawn, at
    /// `now`: at most a refresh, which is at once.
    pub fn lead(&self, now: Duration) -> Duration {
        let idle = self
            .last
            .is_none_or(|last| now.saturating_sub(last) > self.refresh * IDLE_REFRESHES);
        if idle {
            return self.refresh;
        }
        let drawing = self.drawing.iter().max().copied().unwrap_or_default();
        (drawing.max(FLOOR) + self.blank + MARGIN + self.learnt).min(self.refresh)
    }

    /// The vblank a change there at `t` is due at: the first whose
    /// deadline is not past. Without a phase, a refresh from `t`.
    pub fn target(&self, t: Duration) -> Duration {
        self.after(t + self.lead(t))
    }

    /// When to draw a change there at `now`: the deadline of the next
    /// vblank, or at once if it is past (the frame may still make it, but
    /// it is planned for none).
    pub fn draw_at(&self, now: Duration) -> Plan {
        let lead = self.lead(now);
        if self.phase.is_none() {
            return Plan {
                at: now,
                aim: None,
                lead,
            };
        }
        let next = self.after(now);
        match next.checked_sub(lead) {
            Some(at) if at >= now => Plan {
                at,
                aim: Some(next),
                lead,
            },
            _ => Plan {
                at: now,
                aim: None,
                lead,
            },
        }
    }

    /// The first vblank after `t` and after the last known (which has
    /// begun); without a phase, a refresh after `t`.
    fn after(&self, t: Duration) -> Duration {
        let Some(phase) = self.phase else {
            return t + self.refresh;
        };
        let (p, r) = (
            phase.as_nanos() as i128,
            self.refresh.as_nanos().max(1) as i128,
        );
        let t = t.max(phase).as_nanos() as i128;
        let next = p + ((t - p).div_euclid(r) + 1) * r;
        Duration::from_nanos(u64::try_from(next.max(0)).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(us: u64) -> Duration {
        Duration::from_micros(us)
    }

    /// 8 ms refresh, a 0.5 ms vblank ending at 100 ms, a frame ready 1.5 ms
    /// after it was planned: a lead of 2 (the floor) + 0.5 + 1 = 3.5 ms.
    fn schedule() -> Schedule {
        let mut s = Schedule::new(at(8000), at(500));
        s.vblank(at(100_000));
        s.ready(at(101_000), at(102_500), Some(at(108_000)));
        s
    }

    #[test]
    fn the_lead_is_the_drawing_the_vblank_and_the_margin() {
        let mut s = schedule();
        assert_eq!(s.lead(at(103_000)), at(3500));
        // A frame that took 2.5 ms: it sets the lead while it is recent.
        s.ready(at(110_000), at(112_500), Some(at(116_000)));
        assert_eq!(s.lead(at(113_000)), at(4000));
        for n in 0..RECENT as u64 {
            let t = 120_000 + n * 8000;
            s.ready(at(t), at(t + 1000), Some(at(t + 4000)));
        }
        assert_eq!(s.lead(at(120_000 + RECENT as u64 * 8000)), at(3500));
    }

    #[test]
    fn a_frame_ready_too_late_raises_the_margin_and_it_goes_back_slowly() {
        let mut s = schedule();
        // Planned for 116 ms: ready by 114.5. Ready at 115: 0.5 ms short.
        s.ready(at(112_000), at(115_000), Some(at(116_000)));
        assert_eq!(s.learnt, at(500));
        // 1.5 ms of room: a tenth of it goes back.
        s.ready(at(120_000), at(121_000), Some(at(124_000)));
        assert_eq!(s.learnt, at(350));
        // At most 3 ms, at most half a refresh.
        s.ready(at(130_000), at(140_000), Some(at(132_000)));
        assert_eq!(s.learnt, at(3000));
        let mut fast = Schedule::new(at(4000), at(500));
        fast.ready(at(0), at(10_000), Some(at(4000)));
        assert_eq!(fast.learnt, at(2000));
    }

    #[test]
    fn a_frame_drawn_at_once_teaches_nothing() {
        // 0.5.99: a change that came while a frame waited for its vblank is
        // drawn after that vblank, late for the one its change was due
        // at. It must not raise the margin: that moved the deadline next to
        // the vblank and kept every frame a vblank behind.
        let mut s = schedule();
        for n in 0..50u64 {
            let vblank = 108_000 + n * 8000;
            s.ready(at(vblank + 100), at(vblank + 400), None);
        }
        assert_eq!(s.learnt, Duration::ZERO);
        assert_eq!(s.lead(at(508_500)), at(3500));
    }

    #[test]
    fn a_change_is_drawn_at_its_vblanks_deadline() {
        let s = schedule();
        // Right after a vblank: drawn 3.5 ms before the next, for it.
        assert_eq!(s.target(at(103_000)), at(108_000));
        assert_eq!(
            s.draw_at(at(103_000)),
            Plan {
                at: at(104_500),
                aim: Some(at(108_000)),
                lead: at(3500)
            }
        );
        // Past the deadline: due at the next vblank but one, drawn at
        // once, planned for none.
        assert_eq!(s.target(at(105_000)), at(116_000));
        let plan = s.draw_at(at(105_000));
        assert_eq!((plan.at, plan.aim), (at(105_000), None));
        // Long after the known vblank, on its phase.
        assert_eq!(s.draw_at(at(503_000)).at, at(504_500));
    }

    #[test]
    fn the_vblank_just_reported_is_not_the_next() {
        // 0.5.100: the vblank ending at 108 ms is reported at 107.5, where
        // it begins; a client that heard "frame done" there commits at
        // 107.7. Its frame is for the vblank after, at that one's deadline,
        // not at once for the one already begun.
        let mut s = schedule();
        s.vblank(at(108_000));
        assert_eq!(
            s.draw_at(at(107_700)),
            Plan {
                at: at(112_500),
                aim: Some(at(116_000)),
                lead: at(3500)
            }
        );
        assert_eq!(s.target(at(107_700)), at(116_000));
    }

    #[test]
    fn after_an_idle_stretch_it_is_drawn_at_once() {
        let s = schedule();
        let later = at(102_500) + at(8000) * IDLE_REFRESHES + at(1);
        assert_eq!(s.lead(later), at(8000));
        assert_eq!(s.draw_at(later).at, later);
        // A frame never drawn yet: the same.
        let mut first = Schedule::new(at(8000), at(500));
        first.vblank(at(100_000));
        assert_eq!(first.draw_at(at(101_000)).at, at(101_000));
    }

    #[test]
    fn without_a_phase_it_is_drawn_at_once() {
        let mut s = schedule();
        s.reset();
        assert!(s.stale(at(0)));
        assert_eq!(s.draw_at(at(103_000)).at, at(103_000));
    }

    #[test]
    fn an_old_phase_is_stale() {
        let s = schedule();
        assert!(!s.stale(at(100_000) + STALE));
        assert!(s.stale(at(100_001) + STALE));
    }
}
