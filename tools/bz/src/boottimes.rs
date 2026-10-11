//! `bz boot-times`: where a boot spent its time, read from its journal –
//! the one of a real machine (see docs/hardware.md) or of a VM.
//!
//! Four parts: the phases systemd reports at the end of the boot; the gaps
//! between kernel messages before the initrd starts; the slowest units of
//! the initrd and of the system; and the critical chain that
//! `beize-boot-chain.service` writes into the journal after every boot.
//!
//! Unit times are paired by systemd's status texts ("Starting X...",
//! "Started X.") rather than by structured fields: until journald runs –
//! early in the initrd, around the switch to the real root – PID 1 logs
//! through the kernel's ring buffer, where only the text survives.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::util;

/// systemd's "Startup finished in …" message (SD_MESSAGE_STARTUP_FINISHED).
const STARTUP_FINISHED: &str = "b07a249cd024414a82dd00cd181378ff";
/// What `beize-boot-chain.service` logs as.
const CHAIN_IDENTIFIER: &str = "beize-boot-chain";
/// How many of the slowest units are shown per phase.
const SLOWEST: usize = 10;

/// Prints where boot `boot` (as `journalctl --boot` takes it: `0` the last,
/// `-1` the one before, or a boot ID) in the journal at `journal` (a
/// directory or a file) spent its time; kernel gaps from `gap_ms` up. A
/// `.json` file is one boot as `bz smoke` keeps it next to the console log
/// (`journalctl --output=json`); `boot` does not apply to it.
pub fn show(journal: &Path, boot: &str, gap_ms: u64) -> Result<()> {
    let json = if journal.extension().is_some_and(|e| e == "json") {
        fs::read_to_string(journal).with_context(|| format!("reading {}", journal.display()))?
    } else {
        let source = if journal.is_dir() {
            "--directory"
        } else {
            "--file"
        };
        util::output(Command::new("journalctl").arg(source).arg(journal).args([
            "--boot",
            boot,
            "--output=json",
            "--no-pager",
            "--quiet",
        ]))?
    };
    let report = Report::from_json(&json, gap_ms * 1000)
        .with_context(|| format!("boot {boot} in {}", journal.display()))?;
    print!("{}", report.render());
    Ok(())
}

/// The `count` slowest units of the initrd and of the system, as "unit
/// time", in a boot's journal as `bz smoke` keeps it (see [`show`]).
pub fn slowest_units(journal: &Path, count: usize) -> Result<[Vec<String>; 2]> {
    let json =
        fs::read_to_string(journal).with_context(|| format!("reading {}", journal.display()))?;
    let report = Report::from_json(&json, u64::MAX)
        .with_context(|| format!("the boot in {}", journal.display()))?;
    let slowest = |units: &[UnitTime]| {
        units
            .iter()
            .take(count)
            .map(|u| {
                let name = u.unit.as_deref().unwrap_or(&u.description);
                format!("{name} {}", seconds(u.took))
            })
            .collect()
    };
    Ok([slowest(&report.initrd_units), slowest(&report.system_units)])
}

/// One journal entry, with what the analysis needs.
#[derive(Debug)]
struct Entry {
    /// When it happened, in µs since the kernel started: the source's
    /// timestamp where there is one (kernel messages are read by journald
    /// much later), otherwise when journald received it.
    time: u64,
    boot_id: String,
    message: String,
    message_id: Option<String>,
    identifier: Option<String>,
    unit: Option<String>,
    /// Sent by the system manager (PID 1), through journald or the kernel.
    pid1: bool,
    /// The kernel's own message.
    kernel: bool,
    /// The phase durations of "Startup finished", in µs.
    phases: [Option<u64>; 3],
}

impl Entry {
    /// Parses a line of `journalctl --output=json`.
    fn parse(line: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(line)?;
        let field = |key: &str| value.get(key).and_then(text);
        let number = |key: &str| field(key)?.parse::<u64>().ok();
        let time = number("_SOURCE_MONOTONIC_TIMESTAMP")
            .or_else(|| number("__MONOTONIC_TIMESTAMP"))
            .context("a journal entry without a monotonic timestamp")?;
        let identifier = field("SYSLOG_IDENTIFIER");
        let kernel_transport = field("_TRANSPORT").as_deref() == Some("kernel");
        let pid1 = identifier.as_deref() == Some("systemd")
            && [field("_PID"), field("SYSLOG_PID")]
                .iter()
                .any(|pid| pid.as_deref() == Some("1"));
        Ok(Self {
            time,
            boot_id: field("_BOOT_ID").unwrap_or_default(),
            message: field("MESSAGE").unwrap_or_default(),
            message_id: field("MESSAGE_ID"),
            kernel: kernel_transport && identifier.as_deref() == Some("kernel"),
            identifier,
            unit: field("UNIT"),
            pid1,
            phases: ["KERNEL_USEC", "INITRD_USEC", "USERSPACE_USEC"].map(number),
        })
    }
}

