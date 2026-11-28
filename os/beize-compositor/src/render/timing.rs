//! What a frame cost: the CPU's parts (`Spent`) and the GPU's time from
//! its timestamps, calibrated against the clock (`GpuTime`).

use super::*;

impl VkRenderer {
    /// Where the CPU's time went in the submissions since the last call.
    pub fn take_spent(&mut self) -> Spent {
        std::mem::take(&mut self.spent)
    }

    /// How long the GPU worked on `submission` (`GpuTime`): once it is
    /// done (a frame on the screen is), while its frame's objects are not
    /// yet used again. None without timestamps.
    pub fn gpu_time(&self, submission: u64) -> Option<GpuTime> {
        let frame = self.in_flight.iter().find(|f| f.submission == submission)?;
        let (pool, period) = (frame.queries?, self.caps.timestamp_period?);
        let mut times = [0u64; 2];
        // SAFETY: the pool belongs to this device; without WAIT the call
        // only reports NOT_READY for a submission still running.
        let read = unsafe {
            self.shared.device.get_query_pool_results(
                pool,
                0,
                &mut times,
                vk::QueryResultFlags::TYPE_64,
            )
        };
        if read.is_err() || times[1] <= times[0] {
            return None;
        }
        let ns = |ticks: u64| (ticks as f64 * f64::from(period)) as i128;
        let mut stamps = vec![0u64; 2 * frame.blurs];
        // SAFETY: as above; the frame wrote these, its submission is done
        // or the call reports NOT_READY.
        let blurred = stamps.is_empty()
            || unsafe {
                self.shared.device.get_query_pool_results(
                    pool,
                    2,
                    &mut stamps,
                    vk::QueryResultFlags::TYPE_64,
                )
            }
            .is_ok();
        let blur = if blurred {
            stamps
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[begin, end]| (ns(*end) - ns(*begin)) as f64 / 1e6)
                .sum()
        } else {
            0.0
        };
        let span = self.calibration().map(|offset| {
            let at = |ticks| Duration::from_nanos((ns(ticks) + offset).max(0) as u64);
            (at(times[0]), at(times[1]))
        });
        Some(GpuTime {
            ms: (ns(times[1]) - ns(times[0])) as f64 / 1e6,
            span,
            blur,
        })
    }

    /// The nanoseconds from the GPU's clock to CLOCK_MONOTONIC, set anew
    /// after `CALIBRATE`; None without calibrated timestamps.
    pub(super) fn calibration(&self) -> Option<i128> {
        let calibrated = self.ext.calibrated.as_ref()?;
        if let Some((at, offset)) = self.calibration.get()
            && at.elapsed() < CALIBRATE
        {
            return Some(offset);
        }
        let period = self.caps.timestamp_period?;
        let infos = [
            vk::TimeDomainEXT::DEVICE,
            vk::TimeDomainEXT::CLOCK_MONOTONIC,
        ]
        .map(|domain| vk::CalibratedTimestampInfoEXT::default().time_domain(domain));
        // SAFETY: the device has the extension (`device::create`).
        let (stamps, _deviation) = match unsafe { calibrated.get_calibrated_timestamps(&infos) } {
            Ok(read) => read,
            Err(e) => {
                debug!("no calibrated timestamps: {e}");
                return None;
            }
        };
        let offset = i128::from(stamps[1]) - (stamps[0] as f64 * f64::from(period)) as i128;
        self.calibration
            .set(Some((std::time::Instant::now(), offset)));
        Some(offset)
    }
}
