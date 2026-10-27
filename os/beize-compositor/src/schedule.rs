//! When a frame is drawn (ADR 0027 §6 "Zeitplan"): late enough before its
//! vblank that a client drawing after its "frame done" (sent once the frame
//! before is drawn, a refresh earlier) is on the screen at the very next
//! one, early enough that the frame is ready in time. KWin's model (`RenderLoop`, `DrmCommitThread`), each part
//! measured or taken from the mode:
//!
//! - The display takes a new frame where the vblank begins; KMS stamps the
//!   vblank where it ends, so a frame must be ready the vblank's duration
//!   earlier (from the mode: blank lines × line time; Mutter and KWin both).
//! - Ready is committed to KMS and done on the GPU (the kernel's own time
//!   of its fence): measured from when drawing was planned to begin, so a
//!   late wakeup counts too. Expected as KWin's `RenderJournal` does: the
//!   times smoothed over half a second, plus twice how far they went above
//!   that, which jumps up at once and fades over seconds; at least 2 ms:
//!   below that timings are unpredictable when CPU or GPU change their
//!   power state (KWin). Until 0.5.112 the longest of the last 120 frames:
//!   half a second at 240 Hz, after which a stall that comes every 100 ms
//!   on NVK (~2 ms before the GPU begins) was forgotten, the lead fell to
//!   3.46 ms and the next such stall made a frame late.
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
//!   where it begins, stamped where it ends, so until then the reported
//!   one lies ahead and is not the next (KWin counts from its last
//!   presentation). Taking it for the next (0.5.100, "frame done" then at
//!   the vblank) drew at its event, its deadline long past, at once –
//!   before the clients that heard "frame done" there committed: every
//!   frame a vblank behind.
//!
//! A change drawn after its vblank's deadline is drawn at once and may
//! still make it. Until 0.5.98 the lead was the drawing and GPU time alone
//! and a millisecond, against the vblank's end: on nouveau most frames
//! missed their vblank (`docs/hardware.md`).

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
/// How fast the expected drawing follows the frames' times, and how
/// slowly what went above it fades (KWin's `RenderJournal`).
const SMOOTHING: Duration = Duration::from_millis(500);
const VARIANCE_FADING: Duration = Duration::from_secs(6);
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
    /// From planned start to ready: what a frame is expected to take.
    drawing: Journal,
    /// Learnt on top of `MARGIN`.
    learnt: Duration,
}

/// What a frame's drawing is expected to take, as KWin's `RenderJournal`
/// (src/core/renderjournal.cpp): the times smoothed with a time constant
/// of `SMOOTHING`, plus twice their variance – how far a time went above
/// that, taken at once and fading with `VARIANCE_FADING`. In nanoseconds.
#[derive(Default)]
struct Journal {
    result: f64,
    variance: f64,
    /// When the last time was added.
    last: Option<Duration>,
}

impl Journal {
    /// A frame took `time`, ready `at`.
    fn add(&mut self, time: Duration, at: Duration) {
        // The first: as if the last were long ago (KWin: 10 s).
        let since = self
            .last
            .map_or(Duration::from_secs(10), |last| at.saturating_sub(last));
        self.last = Some(at);
        let (time, since) = (time.as_nanos() as f64, since.as_nanos() as f64);
        // `mix(a, b, r)` = a·r + b·(1 − r).
        let mix = |a: f64, b: f64, r: f64| a * r + b * (1.0 - r);
        let ratio = (since / VARIANCE_FADING.as_nanos() as f64).clamp(0.001, 0.1);
        let above = (time - self.result).max(0.0);
        self.variance = mix(above, self.variance, ratio).max(above);
        let ratio = (since / SMOOTHING.as_nanos() as f64).clamp(0.01, 1.0);
        self.result = mix(time, self.result, ratio);
    }

    fn expected(&self) -> Duration {
        Duration::from_nanos((self.result + 2.0 * self.variance).round() as u64)
    }
}