/// A journal field as text: JSON output gives a string, an array of bytes
/// (not valid UTF-8) or, for a repeated field, an array of values.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) if items.iter().all(Value::is_u64) => {
            let bytes: Vec<u8> = items
                .iter()
                .filter_map(|b| u8::try_from(b.as_u64()?).ok())
                .collect();
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        Value::Array(items) => items.first().and_then(text),
        _ => None,
    }
}

/// A phase of the boot with its duration in µs.
type Phase = (&'static str, u64);

/// How long a unit took to start.
#[derive(Debug, PartialEq)]
struct UnitTime {
    /// The unit's description, as in the status texts.
    description: String,
    unit: Option<String>,
    started: u64,
    took: u64,
    failed: bool,
}

/// A gap between two kernel messages.
#[derive(Debug, PartialEq)]
struct Gap {
    from: u64,
    to: u64,
    before: String,
    after: String,
}

#[derive(Debug)]
struct Report {
    boot_id: String,
    phases: Vec<Phase>,
    /// When the kernel handed over to the initrd and the initrd to the
    /// system, and when the boot finished (µs since the kernel started).
    initrd_start: u64,
    system_start: u64,
    finished: u64,
    gap_min: u64,
    gaps: Vec<Gap>,
    initrd_units: Vec<UnitTime>,
    system_units: Vec<UnitTime>,
    chain: Vec<String>,
}

impl Report {
    /// From the output of `journalctl --output=json` for one boot.
    fn from_json(json: &str, gap_min: u64) -> Result<Self> {
        let entries = json
            .lines()
            .map(Entry::parse)
            .collect::<Result<Vec<_>>>()
            .context("reading the journal's JSON output")?;
        Self::new(&entries, gap_min).context(
            "no \"Startup finished\" message: the boot did not finish, \
             or the journal ends before",
        )
    }

    /// `None` without a "Startup finished" message from PID 1.
    fn new(entries: &[Entry], gap_min: u64) -> Option<Self> {
        let finished = entries
            .iter()
            .find(|e| e.pid1 && e.message_id.as_deref() == Some(STARTUP_FINISHED))?;
        let [kernel, initrd, userspace] = finished.phases;
        let (kernel, userspace) = (kernel?, userspace?);
        // Without an initrd, systemd reports no initrd phase.
        let initrd = initrd.unwrap_or(0);
        let mut phases = timespans(&finished.message);
        phases.retain(|(name, _)| matches!(*name, "firmware" | "loader"));
        phases.extend([
            ("kernel", kernel),
            ("initrd", initrd),
            ("userspace", userspace),
        ]);
        let initrd_start = kernel;
        let system_start = kernel + initrd;
        let finished_at = system_start + userspace;

        let units = unit_times(entries);
        let in_phase = |from: u64, to: u64| {
            let mut phase: Vec<UnitTime> = units
                .iter()
                .filter(|u| (from..to).contains(&u.started))
                .map(|u| UnitTime {
                    description: u.description.clone(),
                    unit: u.unit.clone(),
                    ..*u
                })
                .collect();
            phase.sort_by_key(|u| std::cmp::Reverse(u.took));
            phase.truncate(SLOWEST);
            phase
        };
        Some(Self {
            boot_id: finished.boot_id.clone(),
            phases,
            initrd_start,
            system_start,
            finished: finished_at,
            gap_min,
            gaps: kernel_gaps(entries, initrd_start, gap_min),
            initrd_units: in_phase(initrd_start, system_start),
            system_units: in_phase(system_start, finished_at),
            chain: entries
                .iter()
                .filter(|e| e.identifier.as_deref() == Some(CHAIN_IDENTIFIER))
                .map(|e| e.message.clone())
                .collect(),
        })
    }

    fn render(&self) -> String {
        let mut out = format!("Boot {}\n\nPhases\n", self.boot_id);
        for (name, took) in &self.phases {
            out += &format!("  {name:<10} {:>8}\n", seconds(*took));
        }
        let total: u64 = self.phases.iter().map(|(_, took)| took).sum();
        out += &format!("  {:<10} {:>8}\n", "total", seconds(total));

        let lost: u64 = self.gaps.iter().map(|g| g.to - g.from).sum();
        out += &format!(
            "\nKernel: gaps of {} ms or more between messages, {} of {} in total\n",
            self.gap_min / 1000,
            seconds(lost),
            seconds(self.initrd_start)
        );
        if self.gaps.is_empty() {
            out += "  none\n";
        }
        for gap in &self.gaps {
            out += &format!(
                "  {:>8} → {:>8} {:>8}\n    before: {}\n    after:  {}\n",
                seconds(gap.from),
                seconds(gap.to),
                seconds(gap.to - gap.from),
                gap.before,
                gap.after
            );
        }

        for (title, units, from, to) in [
            (
                "initrd",
                &self.initrd_units,
                self.initrd_start,
                self.system_start,
            ),
            (
                "System",
                &self.system_units,
                self.system_start,
                self.finished,
            ),
        ] {
            out += &format!(
                "\n{title} ({} → {}): slowest units, with when they started\n",
                seconds(from),
                seconds(to)
            );
            if units.is_empty() {
                out += "  none\n";
            }
            for unit in units {
                let name = unit
                    .unit
                    .as_ref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                let failed = if unit.failed { " – FAILED" } else { "" };
                out += &format!(
                    "  {:>8}  at {:>8}  {}{name}{failed}\n",
                    seconds(unit.took),
                    seconds(unit.started),
                    unit.description
                );
            }
        }

        out += "\nCritical chain of the system (systemd-analyze, logged by beize-boot-chain)\n";
        if self.chain.is_empty() {
            out += "  not in the journal: the image predates beize-boot-chain.service, \
                    or the boot ended before it ran\n";
        }
        for line in &self.chain {
            out += &format!("  {line}\n");
        }
        out
    }
}

/// µs as seconds with milliseconds, e.g. `11.718 s`; cut off like systemd
/// does, so the phases read the same as in "Startup finished".
fn seconds(us: u64) -> String {
    format!("{}.{:03} s", us / 1_000_000, us / 1000 % 1000)
}

/// The kernel's own messages before `until`, and every pause of at least
/// `min` µs between two of them.
fn kernel_gaps(entries: &[Entry], until: u64, min: u64) -> Vec<Gap> {
    let mut kernel: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.kernel && e.time <= until)
        .collect();
    kernel.sort_by_key(|e| e.time);
    kernel
        .windows(2)
        .filter(|pair| pair[1].time - pair[0].time >= min)
        .map(|pair| Gap {
            from: pair[0].time,
            to: pair[1].time,
            before: pair[0].message.clone(),
            after: pair[1].message.clone(),
        })
        .collect()
}

