//! A package build in a cgroup of its own: a scope of the user's systemd
//! (`systemd-run --user --scope`). Its memory limit makes a build that runs
//! away fail alone instead of taking the machine down, and its memory
//! statistics tell how much the build needs, for the next time
//! ([`crate::schedule`]). Without a user service manager (CI containers)
//! builds run as they are.

use std::ffi::OsString;
use std::fs;
use std::process::Command;
use std::sync::OnceLock;

/// Whether builds can run in scopes of their own.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "true"])
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// `cmd`, which clears its environment, in the scope `<unit>.scope` with at
/// most `limit_mib` of memory and no swap: past the limit the kernel ends the
/// build instead of the machine slowing to a crawl. systemd-run needs the
/// caller's environment to reach the service manager; the build gets only
/// its own (`env -i`).
pub fn wrap(cmd: &Command, unit: &str, limit_mib: u64) -> Command {
    let mut wrapped = Command::new("systemd-run");
    wrapped
        .args(["--user", "--scope", "--quiet", "--collect"])
        // systemd-run replaces ${...} in the command line by default: in a
        // build script, "${opts[@]}" silently became nothing.
        .arg("--expand-environment=no")
        .arg(format!("--unit={unit}"))
        .arg(format!("--property=MemoryMax={limit_mib}M"))
        .arg("--property=MemorySwapMax=0")
        .args(["--", "/usr/bin/env", "-i"]);
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            let mut pair = OsString::from(key);
            pair.push("=");
            pair.push(value);
            wrapped.arg(pair);
        }
    }
    wrapped.arg(cmd.get_program()).args(cmd.get_args());
    if let Some(dir) = cmd.get_current_dir() {
        wrapped.current_dir(dir);
    }
    wrapped
}

/// What a scope's memory statistics say now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Anonymous memory: what the build itself holds, without the page
    /// cache, which the kernel takes back when memory runs short.
    pub anon: u64,
    /// How often the kernel ended a process of the scope for lack of memory.
    pub oom_kills: u64,
}

/// The usage of the scope `<unit>.scope` that process `pid` runs in; None
/// while the process is not (yet) in it.
pub fn usage(pid: u32, unit: &str) -> Option<Usage> {
    let membership = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    if !path.ends_with(&format!("/{unit}.scope")) {
        return None;
    }
    let dir = format!("/sys/fs/cgroup{path}");
    let stat = fs::read_to_string(format!("{dir}/memory.stat")).ok()?;
    let events = fs::read_to_string(format!("{dir}/memory.events")).ok()?;
    Some(Usage {
        anon: field(&stat, "anon")?,
        oom_kills: field(&events, "oom_kill").unwrap_or(0),
    })
}

/// A `name value` line of a cgroup file.
fn field(text: &str, name: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cgroup_fields() {
        let stat = "anon 349175808\nfile 1024\nanon_thp 0\n";
        assert_eq!(field(stat, "anon"), Some(349_175_808));
        assert_eq!(field(stat, "anon_thp"), Some(0));
        assert_eq!(field("low 0\noom_kill 2\n", "oom_kill"), Some(2));
        assert_eq!(field("oom 1\n", "oom_kill"), None);
    }

    #[test]
    fn the_build_gets_only_its_own_environment() {
        let mut cmd = Command::new("bash");
        cmd.args(["-c", "make"])
            .current_dir("/tmp")
            .env_clear()
            .env("PATH", "/usr/bin");
        let wrapped = wrap(&cmd, "bz-zlib-1", 2048);
        let args: Vec<_> = wrapped.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--expand-environment=no",
                "--unit=bz-zlib-1",
                "--property=MemoryMax=2048M",
                "--property=MemorySwapMax=0",
                "--",
                "/usr/bin/env",
                "-i",
                "PATH=/usr/bin",
                "bash",
                "-c",
                "make",
            ]
        );
        assert_eq!(
            wrapped.get_current_dir(),
            Some(std::path::Path::new("/tmp"))
        );
    }
}
