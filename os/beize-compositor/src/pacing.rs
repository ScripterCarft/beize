//! How frames meet their vblank, measured by the compositor itself (ADR
//! 0027 §6 "Bildtakt messen"). The compositor's task is to show a change at
//! the first vblank whose deadline it met (`schedule`). Each drawn frame
//! keeps when its change arrived, that vblank, when drawing began and when
//! it went to KMS; the vblank that shows it says whether it made it.
//!
//! A frame shown later than that vblank is late, and its times say who
//! held it up, measured against where the vblank begins (the display takes
//! a frame there; KMS stamps the vblank's end): committed after it
//! (drawing, the compositor's CPU work or a late wakeup), done on the GPU
//! after it (the kernel's time of the frame's fence), or ready before it
//! and still not shown (display: the kernel and the driver). How often
//! clients draw is theirs; a client that is late shows in its own frame
//! times, not here.
//!
//! Frames in a row (an animation, a client drawing each frame) make one
//! summary in the journal, when they end or every 30 s, at info level from
//! a second on:
//!
//! `pacing: 4800 frames in 20.00 s at 240 Hz, 0 late (…); change to screen
//! median 3.71 ms, …; ready median 1.21 ms before the vblank begins, …;
//! busy 4 % of the 4.17 ms per frame`
//!
//! The first late frames of a run get a line each with their times, so a
//! hardware test shows who held up each one (a summary only says how many).
//! Both say where the drawing (CPU) went (`Parts`): measured, so the work
//! that makes frames late is found before anything is changed.
//!
//! It costs a few numbers per frame, kept until the summary.
//!
//! The vblank times must be real: a display engine's (nouveau, i915,
//! amdgpu). virtio-gpu has no vblank, its "vblank" is when the host took
//! the frame, so in a VM the late frames and the change-to-screen times say
//! little; the frame and drawing times still hold.

use std::time::Duration;

use tracing::{debug, info};

/// A frame longer than this after the last one begins a new run: the
/// screen was idle in between.
const IDLE: Duration = Duration::from_millis(250);
/// A long run is summed up in pieces of this length.
const PIECE: Duration = Duration::from_secs(30);
/// Runs from this long on are summed up at info level, shorter ones (a key,
/// a click) at debug level.
const NOTABLE: Duration = Duration::from_secs(1);
/// Late frames logged one by one with their times, per run: enough to see
/// who held them up, without a line per frame when every frame is late.
const LOGGED_LATE: u32 = 10;

/// When a frame's change arrived, the vblank it is due at (where it ends),
/// when its drawing was planned to begin and began, and when it was
/// committed; on CLOCK_MONOTONIC, as KMS's vblank times. And the schedule's
/// lead it was planned with, if it was planned, and whether that plan was
/// for a vblank (or drew it at once).
#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub requested: Duration,
    pub due: Duration,
    pub planned: Duration,
    pub began: Duration,
    pub committed: Duration,
    pub lead: Option<Duration>,
    pub aimed: bool,
    pub parts: Parts,
}

/// Where a frame's drawing (CPU) went, from when it began to its commit:
/// the scene put together (every window's, panel's and shadow's
/// elements), live glass drawn behind its panels, the frame itself
/// (damage, recording, the GPU's queue, the buffers' fences) and the
/// commit to KMS.
#[derive(Clone, Copy, Debug, Default)]
pub struct Parts {
    pub scene: Duration,
    pub glass: Duration,
    pub frame: Duration,
    pub commit: Duration,
}

/// The frames of one run.
#[derive(Default)]
struct Run {
    /// The vblank of its first frame.
    first: Duration,
    frames: u32,
    /// Shown after their vblank: committed after it began (drawing), done
    /// on the GPU after it (GPU), or ready before and still late (display).
    late_drawing: u32,
    late_gpu: u32,
    late_display: u32,
    /// Late frames logged one by one so far.
    logged: u32,
    /// Change to screen, drawing (CPU) and GPU time, and how long before
    /// its vblank began the frame was ready (negative: after), in
    /// milliseconds.
    latency: Vec<f64>,
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    ahead: Vec<f64>,
    /// The schedule's lead, in milliseconds.
    lead: Vec<f64>,
    /// The drawing's parts (`Parts`), in milliseconds.
    parts: [Vec<f64>; 4],
}

