//! When a package build may start, beside others: CPUs come from the
//! jobserver ([`crate::jobserver`]), memory is checked here before a build
//! starts – as Yocto's `BB_PRESSURE_MAX_MEMORY` does, with what each build
//! needed last time. A build starts only if what it needs fits into the
//! memory available now, less the reserve and less what the builds already
//! running may still take, and the kernel is not short of memory already.
//! Among the builds that may start, the one on the longest remaining chain
//! goes first.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::paths::Paths;
use crate::util;

/// Assumed for a build never measured: more than any but the largest
/// (gcc 4.6 GiB, ntpd-rs 3.2 GiB, zstd 1.1 GiB; all others below 0.7 GiB,
/// measured 2026-10-13 with 10 jobs).
const UNMEASURED_MIB: u64 = 2048;
/// Assumed for a build never timed.
const UNTIMED_SECS: u64 = 10;
/// Memory left to everything else on the machine.
const RESERVE_MIB: u64 = 2048;
/// Memory per parallel build job. Measured (2026-10-13, anonymous memory of
/// each package's build with 10 jobs): gcc, the largest, 4.6 GiB – 460 MiB a
/// job. Running out of memory takes down the whole build machine (WSL
/// instance, CI runner) rather than just failing the build.
const MEMORY_PER_JOB_MIB: u64 = 512;
/// No new build starts while tasks wait for memory this share of the time
/// (`some avg10` of /proc/pressure/memory, percent): the kernel is
/// reclaiming hard, more builds would only make it worse.
const MAX_MEMORY_PRESSURE: f64 = 10.0;

/// Number of parallel build jobs: one per CPU, limited by the memory that is
/// available now, less the reserve.
pub fn job_count() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let available_kib = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| util::meminfo_kib(&info, "MemAvailable"))
        .map(|kib| kib.saturating_sub(RESERVE_MIB * 1024));
    util::jobs_for(cpus, available_kib, MEMORY_PER_JOB_MIB)
}

/// What a build needed last time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measured {
    /// Peak of anonymous memory, if its cgroup could tell.
    pub anon_mib: Option<u64>,
    pub secs: u64,
}

impl Measured {
    pub fn memory_mib(&self) -> u64 {
        self.anon_mib.unwrap_or(UNMEASURED_MIB)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut fields = text.split_whitespace();
        let anon = fields.next()?;
        let secs = fields.next()?.parse().ok()?;
        let anon_mib = match anon {
            "-" => None,
            mib => Some(mib.parse().ok()?),
        };
        Some(Self { anon_mib, secs })
    }

    fn text(&self) -> String {
        let anon = self.anon_mib.map_or("-".to_owned(), |mib| mib.to_string());
        format!("{anon} {}\n", self.secs)
    }
}

fn record(paths: &Paths, name: &str) -> PathBuf {
    paths.work.join("measured").join(name)
}

/// What `name` needed when it was last built.
pub fn measured(paths: &Paths, name: &str) -> Option<Measured> {
    Measured::parse(&fs::read_to_string(record(paths, name)).ok()?)
}

pub fn remember(paths: &Paths, name: &str, measured: Measured) -> Result<()> {
    let path = record(paths, name);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, measured.text())?;
    Ok(())
}

/// For each build, how long the longest chain of builds from its start to
/// the end of all builds takes: its own time plus the longest chain of what
/// waits for it. `deps` lists each build's dependencies among the builds.
pub fn chain_secs(
    deps: &BTreeMap<String, Vec<String>>,
    secs: impl Fn(&str) -> u64,
) -> BTreeMap<String, u64> {
    fn visit(
        name: &str,
        dependents: &BTreeMap<&str, Vec<&str>>,
        secs: &dyn Fn(&str) -> u64,
        chain: &mut BTreeMap<String, u64>,
    ) -> u64 {
        if let Some(&known) = chain.get(name) {
            return known;
        }
        let after = dependents.get(name).map_or(0, |waiting| {
            waiting
                .iter()
                .map(|next| visit(next, dependents, secs, chain))
                .max()
                .unwrap_or(0)
        });
        let total = secs(name) + after;
        chain.insert(name.to_owned(), total);
        total
    }

    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, needs) in deps {
        for dep in needs {
            dependents.entry(dep).or_default().push(name);
        }
    }
    let mut chain = BTreeMap::new();
    for name in deps.keys() {
        visit(name, &dependents, &secs, &mut chain);
    }
    chain
}