/// How long each unit took from "Starting X..." to "Started X.",
/// "Finished X." or "Failed to start X.", from the messages of PID 1.
fn unit_times(entries: &[Entry]) -> Vec<UnitTime> {
    let mut messages: Vec<&Entry> = entries.iter().filter(|e| e.pid1).collect();
    messages.sort_by_key(|e| e.time);
    // Only some messages name their unit (those sent through journald).
    let mut names: HashMap<&str, &str> = HashMap::new();
    for entry in &messages {
        if let (Some(unit), Some(description)) = (&entry.unit, status_subject(&entry.message)) {
            names.insert(description, unit);
        }
    }
    let mut starting: HashMap<&str, u64> = HashMap::new();
    let mut times = Vec::new();
    for entry in messages {
        let message = entry.message.as_str();
        if let Some(description) = message
            .strip_prefix("Starting ")
            .and_then(|m| m.strip_suffix("..."))
        {
            starting.insert(description, entry.time);
            continue;
        }
        let Some(description) = status_subject(message) else {
            continue;
        };
        if let Some(started) = starting.remove(description) {
            times.push(UnitTime {
                description: description.to_owned(),
                unit: names.get(description).map(|&n| n.to_owned()),
                started,
                took: entry.time - started,
                failed: message.starts_with("Failed to start "),
            });
        }
    }
    times
}

/// The description in a message that ends a unit's start, e.g.
/// "Journal Service" in "Started Journal Service.".
fn status_subject(message: &str) -> Option<&str> {
    ["Started ", "Finished ", "Failed to start "]
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix))?
        .strip_suffix('.')
}

/// The parts of "Startup finished in 16.915s (firmware) + 1.035s (loader)
/// + … = 35.330s." as (name, µs).
fn timespans(message: &str) -> Vec<Phase> {
    let Some(parts) = message
        .strip_prefix("Startup finished in ")
        .and_then(|m| m.split(" = ").next())
    else {
        return Vec::new();
    };
    parts
        .split(" + ")
        .filter_map(|part| {
            let (span, name) = part.trim().rsplit_once(" (")?;
            let name = name.strip_suffix(')')?;
            let name = ["firmware", "loader", "kernel", "initrd", "userspace"]
                .into_iter()
                .find(|known| *known == name)?;
            Some((name, timespan(span)?))
        })
        .collect()
}