pub struct Pacing {
    refresh: Duration,
    /// How long the vblank lasts.
    blank: Duration,
    /// The vblank of the last frame shown.
    last: Option<Duration>,
    run: Run,
}

impl Pacing {
    pub fn new(refresh: Duration, blank: Duration) -> Pacing {
        Pacing {
            refresh,
            blank,
            last: None,
            run: Run::default(),
        }
    }

    /// A drawn frame is on the screen since `vblank`; `gpu_ms` how long
    /// the GPU worked on it, if measured, `gpu_done` when its fence
    /// signalled if it had one.
    pub fn shown(
        &mut self,
        times: Times,
        vblank: Duration,
        gpu_ms: Option<f64>,
        gpu_done: Option<Duration>,
    ) {
        if self
            .last
            .is_none_or(|last| vblank.saturating_sub(last) > IDLE)
        {
            self.report();
            self.run.first = vblank;
        }
        self.last = Some(vblank);
        let run = &mut self.run;
        run.frames += 1;
        let due = times.due;
        let begins = due.saturating_sub(self.blank);
        let ready = gpu_done.map_or(times.committed, |g| g.max(times.committed));
        // Half a refresh of tolerance: vblank times jitter by microseconds.
        if vblank > due + self.refresh / 2 {
            let who = if times.committed > begins {
                self.run.late_drawing += 1;
                "drawing"
            } else if ready > begins {
                self.run.late_gpu += 1;
                "GPU"
            } else {
                self.run.late_display += 1;
                "display"
            };
            if self.run.logged < LOGGED_LATE {
                self.run.logged += 1;
                let shown = vblank.saturating_sub(self.blank);
                info!("{}", late(who, &times, begins, ready, shown, gpu_ms));
            }
        }
        let run = &mut self.run;
        run.latency.push(ms(vblank.saturating_sub(times.requested)));
        run.cpu
            .push(ms(times.committed.saturating_sub(times.began)));
        run.gpu.extend(gpu_ms);
        run.ahead.push(ms(begins) - ms(ready));
        run.lead.extend(times.lead.map(ms));
        let p = times.parts;
        for (all, part) in run
            .parts
            .iter_mut()
            .zip([p.scene, p.glass, p.frame, p.commit])
        {
            all.push(ms(part));
        }
        if vblank.saturating_sub(run.first) >= PIECE {
            self.report();
            self.run.first = vblank;
        }
    }