pub fn chain_secs_of(paths: &Paths, deps: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, u64> {
    chain_secs(deps, |name| {
        measured(paths, name).map_or(UNTIMED_SECS, |m| m.secs)
    })
}

/// The machine's memory now.
#[derive(Clone, Copy, Debug)]
pub struct Memory {
    pub available_mib: u64,
    /// `some avg10` of memory pressure, if the kernel reports it.
    pub pressure: Option<f64>,
}

impl Memory {
    pub fn now() -> Option<Self> {
        let info = fs::read_to_string("/proc/meminfo").ok()?;
        Some(Self {
            available_mib: util::meminfo_kib(&info, "MemAvailable")? / 1024,
            pressure: fs::read_to_string("/proc/pressure/memory")
                .ok()
                .and_then(|text| some_avg10(&text)),
        })
    }

    /// Whether a build that needs `need_mib` may start, while the running
    /// builds may still take `pending_mib` beyond what they hold now.
    pub fn admits(&self, need_mib: u64, pending_mib: u64) -> bool {
        self.pressure.is_none_or(|p| p < MAX_MEMORY_PRESSURE)
            && self
                .available_mib
                .saturating_sub(RESERVE_MIB)
                .saturating_sub(pending_mib)
                >= need_mib
    }
}

fn some_avg10(pressure: &str) -> Option<f64> {
    pressure
        .lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// The memory limit of each build's cgroup: all the machine can give
/// without touching the reserve. Together with the check before each start
/// it keeps builds side by side from running out of memory; alone it keeps
/// one runaway build from taking the machine down.
pub fn limit_mib() -> u64 {
    Memory::now().map_or(u64::MAX, |m| {
        m.available_mib.saturating_sub(RESERVE_MIB).max(1024)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        for measured in [
            Measured {
                anon_mib: Some(4618),
                secs: 203,
            },
            Measured {
                anon_mib: None,
                secs: 4,
            },
        ] {
            assert_eq!(Measured::parse(&measured.text()), Some(measured));
        }
        assert_eq!(Measured::parse("garbage"), None);
        assert_eq!(
            Measured {
                anon_mib: None,
                secs: 4
            }
            .memory_mib(),
            UNMEASURED_MIB
        );
    }

    #[test]
    fn the_longest_chain_goes_first() {
        let deps: BTreeMap<String, Vec<String>> = [
            ("openssl", vec![]),
            ("tpm2-tss", vec!["openssl"]),
            ("systemd", vec!["tpm2-tss"]),
            ("bash", vec![]),
        ]
        .into_iter()
        .map(|(name, deps)| {
            (
                name.to_owned(),
                deps.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let secs = |name: &str| match name {
            "openssl" => 30,
            "tpm2-tss" => 9,
            "systemd" => 33,
            "bash" => 40,
            _ => unreachable!(),
        };
        let chain = chain_secs(&deps, secs);
        assert_eq!(chain["openssl"], 72);
        assert_eq!(chain["tpm2-tss"], 42);
        assert_eq!(chain["systemd"], 33);
        assert_eq!(chain["bash"], 40, "longer alone, but openssl holds up more");
    }

    #[test]
    fn memory_admits_only_what_fits() {
        let memory = Memory {
            available_mib: 14 * 1024,
            pressure: Some(0.5),
        };
        // 14 GiB available, 2 GiB reserve: 12 GiB for builds.
        assert!(memory.admits(4618, 0));
        assert!(memory.admits(4618, 7 * 1024));
        assert!(!memory.admits(4618, 8 * 1024));
        let pressed = Memory {
            pressure: Some(25.0),
            ..memory
        };
        assert!(!pressed.admits(100, 0));
        let unknown = Memory {
            pressure: None,
            ..memory
        };
        assert!(unknown.admits(100, 0));
    }

    #[test]
    fn memory_limits_parallel_jobs() {
        // This machine: 24 threads, ~14 GiB available in WSL, less the
        // reserve -> CPU bound.
        assert_eq!(
            util::jobs_for(24, Some(12 * 1024 * 1024), MEMORY_PER_JOB_MIB),
            24
        );
        // CI runner: 2 CPUs, 7.8 GiB -> CPU bound.
        assert_eq!(
            util::jobs_for(2, Some(5 * 1024 * 1024), MEMORY_PER_JOB_MIB),
            2
        );
        // Little memory left: memory bound.
        assert_eq!(
            util::jobs_for(24, Some(4 * 1024 * 1024), MEMORY_PER_JOB_MIB),
            8
        );
        // Tiny machine still builds, one job at a time.
        assert_eq!(util::jobs_for(8, Some(0), MEMORY_PER_JOB_MIB), 1);
        assert_eq!(util::jobs_for(8, None, MEMORY_PER_JOB_MIB), 8);
    }

    #[test]
    fn reads_memory_pressure() {
        let text = "some avg10=12.50 avg60=3.00 avg300=0.10 total=1234\n\
                    full avg10=1.00 avg60=0.00 avg300=0.00 total=10\n";
        assert_eq!(some_avg10(text), Some(12.5));
        assert_eq!(some_avg10("full avg10=1.00\n"), None);
    }
}
