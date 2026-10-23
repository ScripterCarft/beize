//! `bz outdated`: which pinned sources have newer upstream releases.
//!
//! Each source names where its releases are announced (`latest` in the
//! recipe, see [`Latest`]): release-monitoring.org for nearly all – the
//! service Fedora tracks its packages with –, kernel.org's own list for the
//! kernel (it has the point releases first), and the current file's digest
//! where upstream publishes no versions (curl's CA bundle).

use std::cmp::Ordering;
use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::paths::Paths;
use crate::recipe::{self, Recipe};
use crate::source::{Latest, Source};
use crate::util;

pub fn show(paths: &Paths) -> Result<()> {
    let mut newer = Vec::new();
    let mut current = 0;
    for name in recipe::all_names(paths)? {
        let text = fs::read_to_string(paths.recipe_file(&name))?;
        // Recipes sharing another's source (gcc-pass1, linux-headers) follow it.
        let recipe = Recipe::parse(&text).with_context(|| format!("parsing {name}"))?;
        let mut sources: Vec<(String, String, &Source)> = Vec::new();
        if let Some(source) = &recipe.source {
            sources.push((name.clone(), recipe.version.clone(), source));
        }
        for extra in &recipe.extra {
            let stem = extra.source.stem()?;
            // Part of the same release (e.g. tzcode with tzdata), or pinned
            // by it (Mesa's crates).
            if matches!(extra.source.latest, Some(Latest::PinnedBy(_)))
                || extra.source.latest.is_none() && stem.contains(&recipe.version)
            {
                continue;
            }
            let (extra_name, version) = stem
                .rsplit_once('-')
                .with_context(|| format!("{name}: no version in {stem}"))?;
            sources.push((
                format!("{name}/{extra_name}"),
                version.to_owned(),
                &extra.source,
            ));
        }
        for (label, version, source) in sources {
            match check(source, &version).with_context(|| format!("checking {label}"))? {
                Some(news) => newer.push(format!("  {label} {version}: {news}")),
                None => current += 1,
            }
        }
    }
    if newer.is_empty() {
        println!("All {current} sources are at upstream's newest release.");
    } else {
        println!("Newer upstream releases:");
        for line in &newer {
            println!("{line}");
        }
        println!("{current} other sources are current.");
    }
    Ok(())
}

/// What upstream has that is newer than `version`, if anything.
fn check(source: &Source, version: &str) -> Result<Option<String>> {
    let Some(latest) = &source.latest else {
        bail!("the recipe does not say where releases are announced (`latest`)");
    };
    Ok(match latest {
        Latest::ReleaseMonitoring(id) => {
            let json = fetch_json(&format!(
                "https://release-monitoring.org/api/v2/versions/?project_id={id}"
            ))?;
            let newest = json["stable_versions"]
                .as_array()
                .and_then(|versions| versions.first())
                .and_then(Value::as_str)
                .context("release-monitoring.org lists no stable version")?;
            (compare_versions(newest, version) == Ordering::Greater).then(|| newest.to_owned())
        }
        Latest::KernelOrg(moniker) => {
            let json = fetch_json("https://www.kernel.org/releases.json")?;
            let newest = json["releases"]
                .as_array()
                .context("kernel.org lists no releases")?
                .iter()
                .filter(|release| release["moniker"] == moniker.as_str())
                .filter_map(|release| release["version"].as_str())
                .max_by(|a, b| compare_versions(a, b))
                .with_context(|| format!("kernel.org lists no {moniker} release"))?;
            (compare_versions(newest, version) == Ordering::Greater).then(|| newest.to_owned())
        }
        Latest::Npm(package) => {
            // The registry's `latest` tag: what `npm install` takes.
            let json = fetch_json(&format!("https://registry.npmjs.org/{package}/latest"))?;
            let newest = json["version"]
                .as_str()
                .with_context(|| format!("npm names no version of {package}"))?;
            (compare_versions(newest, version) == Ordering::Greater).then(|| newest.to_owned())
        }
        Latest::File(url) => {
            let file = std::env::temp_dir().join(format!("bz-latest-{}", std::process::id()));
            util::run(
                Command::new("curl")
                    .args(["--silent", "--show-error", "--fail", "--location"])
                    .args(["--max-time", "60", "--output"])
                    .arg(&file)
                    .arg(url),
            )?;
            let digest = util::sha256_file(&file);
            let _ = fs::remove_file(&file);
            (digest? != source.pin()).then(|| format!("{url} has changed since the pin"))
        }
        // Moves with the source that pins it (skipped above).
        Latest::PinnedBy(_) => None,
    })
}

fn fetch_json(url: &str) -> Result<Value> {
    let text = util::output(
        Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--max-time", "60"])
            .arg(url),
    )?;
    serde_json::from_str(&text).with_context(|| format!("parsing {url}"))
}

/// Orders versions by their runs of digits (numerically) and letters:
/// `34.2` > `34`, `2026e` > `2026d`, `1.10` > `1.9`.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a, b) = (version_parts(a), version_parts(b));
    for (x, y) in a.iter().zip(&b) {
        let digits = |part: &str| part.starts_with(|c: char| c.is_ascii_digit());
        let order = if digits(x) && digits(y) {
            let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        } else {
            x.cmp(y)
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// `2026e-1.10` -> `["2026", "e", "1", "10"]`.
fn version_parts(version: &str) -> Vec<&str> {
    let separator = |c: char| !c.is_ascii_alphanumeric();
    let mut parts = Vec::new();
    let mut rest = version.trim_start_matches(separator);
    while !rest.is_empty() {
        let digits = rest.starts_with(|c: char| c.is_ascii_digit());
        let end = rest
            .find(|c: char| separator(c) || c.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        parts.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(separator);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_versions_like_upstream_numbers_them() {
        assert_eq!(compare_versions("34.2", "34"), Ordering::Greater);
        assert_eq!(compare_versions("1.10", "1.9"), Ordering::Greater);
        assert_eq!(compare_versions("2026e", "2026d"), Ordering::Greater);
        assert_eq!(compare_versions("7.2.9", "7.2.10"), Ordering::Less);
        assert_eq!(compare_versions("20260925", "20260916"), Ordering::Greater);
        assert_eq!(compare_versions("1.5.7", "1.5.7"), Ordering::Equal);
        assert_eq!(compare_versions("2026-09-25", "2026-10-02"), Ordering::Less);
    }
}