    /// Ends the run once no frame came for a while; false when there is no
    /// run (left) to watch.
    pub fn idle(&mut self, now: Duration) -> bool {
        match self.last {
            Some(last) if now.saturating_sub(last) > IDLE => {
                self.report();
                self.last = None;
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// The session paused: the run ends here.
    pub fn pause(&mut self) {
        self.report();
        self.last = None;
    }

    /// Logs the run's summary and starts a new one.
    fn report(&mut self) {
        let run = std::mem::take(&mut self.run);
        let Some(line) = self.summary(&run) else {
            return;
        };
        let length = self
            .last
            .map_or(Duration::ZERO, |l| l.saturating_sub(run.first));
        if length >= NOTABLE {
            info!("{line}");
        } else {
            debug!("{line}");
        }
    }

    fn summary(&self, run: &Run) -> Option<String> {
        if run.frames < 2 {
            return None;
        }
        let length = self.last?.saturating_sub(run.first);
        let budget = ms(self.refresh);
        let (cpu, gpu, latency) = (sorted(&run.cpu), sorted(&run.gpu), sorted(&run.latency));
        let (ahead, lead) = (sorted(&run.ahead), sorted(&run.lead));
        let busy = (median(&cpu) + median(&gpu)) / budget * 100.0;
        let parts = run.parts.each_ref().map(|p| sorted(p));
        let parts = ["scene", "glass", "frame", "commit"]
            .iter()
            .zip(&parts)
            .map(|(name, p)| format!("{name} {:.2}/{:.2}", median(p), percentile(p, 0.99)))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "pacing: {} frames in {:.2} s at {:.0} Hz, {} late (drawing {}, GPU {}, display {}); \
             change to screen median {:.2} ms, max {:.2}; ready median {:.2} ms before the \
             vblank begins, least {:.2}; lead median {:.2} ms, max {:.2}; drawing (CPU) median \
             {:.2} ms, 99 % {:.2}, max {:.2}; \
             GPU median {:.2} ms, max {:.2}; busy {:.0} % of the {:.2} ms per frame; \
             drawing in parts, median/99 % ms: {parts}",
            run.frames,
            length.as_secs_f64(),
            1000.0 / budget,
            run.late_drawing + run.late_gpu + run.late_display,
            run.late_drawing,
            run.late_gpu,
            run.late_display,
            median(&latency),
            max(&latency),
            median(&ahead),
            ahead.first().copied().unwrap_or(0.0),
            median(&lead),
            max(&lead),
            median(&cpu),
            percentile(&cpu, 0.99),
            max(&cpu),
            median(&gpu),
            max(&gpu),
            busy,
            budget,
        ))
    }
}

/// A late frame's times, in milliseconds from where its vblank begins
/// (negative: before); `shown` is where the vblank that showed it began.
fn late(
    who: &str,
    times: &Times,
    begins: Duration,
    ready: Duration,
    shown: Duration,
    gpu_ms: Option<f64>,
) -> String {
    let at = |t: Duration| ms(t) - ms(begins);
    let plan = match (times.lead, times.aimed) {
        (Some(lead), true) => format!("planned with a lead of {:.2} ms", ms(lead)),
        (Some(lead), false) => format!("drawn at once (lead {:.2} ms)", ms(lead)),
        (None, _) => "drawn without a plan".to_string(),
    };
    let p = times.parts;
    // Of the time from commit to ready, how long the GPU worked; the rest it
    // waited (for a client's buffer, a busy queue).
    let gpu = gpu_ms.map_or("GPU time unknown".to_string(), |g| {
        format!("on the GPU {g:.2} ms")
    });
    format!(
        "late frame ({who}), ms from its vblank's begin: change {:.2}, planned {:.2}, began \
         {:.2}, committed {:.2}, ready {:.2}, shown {:.2}; {gpu}; {plan}; drawing in parts, ms: \
         scene {:.2}, glass {:.2}, frame {:.2}, commit {:.2}",
        at(times.requested),
        at(times.planned),
        at(times.began),
        at(times.committed),
        at(ready),
        at(shown),
        ms(p.scene),
        ms(p.glass),
        ms(p.frame),
        ms(p.commit),
    )
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v
}

/// The value below which `p` of the sorted `values` lie; 0 without any.
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let i = ((values.len() - 1) as f64 * p).round() as usize;
    values[i.min(values.len() - 1)]
}

fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
}

