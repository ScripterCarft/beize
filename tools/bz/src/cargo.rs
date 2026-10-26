//! Crates a recipe moves on from its source's Cargo.lock (`[cargo.update]`),
//! as distributions build Rust programs against crates they keep fixed
//! (Fedora's rust-* packages): to a semver-compatible release that fixes
//! an advisory, until the project releases with it. Of a crate the lock
//! has in several versions, the one compatible with the update moves
//! (`cargo update -p name@version`). The source stays as pinned and
//! verified; a copy of it gets the new Cargo.lock, for the build and for
//! `bz cve` alike.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::outdated::compare_versions;
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::tree;
use crate::{say, util};

/// `src`, the unpacked source of `recipe`, or – if the recipe moves crates
/// on – a copy of it with those crates updated in its Cargo.lock.
pub fn source(paths: &Paths, recipe: &Recipe, src: &Path) -> Result<PathBuf> {
    if recipe.cargo.update.is_empty() {
        return Ok(src.to_owned());
    }
    let lock = src.join("Cargo.lock");
    let pinned = versions(&lock)?;
    let mut moved = BTreeMap::new();
    for (name, update) in &recipe.cargo.update {
        let Some(old) = pinned
            .get(name)
            .and_then(|versions| compatible(versions, &update.version))
        else {
            bail!(
                "{}: [cargo.update] {name}: no version compatible with {} in its Cargo.lock",
                recipe.name,
                update.version
            );
        };
        if compare_versions(&old, &update.version) != Ordering::Less {
            bail!(
                "{}: [cargo.update] {name} is obsolete: its source has {old} now; remove it",
                recipe.name
            );
        }
        moved.insert(name, old);
    }

    let copy = paths.work.join("cargo-src").join(&recipe.name);
    tree::remove(&copy)?;
    fs::create_dir_all(copy.parent().context("cargo-src")?)?;
    util::run(Command::new("cp").arg("--archive").arg(src).arg(&copy))?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for (name, update) in &recipe.cargo.update {
        util::run(
            Command::new(&cargo)
                .env("CARGO_HOME", paths.cache.join("cargo"))
                .args(["update", "--quiet", "--manifest-path"])
                .arg(copy.join("Cargo.toml"))
                .args(["--package", &format!("{name}@{}", moved[name])])
                .args(["--precise", &update.version]),
        )
        .with_context(|| format!("{}: updating {name} to {}", recipe.name, update.version))?;
        say!(
            "  {}: {name} {} instead of {} ({})",
            recipe.name,
            update.version,
            moved[name],
            update.why
        );
    }
    let updated = versions(&copy.join("Cargo.lock"))?;
    for (name, update) in &recipe.cargo.update {
        // The update in, the version it replaced gone, the others kept.
        let mut wanted: Vec<String> = pinned[name]
            .iter()
            .filter(|v| **v != moved[name])
            .cloned()
            .chain([update.version.clone()])
            .collect();
        wanted.sort();
        let mut has = updated.get(name).cloned().unwrap_or_default();
        has.sort();
        if has != wanted {
            bail!(
                "{}: Cargo.lock has {name} {:?}, not {}",
                recipe.name,
                updated.get(name),
                update.version
            );
        }
    }
    Ok(copy)
}

/// The version of a crate in a Cargo.lock that an update to `wanted` moves
/// on: the one semver-compatible with it (same major version, or same minor
/// below 1.0), as Cargo resolves a requirement.
fn compatible(versions: &[String], wanted: &str) -> Option<String> {
    let line = |v: &str| {
        let mut parts = v.split('.');
        let major = parts.next().unwrap_or_default().to_owned();
        let minor = parts.next().unwrap_or_default().to_owned();
        if major == "0" {
            (major, minor)
        } else {
            (major, String::new())
        }
    };
    versions.iter().find(|v| line(v) == line(wanted)).cloned()
}

/// Crate name → its versions in a Cargo.lock.
fn versions(lock: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    #[derive(serde::Deserialize)]
    struct Lock {
        package: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        version: String,
    }
    let text = fs::read_to_string(lock).with_context(|| format!("reading {}", lock.display()))?;
    let lock: Lock =
        toml::from_str(&text).with_context(|| format!("parsing {}", lock.display()))?;
    let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in lock.package {
        versions.entry(entry.name).or_default().push(entry.version);
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_moves_the_compatible_version() {
        let pinned = vec!["0.23.40".to_owned()];
        assert_eq!(compatible(&pinned, "0.23.46").as_deref(), Some("0.23.40"));
        // webpki-root-certs in ntpd-rs 1.9.0: the 0.26 wrapper stays.
        let two = vec!["0.26.11".to_owned(), "1.0.1".to_owned()];
        assert_eq!(compatible(&two, "1.0.10").as_deref(), Some("1.0.1"));
        // Another line is no update of it.
        let beyond = vec!["0.21.12".to_owned(), "0.24.0".to_owned()];
        assert_eq!(compatible(&beyond, "0.23.46"), None);
    }
}
