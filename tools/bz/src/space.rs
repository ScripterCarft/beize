//! What bz keeps on disk: a working tree holds what the current state
//! needs, so it does not grow with every upgrade (WSL's disk never shrinks
//! by itself; CLAUDE.md §3). At each build, sources no recipe names leave
//! the cache, releases but each variant's newest leave `build/out`, and
//! Cargo's target directory starts over when the Rust toolchain changed or
//! it outgrew `TARGET_BUDGET` – Cargo itself never lets go of what earlier
//! builds left there (it reached 28 GB once, 25.10.).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::outdated::compare_versions;
use crate::paths::Paths;
use crate::recipe::{self, Recipe};
use crate::say;
use crate::tree;

/// How large Cargo's builds of the workspace may grow before they start
/// over (a few minutes to build again): twice what a healthy tree holds.
const TARGET_BUDGET: u64 = 10 << 30;

/// Directories of the cache that are no source's (Cargo's crates, the
/// buildroot's packages); `git` holds the sources' clones.
const CACHE_DIRS: [&str; 3] = ["cargo", "buildroot-packages", "git"];

/// Lets go of what no current build needs, and says how much.
pub fn tidy(paths: &Paths) -> Result<()> {
    let freed = cache(paths)? + releases(paths)? + target(paths)?;
    if freed > 0 {
        say!(
            "  space: let go of {:.1} GiB no build needs any more",
            freed as f64 / (1u64 << 30) as f64
        );
    }
    Ok(())
}

/// Removes from the cache what no recipe's source is.
fn cache(paths: &Paths) -> Result<u64> {
    let mut wanted = Vec::new();
    for name in recipe::all_names(paths)? {
        let recipe = Recipe::load(paths, &name)?;
        for source in recipe.sources() {
            wanted.extend(source.cache_entries(paths)?);
        }
    }
    let mut freed = 0;
    for dir in [paths.cache.clone(), paths.cache.join("git")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            let kept = dir == paths.cache && name.is_some_and(|n| CACHE_DIRS.contains(&n.as_str()));
            if !kept && !wanted.contains(&path) {
                freed += remove(&path)?;
            }
        }
    }
    Ok(freed)
}

/// Removes each variant's releases but its newest.
fn releases(paths: &Paths) -> Result<u64> {
    let mut freed = 0;
    for variant in fs::read_dir(&paths.out)? {
        let Ok(entries) = fs::read_dir(variant?.path().join("release")) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        versions.sort_by(|a, b| {
            compare_versions(
                &a.file_name().unwrap_or_default().to_string_lossy(),
                &b.file_name().unwrap_or_default().to_string_lossy(),
            )
        });
        versions.pop();
        for old in versions {
            freed += remove(&old)?;
        }
    }
    Ok(freed)
}

/// Starts Cargo's builds of the workspace over when the toolchain changed
/// or they outgrew `TARGET_BUDGET` – not bz's own build, which runs.
fn target(paths: &Paths) -> Result<u64> {
    let target = paths.root.join("target");
    let toolchain = fs::read_to_string(paths.root.join("rust-toolchain.toml"))
        .context("reading rust-toolchain.toml")?;
    let mark = target.join(".beize-toolchain");
    let builds = [
        target.join("debug"),
        target.join("release"),
        target.join("buildroot/debug"),
    ];
    let size: u64 = builds.iter().map(|d| size(d)).sum();
    let same = fs::read_to_string(&mark).is_ok_and(|m| m == toolchain);
    let mut freed = 0;
    if !same || size > TARGET_BUDGET {
        for dir in &builds {
            freed += remove(dir)?;
        }
    }
    if !same {
        fs::create_dir_all(&target)?;
        fs::write(&mark, toolchain)?;
    }
    Ok(freed)
}

/// Removes `path`, a file or a directory; how many bytes it held.
fn remove(path: &Path) -> Result<u64> {
    let bytes = size(path);
    tree::remove(path)?;
    Ok(bytes)
}

/// The bytes the files under `path` hold (0 if it is missing).
fn size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path).map_or(0, |entries| {
        entries.flatten().map(|e| size(&e.path())).sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_weighs_its_files() {
        let root = std::env::temp_dir().join(format!("bz-space-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/one"), [0; 10]).unwrap();
        fs::write(root.join("a/b/two"), [0; 5]).unwrap();
        assert_eq!(size(&root), 15);
        assert_eq!(remove(&root.join("a")).unwrap(), 15);
        assert_eq!(size(&root.join("a")), 0);
        fs::remove_dir_all(&root).unwrap();
    }
}