fn max(values: &[f64]) -> f64 {
    values.last().copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: Duration = Duration::from_micros(4167);
    const BLANK: Duration = Duration::from_micros(461);

    fn at(us: u64) -> Duration {
        Duration::from_micros(us)
    }

    /// A frame asked for `before` µs ahead of vblank `n`, drawn in `cpu`
    /// µs, shown `shown` vblanks after `n`.
    fn frame(p: &mut Pacing, n: u64, before: u64, cpu: u64, shown: u64) {
        let vblank = REFRESH.as_micros() as u64;
        let requested = at(n * vblank - before);
        let times = Times {
            requested,
            due: at(n * vblank),
            planned: requested,
            began: requested,
            committed: requested + at(cpu),
            lead: Some(at(3000)),
            aimed: true,
            parts: Parts::default(),
        };
        p.shown(times, at((n + shown) * vblank), Some(0.1), None);
    }

    #[test]
    fn on_time_frames_are_not_late() {
        let mut p = Pacing::new(REFRESH, BLANK);
        for n in 1..=240 {
            frame(&mut p, n, 3000, 200, 0);
        }
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 240);
        let line = p.summary(&p.run).unwrap();
        assert!(line.contains("240 frames"), "{line}");
        assert!(line.contains("0 late"), "{line}");
        // Committed 2.8 ms before the vblank's end, 0.461 ms of blank.
        assert!(line.contains("ready median 2.34 ms before"), "{line}");
        assert!(line.contains("lead median 3.00 ms, max 3.00"), "{line}");
    }

    #[test]
    fn late_frames_are_blamed_on_who_held_them() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        // Committed inside vblank 2: after it began.
        frame(&mut p, 2, 3000, 2800, 1);
        // Committed in time, shown a vblank later: the display.
        frame(&mut p, 4, 3000, 200, 1);
        assert_eq!(
            (p.run.late_drawing, p.run.late_gpu, p.run.late_display),
            (1, 0, 1)
        );
        // Committed in time, the GPU done after the vblank began.
        let vblank = REFRESH.as_micros() as u64;
        let times = Times {
            requested: at(6 * vblank - 3000),
            due: at(6 * vblank),
            planned: at(6 * vblank - 3000),
            began: at(6 * vblank - 3000),
            committed: at(6 * vblank - 2800),
            lead: None,
            aimed: false,
            parts: Parts::default(),
        };
        p.shown(times, at(7 * vblank), None, Some(at(6 * vblank - 100)));
        assert_eq!(p.run.late_gpu, 1);
    }

    #[test]
    fn a_late_frame_is_logged_with_its_times() {
        // Asked for 3 ms before vblank 2 ends, committed 0.2 ms later, shown
        // a vblank late: everything from the begin of vblank 2 (0.461 ms of
        // blank before its end).
        let vblank = REFRESH.as_micros() as u64;
        let times = Times {
            requested: at(2 * vblank - 3000),
            due: at(2 * vblank),
            planned: at(2 * vblank - 3000),
            began: at(2 * vblank - 3000),
            committed: at(2 * vblank - 2800),
            lead: Some(at(3000)),
            aimed: true,
            parts: Parts {
                scene: at(50),
                glass: at(20),
                frame: at(110),
                commit: at(20),
            },
        };
        let begins = times.due - BLANK;
        let line = late(
            "display",
            &times,
            begins,
            times.committed,
            at(3 * vblank) - BLANK,
            Some(0.08),
        );
        assert_eq!(
            line,
            "late frame (display), ms from its vblank's begin: change -2.54, planned -2.54, \
             began -2.54, committed -2.34, ready -2.34, shown 4.17; on the GPU 0.08 ms; planned \
             with a lead of 3.00 ms; drawing in parts, ms: scene 0.05, glass 0.02, frame 0.11, \
             commit 0.02"
        );
        // Only the first few of a run are logged one by one.
        let mut p = Pacing::new(REFRESH, BLANK);
        for n in 1..=20 {
            frame(&mut p, 2 * n, 3000, 200, 1);
        }
        assert_eq!((p.run.late_display, p.run.logged), (20, LOGGED_LATE));
    }

    #[test]
    fn a_pause_in_frames_is_not_lateness() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        // The client draws again 10 vblanks later: no frame was late.
        frame(&mut p, 11, 3000, 200, 0);
        assert_eq!((p.run.late_drawing, p.run.late_display), (0, 0));
        assert_eq!(p.run.frames, 2);
    }

    #[test]
    fn idle_ends_the_run() {
        let mut p = Pacing::new(REFRESH, BLANK);
        frame(&mut p, 1, 3000, 200, 0);
        frame(&mut p, 2, 3000, 200, 0);
        assert!(p.idle(at(20_000)));
        assert!(!p.idle(at(400_000)));
        assert_eq!(p.run.frames, 0);
        assert!(!p.idle(at(800_000)));
    }
}
