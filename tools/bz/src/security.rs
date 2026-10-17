//! How exposed the services in `/usr` are, against the variant's
//! `images/*/security.toml` (see `images/base/`): `systemd-analyze security` from Beize's own systemd, offline
//! on the assembled tree. Like a ratchet: a service may only become better
//! sandboxed, and the file always says how well it is.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::say;
use crate::util;

/// Where the measured exposure of Beize's services stands, for messages.
const DEFINITION: &str = "the variant's security.toml";

/// Checks the services of the `/usr` tree under `root`.
pub fn check(paths: &Paths, root: &Path) -> Result<()> {
    let exposure = paths.variant.exposure()?;
    let units = root.join("usr/lib/systemd/system");
    let mut problems = Vec::new();
    for entry in fs::read_dir(&units)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("beize-") && name.ends_with(".service") && !exposure.contains_key(&name)
        {
            problems.push(format!("{name}: Beize's service has no entry"));
        }
    }
    let measured = measure(paths, root, exposure.keys())?;
    for (unit, expected) in &exposure {
        let Some(actual) = measured.get(unit) else {
            problems.push(format!(
                "{unit}: not in /usr, or systemd-analyze gave no value"
            ));
            continue;
        };
        if (actual - expected).abs() >= 0.05 {
            let what = if actual > expected {
                "worse"
            } else {
                "better: write the new value"
            };
            problems.push(format!(
                "{unit}: {actual:.1} instead of {expected:.1} ({what})"
            ));
        }
    }
    if !problems.is_empty() {
        bail!(
            "the services' exposure differs from {DEFINITION}:\n  {}",
            problems.join("\n  ")
        );
    }
    say!(
        "  ok: {} services as sandboxed as {DEFINITION} says",
        exposure.len()
    );
    Ok(())
}

/// Each unit's overall exposure; templates with an instance.
fn measure<'a>(
    paths: &Paths,
    root: &Path,
    units: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, f64>> {
    let sysroot = &paths.sysroot;
    let mut instances = BTreeMap::new();
    for unit in units {
        instances.insert(unit.replace("@.service", "@x.service"), unit.clone());
    }
    let out = util::output(
        Command::new(sysroot.join("usr/lib/ld-linux-x86-64.so.2"))
            .arg("--library-path")
            .arg(format!(
                "{0}/usr/lib:{0}/usr/lib/systemd",
                sysroot.display()
            ))
            .arg(sysroot.join("usr/bin/systemd-analyze"))
            .args(["security", "--offline=true", "--no-pager"])
            .arg(format!("--root={}", root.display()))
            .args(instances.keys()),
    )
    .context("running systemd-analyze security")?;
    Ok(parse(&out)
        .into_iter()
        .filter_map(|(instance, value)| Some((instances.get(&instance)?.clone(), value)))
        .collect())
}

/// `→ Overall exposure level for x.service: 1.7 OK 🙂` → (x.service, 1.7).
fn parse(output: &str) -> BTreeMap<String, f64> {
    output
        .lines()
        .filter_map(|line| {
            let rest = line.split("Overall exposure level for ").nth(1)?;
            let (unit, value) = rest.split_once(": ")?;
            Some((
                unit.to_owned(),
                value.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_overall_exposure() {
        let out = "✗ UMask= Files … 0.1\n\n\
                   → Overall exposure level for beize-accounts@x.service: 1.7 OK 🙂\n\
                   → Overall exposure level for dbus.service: 9.0 UNSAFE 😨\n";
        let parsed = parse(out);
        assert_eq!(parsed["beize-accounts@x.service"], 1.7);
        assert_eq!(parsed["dbus.service"], 9.0);
        assert_eq!(parsed.len(), 2);
    }
}