impl Schedule {
    pub fn new(refresh: Duration, blank: Duration) -> Schedule {
        Schedule {
            refresh,
            blank,
            phase: None,
            last: None,
            drawing: Journal::default(),
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
        self.drawing.add(ready.saturating_sub(planned), ready);
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
        let drawing = self.drawing.expected();
        (drawing.max(FLOOR) + self.blank + MARGIN + self.learnt).min(self.refresh)
    }

    /// The next vblank after `now`; without a phase, a refresh from it.
    pub fn next(&self, now: Duration) -> Duration {
        self.after(now)
    }

    /// The first vblank at or after `t` (and after the last known).
    pub fn from(&self, t: Duration) -> Duration {
        self.after(t.saturating_sub(Duration::from_nanos(1)))
    }

    /// The vblank a change there at `t` is due at: the first whose
    /// deadline is not past. Without a phase, a refresh from `t`.
    pub fn target(&self, t: Duration) -> Duration {
        self.after(t + self.lead(t))
    }

    /// When to draw a change there at `now`: the deadline of the next
    /// vblank, or at once if it is past (the frame may still make it, but
    /// it is planned for none). A frame that needs the whole refresh is
    /// drawn at once from the vblank before's event, which comes where that
    /// vblank begins – waiting for its stamp, where it ends, lost the
    /// vblank's duration of every such frame (0.43 ms at 240 Hz; 0.5.111
    /// drew 346 of its 380 late frames with a lead of a refresh).
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
                at: if lead >= self.refresh { now } else { at },
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

    /// 8 ms refresh, a 0.5 ms vblank ending at 100 ms, frames that took a
    /// steady 1.5 ms, the last ready at 102.5 ms: a lead of 2 (the floor) +
    /// 0.5 + 1 = 3.5 ms.
    fn schedule() -> Schedule {
        let mut s = Schedule::new(at(8000), at(500));
        s.vblank(at(100_000));
        s.ready(at(101_000), at(102_500), Some(at(108_000)));
        s.drawing = Journal {
            result: 1_500_000.0,
            variance: 0.0,
            last: Some(at(102_500)),
        };
        s
    }

    #[test]
    fn the_journal_follows_kwins() {
        // The first frame counts thrice (its variance is itself), as in KWin.
        let mut j = Journal::default();
        j.add(at(1000), at(0));
        assert_eq!(j.expected(), at(3000));
        // A steady time: the variance fades, the result stays.
        let mut j = Journal {
            result: 1_000_000.0,
            variance: 1_000_000.0,
            last: Some(at(0)),
        };
        j.add(at(1000), at(60_000));
        // 60 ms of 6 s: a hundredth of the variance goes.
        assert_eq!(j.expected(), at(1000) + at(1980));
    }

    #[test]
    fn a_commit_due_on_a_vblank_shows_at_that_one() {
        // wp_commit_timing (`timing`): a time on a vblank is that vblank,
        // a nanosecond after it the next one.
        let s = schedule();
        assert_eq!(s.from(at(116_000)), at(116_000));
        assert_eq!(s.from(at(116_000) + Duration::from_nanos(1)), at(124_000));
        assert_eq!(s.from(at(110_000)), at(116_000));
        assert_eq!(s.next(at(108_000)), at(116_000));
    }

    #[test]
    fn the_lead_is_the_drawing_the_vblank_and_the_margin() {
        let mut s = schedule();
        assert_eq!(s.lead(at(103_000)), at(3500));
        // A frame stalled to 3.5 ms (NVK's ~2 ms before the GPU begins):
        // 2 ms above the 1.5 expected, twice that on top.
        s.ready(at(110_000), at(113_500), Some(at(116_000)));
        let lead = s.lead(at(114_000));
        assert!(lead > at(7000) && lead < at(7100), "{lead:?}");
        // Remembered for seconds, not frames: after a second of 1 ms
        // frames (until 0.5.112 the 120 frames that set the lead), still
        // more than a millisecond above.
        let mut t = 114_000;
        for _ in 0..125 {
            t += 8000;
            s.ready(at(t), at(t + 1000), Some(at(t + 4000)));
        }
        let lead = s.lead(at(t + 1000));
        assert!(lead > at(4500), "{lead:?}");
        // Faded after half a minute.
        for _ in 0..3750 {
            t += 8000;
            s.ready(at(t), at(t + 1000), Some(at(t + 4000)));
        }
        let lead = s.lead(at(t + 1000));
        assert!(lead < at(3600), "{lead:?}");
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
    fn a_frame_that_needs_the_refresh_begins_at_the_vblank_event() {
        // A frame of 7 ms: the lead is the whole refresh. The vblank ending
        // at 108 ms is reported at 107.5, where it begins: drawn then, for
        // the vblank after, not at 108.
        let mut s = schedule();
        s.ready(at(101_000), at(108_000), Some(at(108_000)));
        assert_eq!(s.lead(at(107_500)), at(8000));
        s.vblank(at(108_000));
        assert_eq!(
            s.draw_at(at(107_520)),
            Plan {
                at: at(107_520),
                aim: Some(at(116_000)),
                lead: at(8000)
            }
        );
        // Once the vblank is over, a change is late for the next: at once,
        // planned for none, as before.
        let plan = s.draw_at(at(109_000));
        assert_eq!((plan.at, plan.aim), (at(109_000), None));
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
