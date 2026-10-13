use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Directory layout used by every build step.
///
/// Everything generated lives under `build/` and can be deleted at any time.
pub struct Paths {
    pub root: PathBuf,
    /// Verified downloads.
    pub cache: PathBuf,
    /// Unpacked sources, build trees, staging and intermediate state.
    pub work: PathBuf,
    /// Cross toolchain (binutils, gcc) running on the build machine.
    pub tools: PathBuf,
    /// Everything built for Beize, including headers and static libraries.
    pub sysroot: PathBuf,
    /// One file list per installed package, recording who owns what.
    pub manifests: PathBuf,
    /// Artifacts other steps consume: kernel, root filesystem, disk image.
    pub out: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .context("locating repository root")?;
        // Builds get paths inside compiler commands (CC="gcc --sysroot=..."),
        // which build systems split at blanks.
        if root.to_string_lossy().contains(char::is_whitespace) {
            bail!("the repository path {} contains blanks", root.display());
        }
        let build = root.join("build");
        let paths = Self {
            cache: build.join("cache"),
            work: build.join("work"),
            tools: build.join("tools"),
            sysroot: build.join("sysroot"),
            manifests: build.join("manifests"),
            out: build.join("out"),
            root,
        };
        for dir in [
            &paths.cache,
            &paths.work,
            &paths.tools,
            &paths.sysroot,
            &paths.manifests,
            &paths.out,
        ] {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(paths)
    }

    pub fn recipe_file(&self, name: &str) -> PathBuf {
        self.root.join("recipes").join(format!("{name}.toml"))
    }
}
