//! When a frame is drawn (ADR 0027 §6 "Zeitplan"): as late before its
//! vblank as the compositor still makes it. A client that draws after its
//! "frame done" (sent at the vblank) is then on the screen at the very next
//! vblank, together with the pointer and every other change of that
//! refresh. Weston calls it the repaint window; KWin and Mutter learn its
//! length from recent frames, as here.
//!
//! The lead before the vblank is the longest drawing (CPU) and the longest
//! GPU time of the last frames, plus a margin for the commit and the
//! kernel. A change after a vblank's deadline goes to the next one.
//!
//! Drawing at the vblank instead (until 0.5.97) left every frame a refresh
//! behind once one change met a commit still waiting: 8.65 ms from change
//! to screen at 240 Hz instead of at most one refresh (`docs/hardware.md`).

use std::collections::VecDeque;
use std::time::Duration;

/// For the commit to reach the display engine and the kernel to program
/// it, beyond the measured drawing and GPU time.
const MARGIN: Duration = Duration::from_millis(1);
/// The frames whose times set the lead: a second at 120 Hz.
const RECENT: usize = 120;
/// A vblank time older than this is asked of KMS anew before it is used:
/// the display's clock drifts from the mode's nominal refresh (50 ppm are
/// 50 µs a second).
const STALE: Duration = Duration::from_secs(1);

pub struct Schedule {
    refresh: Duration,
    /// A recent vblank: with `refresh`, where every vblank lies.
    phase: Option<Duration>,
    /// Drawing (CPU) and GPU time of the recent frames.
    cpu: VecDeque<Duration>,
    gpu: VecDeque<Duration>,
}

impl Schedule {
    pub fn new(refresh: Duration) -> Schedule {
        Schedule {
            refresh,
            phase: None,
            cpu: VecDeque::with_capacity(RECENT),
            gpu: VecDeque::with_capacity(RECENT),
        }
    }

    /// A vblank passed at `time`.
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
    }

    /// A frame took `cpu` from drawing to its commit.
    pub fn drawn(&mut self, cpu: Duration) {
        push(&mut self.cpu, cpu);
    }

    /// A frame's GPU time was read.
    pub fn gpu(&mut self, gpu: Duration) {
        push(&mut self.gpu, gpu);
    }

    /// How long before its vblank a frame is drawn: at most a refresh.
    pub fn lead(&self) -> Duration {
        let longest = |times: &VecDeque<Duration>| times.iter().max().copied();
        let cost = longest(&self.cpu).unwrap_or_default()
            + longest(&self.gpu).unwrap_or_default()
            + MARGIN;
        cost.min(self.refresh)
    }

    /// The vblank a change there at `t` is for: the first whose deadline
    /// is not past. Without a phase, a refresh from `t`.
    pub fn target(&self, t: Duration) -> Duration {
        let Some(phase) = self.phase else {
            return t + self.refresh;
        };
        let (p, r) = (
            phase.as_nanos() as i128,
            self.refresh.as_nanos().max(1) as i128,
        );
        // The first vblank after `t + lead`: its deadline lies after `t`.
        let t = (t + self.lead()).as_nanos() as i128;
        let next = p + ((t - p).div_euclid(r) + 1) * r;
        Duration::from_nanos(u64::try_from(next.max(0)).unwrap_or(u64::MAX))
    }

    /// When to draw a change there at `now`: the deadline of its vblank,
    /// at once without a phase.
    pub fn draw_at(&self, now: Duration) -> Duration {
        if self.phase.is_none() {
            return now;
        }
        self.target(now).saturating_sub(self.lead()).max(now)
    }
}

fn push(times: &mut VecDeque<Duration>, time: Duration) {
    if times.len() == RECENT {
        times.pop_front();
    }
    times.push_back(time);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(us: u64) -> Duration {
        Duration::from_micros(us)
    }

    /// 4 ms refresh, a vblank at 100 ms, a frame of 0.5 ms CPU and 0.5 ms
    /// GPU: a lead of 2 ms.
    fn schedule() -> Schedule {
        let mut s = Schedule::new(Duration::from_millis(4));
        s.vblank(at(100_000));
        s.drawn(at(500));
        s.gpu(at(500));
        s
    }

    #[test]
    fn the_lead_is_the_longest_frame_and_the_margin() {
        let mut s = schedule();
        assert_eq!(s.lead(), at(2000));
        s.drawn(at(300));
        s.drawn(at(900));
        s.gpu(at(200));
        assert_eq!(s.lead(), at(2400));
        // The slow frames age out.
        for _ in 0..RECENT {
            s.drawn(at(300));
            s.gpu(at(200));
        }
        assert_eq!(s.lead(), at(1500));
    }

    #[test]
    fn the_lead_is_at_most_a_refresh() {
        let mut s = schedule();
        s.drawn(at(9000));
        assert_eq!(s.lead(), at(4000));
    }

    #[test]
    fn a_change_is_drawn_at_its_vblanks_deadline() {
        let s = schedule();
        // Right after a vblank: drawn 2 ms before the next.
        assert_eq!(s.target(at(100_100)), at(104_000));
        assert_eq!(s.draw_at(at(100_100)), at(102_000));
        // Just before the deadline: still that vblank.
        assert_eq!(s.draw_at(at(101_900)), at(102_000));
        // Past it: the next one.
        assert_eq!(s.target(at(102_100)), at(108_000));
        assert_eq!(s.draw_at(at(102_100)), at(106_000));
        // Long after the known vblank, on its phase.
        assert_eq!(s.draw_at(at(501_000)), at(502_000));
    }

    #[test]
    fn without_a_phase_it_is_drawn_at_once() {
        let mut s = schedule();
        s.reset();
        assert!(s.stale(at(0)));
        assert_eq!(s.draw_at(at(100_100)), at(100_100));
        assert_eq!(s.target(at(100_100)), at(104_100));
    }

    #[test]
    fn an_old_phase_is_stale() {
        let s = schedule();
        assert!(!s.stale(at(100_000) + STALE));
        assert!(s.stale(at(100_001) + STALE));
    }
}
