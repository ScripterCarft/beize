use std::cell::RefCell;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Like `eprintln!`, but kept while [`capture`] runs on this thread.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::util::say_line(format!($($arg)*))
    };
}

static VERBOSE: AtomicBool = AtomicBool::new(false);
static LOCKED: AtomicBool = AtomicBool::new(false);

/// `bz --locked` (CI): files bz keeps up to date in the repository, such as
/// the resolved kernel configuration, must already be current.
pub fn set_locked(on: bool) {
    LOCKED.store(on, Ordering::Relaxed);
}

pub fn locked() -> bool {
    LOCKED.load(Ordering::Relaxed)
}

/// `bz --verbose`: echo every command and show all output as it happens.
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// How many lines of a failed command's output are shown.
const TAIL_LINES: usize = 40;

/// Runs a command and fails on a non-zero exit status. Its output is shown
/// only when it fails (the last lines) or with `--verbose`.
pub fn run(cmd: &mut Command) -> Result<()> {
    if verbose() {
        return run_attached(cmd);
    }
    let out = cmd
        .output()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    if !out.status.success() {
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        print_text_tail(&text, &describe(cmd));
        bail!("{} failed with {}", describe(cmd), out.status);
    }
    Ok(())
}

/// Runs a command on this terminal (input and output), echoing it first;
/// for commands that may ask the user something (sudo, QEMU's console).
pub fn run_attached(cmd: &mut Command) -> Result<()> {
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
    Ok(hex(&hasher.finalize()))
}

/// Bytes as lowercase hex digits, as sha256sum writes a digest.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Memory reserved per parallel build job. Large C++ translation units (gcc)
/// need about this much; running out of memory takes down the whole build
/// machine (WSL instance, CI runner) rather than just failing the build.
const MEMORY_PER_JOB_MIB: u64 = 1536;

/// Number of parallel build jobs: one per CPU, limited by total memory.
pub fn job_count() -> usize {
    parallel(1, MEMORY_PER_JOB_MIB)
}

/// How many jobs that each need `cpus_each` CPUs and `memory_each_mib` of
/// memory this machine runs side by side (at least one).
pub fn parallel(cpus_each: usize, memory_each_mib: u64) -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let memory_kib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| mem_total_kib(&info));
    jobs_for(cpus / cpus_each.max(1), memory_kib, memory_each_mib)
}

fn jobs_for(by_cpus: usize, memory_kib: Option<u64>, memory_each_mib: u64) -> usize {
    let by_memory = memory_kib.map_or(usize::MAX, |kib| {
        usize::try_from(kib / 1024 / memory_each_mib.max(1)).unwrap_or(usize::MAX)
    });
    by_cpus.min(by_memory).max(1)
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

/// How often a long command reports that it is still alive. Its output goes
/// to a log, so without this a long build (GCC on a small CI runner takes
/// over an hour) looks exactly like a hang.
const HEARTBEAT: Duration = Duration::from_secs(60);

/// Runs a long command with its output in `log`, reporting every minute that
/// `what` is still running; on failure shows the end of the log.
pub fn run_logged(cmd: &mut Command, what: &str, log: &Path) -> Result<()> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let started = Instant::now();
    let mut child = cmd
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    let mut next = started + HEARTBEAT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= next {
            let log_kib = fs::metadata(log).map_or(0, |m| m.len() / 1024);
            say!(
                "  still building {what}: {} min, log {log_kib} KiB",
                started.elapsed().as_secs() / 60
            );
            next += HEARTBEAT;
        }
        thread::sleep(Duration::from_millis(200));
    };
    if !status.success() {
        print_tail(log);
        bail!("building {what} failed with {status}");
    }
    Ok(())
}

/// Shows the last lines of a log file.
pub fn print_tail(log: &Path) {
    if let Ok(bytes) = fs::read(log) {
        print_text_tail(&String::from_utf8_lossy(&bytes), &log.display().to_string());
    }
}

fn print_text_tail(text: &str, source: &str) {
    let lines: Vec<&str> = text.lines().collect();
    let shown = &lines[lines.len().saturating_sub(TAIL_LINES)..];
    say!("  last {} lines of {source}:", shown.len());
    for line in shown {
        say!("  | {line}");
    }
}

/// Prints a step header so long builds stay readable.
pub fn step(title: &str) {
    say!("\n==> {title}");
}

thread_local! {
    /// What a job running beside others said, shown in one piece when it
    /// is done (see [`capture`]).
    static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Prints a line of progress to stderr – or keeps it while [`capture`] runs
/// on this thread. Use through [`say!`](crate::say).
pub fn say_line(line: String) {
    if let Some(line) = CAPTURED.with_borrow_mut(|captured| match captured {
        Some(lines) => {
            lines.push(line);
            None
        }
        None => Some(line),
    }) {
        eprintln!("{line}");
    }
}

/// Runs `job` with everything it says kept instead of printed; returns its
/// result and what it said. Nests: an outer capture goes on afterwards.
pub fn capture<T>(job: impl FnOnce() -> T) -> (T, Vec<String>) {
    let outer = CAPTURED.replace(Some(Vec::new()));
    let result = job();
    (result, CAPTURED.replace(outer).unwrap_or_default())
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
        assert_eq!(jobs_for(24, Some(16_152_084), MEMORY_PER_JOB_MIB), 10);
        // GitHub runner: 4 CPUs, 16 GiB -> CPU bound.
        assert_eq!(jobs_for(4, Some(16 * 1024 * 1024), MEMORY_PER_JOB_MIB), 4);
        // Tiny machine still builds, one job at a time.
        assert_eq!(jobs_for(8, Some(512 * 1024), MEMORY_PER_JOB_MIB), 1);
        assert_eq!(jobs_for(8, None, MEMORY_PER_JOB_MIB), 8);
    }

    #[test]
    fn captured_output_is_kept_per_thread() {
        let ((), said) = capture(|| {
            say!("  ok: {}", 1);
            step("Next");
        });
        assert_eq!(said, ["  ok: 1", "\n==> Next"]);
        // A capture inside a capture keeps its own lines; the outer one goes on.
        let (inner, outer) = capture(|| {
            say!("outer 1");
            let ((), inner) = capture(|| say!("inner"));
            say!("outer 2");
            inner
        });
        assert_eq!(inner, ["inner"]);
        assert_eq!(outer, ["outer 1", "outer 2"]);
    }
}