/// A duration as systemd writes it (`11.716s`, `1min 11.716s`, `350ms`) in µs.
fn timespan(text: &str) -> Option<u64> {
    const UNITS: [(&str, f64); 6] = [
        ("min", 60e6),
        ("ms", 1e3),
        ("us", 1.0),
        ("μs", 1.0),
        ("h", 3600e6),
        ("s", 1e6),
    ];
    if text.trim().is_empty() {
        return None;
    }
    let mut total = 0.0;
    for token in text.split_whitespace() {
        let (number, factor) = UNITS
            .iter()
            .find_map(|(unit, factor)| Some((token.strip_suffix(unit)?, *factor)))?;
        total += number.parse::<f64>().ok()? * factor;
    }
    Some(total.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal entry as `journalctl --output=json` writes it.
    fn json(fields: &[(&str, &str)]) -> String {
        let map: serde_json::Map<String, Value> = fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
            .collect();
        Value::Object(map).to_string()
    }

    fn kernel(us: u64, message: &str) -> String {
        json(&[
            ("__MONOTONIC_TIMESTAMP", "40000000"),
            ("_SOURCE_MONOTONIC_TIMESTAMP", &us.to_string()),
            ("_TRANSPORT", "kernel"),
            ("SYSLOG_IDENTIFIER", "kernel"),
            ("_BOOT_ID", "b1"),
            ("MESSAGE", message),
        ])
    }

    /// PID 1 through the kernel's ring buffer (before journald runs).
    fn pid1_kmsg(us: u64, message: &str) -> String {
        json(&[
            ("__MONOTONIC_TIMESTAMP", "40000000"),
            ("_SOURCE_MONOTONIC_TIMESTAMP", &us.to_string()),
            ("_TRANSPORT", "kernel"),
            ("SYSLOG_IDENTIFIER", "systemd"),
            ("SYSLOG_PID", "1"),
            ("_BOOT_ID", "b1"),
            ("MESSAGE", message),
        ])
    }

    /// PID 1 through journald.
    fn pid1(us: u64, unit: &str, message: &str) -> String {
        json(&[
            ("__MONOTONIC_TIMESTAMP", &us.to_string()),
            ("_TRANSPORT", "journal"),
            ("SYSLOG_IDENTIFIER", "systemd"),
            ("_PID", "1"),
            ("_BOOT_ID", "b1"),
            ("UNIT", unit),
            ("MESSAGE", message),
        ])
    }

    fn finished() -> String {
        json(&[
            ("__MONOTONIC_TIMESTAMP", "17379000"),
            ("_TRANSPORT", "journal"),
            ("SYSLOG_IDENTIFIER", "systemd"),
            ("_PID", "1"),
            ("_BOOT_ID", "b1"),
            ("MESSAGE_ID", STARTUP_FINISHED),
            ("KERNEL_USEC", "11718537"),
            ("INITRD_USEC", "4018236"),
            ("USERSPACE_USEC", "1642235"),
            (
                "MESSAGE",
                "Startup finished in 16.915s (firmware) + 1.035s (loader) + 11.718s (kernel) \
                 + 4.018s (initrd) + 1.642s (userspace) = 35.330s.",
            ),
        ])
    }

    fn report(lines: &[String]) -> Option<Report> {
        let entries: Vec<Entry> = lines.iter().map(|l| Entry::parse(l).unwrap()).collect();
        Report::new(&entries, 100_000)
    }

    #[test]
    fn reads_systemd_timespans() {
        assert_eq!(timespan("11.716s"), Some(11_716_000));
        assert_eq!(timespan("1min 11.716s"), Some(71_716_000));
        assert_eq!(timespan("350ms"), Some(350_000));
        assert_eq!(timespan("812us"), Some(812));
        assert_eq!(timespan("1h 2min"), Some(3_720_000_000));
        assert_eq!(timespan(""), None);
        assert_eq!(timespan("11.7 parsecs"), None);
    }

    #[test]
    fn phases_from_startup_finished() {
        let report = report(&[finished()]).unwrap();
        assert_eq!(
            report.phases,
            [
                ("firmware", 16_915_000),
                ("loader", 1_035_000),
                ("kernel", 11_718_537),
                ("initrd", 4_018_236),
                ("userspace", 1_642_235),
            ]
        );
        assert_eq!(report.system_start, 15_736_773);
        assert!(report.render().contains("  total      35.329 s\n"));
    }

    #[test]
    fn a_boot_that_did_not_finish_has_no_report() {
        assert!(report(&[kernel(0, "Linux version 7.2.9-beize")]).is_none());
        // Another manager's (a user's) message does not count.
        let user = finished().replace(r#""_PID":"1""#, r#""_PID":"1000""#);
        assert!(report(&[user]).is_none());
    }

    #[test]
    fn kernel_gaps_use_the_source_timestamp_and_stop_at_the_initrd() {
        let report = report(&[
            finished(),
            kernel(211_000, "pci 0000:00:1a.0: PCI bridge"),
            kernel(1_210_000, "pci 0000:00:1a.0: retraining failed"),
            kernel(1_250_000, "next"),
            // Written by PID 1 through the ring buffer: not the kernel.
            pid1_kmsg(5_000_000, "Hello"),
            kernel(11_700_000, "Run /init as init process"),
            // In the initrd: not part of the kernel phase.
            kernel(15_000_000, "usb 2-8.4: new SuperSpeed USB device"),
        ])
        .unwrap();
        assert_eq!(
            report.gaps,
            [
                Gap {
                    from: 211_000,
                    to: 1_210_000,
                    before: "pci 0000:00:1a.0: PCI bridge".into(),
                    after: "pci 0000:00:1a.0: retraining failed".into(),
                },
                Gap {
                    from: 1_250_000,
                    to: 11_700_000,
                    before: "next".into(),
                    after: "Run /init as init process".into(),
                },
            ]
        );
    }

    #[test]
    fn units_are_paired_by_their_status_texts_in_each_phase() {
        let report = report(&[
            finished(),
            // initrd, before journald: text only.
            pid1_kmsg(12_188_000, "Starting Journal Service..."),
            pid1_kmsg(12_203_000, "Started Journal Service."),
            // Starting through the ring buffer, done through journald.
            pid1_kmsg(12_300_000, "Starting Cryptography Setup for root..."),
            pid1(
                14_800_000,
                "systemd-cryptsetup@root.service",
                "Finished Cryptography Setup for root.",
            ),
            // System.
            pid1(16_000_000, "a.service", "Starting A..."),
            pid1(16_500_000, "a.service", "Failed to start A."),
            // A start that never ended is not a unit time.
            pid1(16_100_000, "b.service", "Starting B..."),
            // After the boot finished: not part of it.
            pid1(18_000_000, "late.service", "Starting Late..."),
            pid1(18_100_000, "late.service", "Started Late."),
        ])
        .unwrap();
        assert_eq!(
            report.initrd_units,
            [
                UnitTime {
                    description: "Cryptography Setup for root".into(),
                    unit: Some("systemd-cryptsetup@root.service".into()),
                    started: 12_300_000,
                    took: 2_500_000,
                    failed: false,
                },
                UnitTime {
                    description: "Journal Service".into(),
                    unit: None,
                    started: 12_188_000,
                    took: 15_000,
                    failed: false,
                },
            ]
        );
        assert_eq!(
            report.system_units,
            [UnitTime {
                description: "A".into(),
                unit: Some("a.service".into()),
                started: 16_000_000,
                took: 500_000,
                failed: true,
            }]
        );
    }

    #[test]
    fn critical_chain_from_beize_boot_chain() {
        let chain = json(&[
            ("__MONOTONIC_TIMESTAMP", "18000000"),
            ("_BOOT_ID", "b1"),
            ("SYSLOG_IDENTIFIER", CHAIN_IDENTIFIER),
            ("MESSAGE", "multi-user.target @1.642s"),
        ]);
        let with = report(&[finished(), chain]).unwrap();
        assert_eq!(with.chain, ["multi-user.target @1.642s"]);
        assert!(with.render().contains("  multi-user.target @1.642s\n"));
        let without = report(&[finished()]).unwrap().render();
        assert!(without.contains("not in the journal"));
    }

    #[test]
    fn fields_in_bytes_or_repeated() {
        let entry = Entry::parse(
            r#"{"__MONOTONIC_TIMESTAMP":"5","MESSAGE":[104,105,255],"UNIT":["a.service","b.service"]}"#,
        )
        .unwrap();
        assert_eq!(entry.message, "hi\u{fffd}");
        assert_eq!(entry.unit.as_deref(), Some("a.service"));
        assert!(Entry::parse(r#"{"MESSAGE":"no time"}"#).is_err());
    }
}
