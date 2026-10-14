//! Crates a recipe moves on from its source's Cargo.lock (`[cargo.update]`),
//! as distributions build Rust programs against crates they keep fixed
//! (Fedora's rust-* packages): to a semver-compatible release that fixes
//! an advisory, until the project releases with it. The source stays as
//! pinned and verified; a copy of it gets the new Cargo.lock, for the build
//! and for `bz cve` alike.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::outdated::compare_versions;
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::{say, util};

/// `src`, the unpacked source of `recipe`, or – if the recipe moves crates
/// on – a copy of it with those crates updated in its Cargo.lock.
pub fn source(paths: &Paths, recipe: &Recipe, src: &Path) -> Result<PathBuf> {
    if recipe.cargo.update.is_empty() {
        return Ok(src.to_owned());
    }
    let lock = src.join("Cargo.lock");
    let pinned = versions(&lock)?;
    for (name, update) in &recipe.cargo.update {
        let Some(versions) = pinned.get(name) else {
            bail!(
                "{}: [cargo.update] {name}: no such crate in its Cargo.lock",
                recipe.name
            );
        };
        if let Some(newer) = obsolete(versions, &update.version) {
            bail!(
                "{}: [cargo.update] {name} is obsolete: its source has {newer} now; remove it",
                recipe.name
            );
        }
    }

    let copy = paths.work.join("cargo-src").join(&recipe.name);
    if copy.exists() {
        fs::remove_dir_all(&copy)?;
    }
    fs::create_dir_all(copy.parent().context("cargo-src")?)?;
    util::run(Command::new("cp").arg("--archive").arg(src).arg(&copy))?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for (name, update) in &recipe.cargo.update {
        util::run(
            Command::new(&cargo)
                .env("CARGO_HOME", paths.cache.join("cargo"))
                .args(["update", "--quiet", "--manifest-path"])
                .arg(copy.join("Cargo.toml"))
                .args(["--package", name, "--precise", &update.version]),
        )
        .with_context(|| format!("{}: updating {name} to {}", recipe.name, update.version))?;
        say!(
            "  {}: {name} {} instead of {} ({})",
            recipe.name,
            update.version,
            pinned[name].join(", "),
            update.why
        );
    }
    let updated = versions(&copy.join("Cargo.lock"))?;
    for (name, update) in &recipe.cargo.update {
        if updated.get(name) != Some(&vec![update.version.clone()]) {
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

/// A version the source's Cargo.lock has of a crate that is at least the
/// one a recipe asks for: then the update is no longer needed.
fn obsolete(versions: &[String], wanted: &str) -> Option<String> {
    versions
        .iter()
        .find(|v| compare_versions(v, wanted) != Ordering::Less)
        .cloned()
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
    fn an_update_the_source_caught_up_with_is_obsolete() {
        let pinned = vec!["0.23.40".to_owned()];
        assert_eq!(obsolete(&pinned, "0.23.46"), None);
        let caught_up = vec!["0.23.46".to_owned()];
        assert_eq!(obsolete(&caught_up, "0.23.46").as_deref(), Some("0.23.46"));
        let beyond = vec!["0.21.12".to_owned(), "0.24.0".to_owned()];
        assert_eq!(obsolete(&beyond, "0.23.46").as_deref(), Some("0.24.0"));
    }
}
