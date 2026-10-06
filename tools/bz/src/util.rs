use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Runs a command, echoing it first, and fails on a non-zero exit status.
pub fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("  $ {}", describe(cmd));
    let status = cmd
        .status()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    if !status.success() {
        bail!("{} failed with {status}", describe(cmd));
    }
    Ok(())
}

/// Runs a command and returns its trimmed standard output.
pub fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    if !out.status.success() {
        bail!(
            "{} failed with {}: {}",
            describe(cmd),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Memory reserved per parallel build job. Large C++ translation units (gcc)
/// need about this much; running out of memory takes down the whole build
/// machine (WSL instance, CI runner) rather than just failing the build.
const MEMORY_PER_JOB_MIB: u64 = 1536;

/// Number of parallel build jobs: one per CPU, limited by total memory.
pub fn job_count() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let memory_kib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| mem_total_kib(&info));
    jobs_for(cpus, memory_kib)
}

fn jobs_for(cpus: usize, memory_kib: Option<u64>) -> usize {
    match memory_kib {
        Some(kib) => {
            let by_memory = usize::try_from(kib / 1024 / MEMORY_PER_JOB_MIB).unwrap_or(usize::MAX);
            cpus.min(by_memory).max(1)
        }
        None => cpus,
    }
}

fn mem_total_kib(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()
}

/// Prints a step header so long builds stay readable.
pub fn step(title: &str) {
    eprintln!("\n==> {title}");
}

fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().into_owned()];
    parts.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_total_memory() {
        let info = "MemTotal:       16152084 kB\nMemFree:        14000000 kB\n";
        assert_eq!(mem_total_kib(info), Some(16_152_084));
        assert_eq!(mem_total_kib("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn memory_limits_parallel_jobs() {
        // This machine: 24 threads, ~15.4 GiB in WSL -> memory bound.
        assert_eq!(jobs_for(24, Some(16_152_084)), 10);
        // GitHub runner: 4 CPUs, 16 GiB -> CPU bound.
        assert_eq!(jobs_for(4, Some(16 * 1024 * 1024)), 4);
        // Tiny machine still builds, one job at a time.
        assert_eq!(jobs_for(8, Some(512 * 1024)), 1);
        assert_eq!(jobs_for(8, None), 8);
    }
}
