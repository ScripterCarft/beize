use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::variant::Variant;

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
    /// Artifacts other steps consume and every variant shares: kernel,
    /// modules, initrd.
    pub out: PathBuf,
    /// The variant this run builds or tests (ADR 0025).
    pub variant: Variant,
    /// The variant's artifacts: `/usr`, UKI, disk image, releases.
    pub image: PathBuf,
    /// The variant's intermediate state: its `/usr` tree, its VMs.
    pub variant_work: PathBuf,
}

impl Paths {
    /// The paths of a run for `variant` (a directory of `images/`).
    pub fn new(variant: &str) -> Result<Self> {
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
        let variant = Variant::load(&root, variant)?;
        let paths = Self {
            cache: build.join("cache"),
            work: build.join("work"),
            tools: build.join("tools"),
            sysroot: build.join("sysroot"),
            manifests: build.join("manifests"),
            out: build.join("out"),
            image: build.join("out").join(&variant.id),
            variant_work: build.join("work").join(format!("variant-{}", variant.id)),
            variant,
            root,
        };
        for dir in [
            &paths.cache,
            &paths.work,
            &paths.tools,
            &paths.sysroot,
            &paths.manifests,
            &paths.out,
            &paths.image,
            &paths.variant_work,
        ] {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(paths)
    }

    pub fn recipe_file(&self, name: &str) -> PathBuf {
        self.root.join("recipes").join(format!("{name}.toml"))
    }
}
