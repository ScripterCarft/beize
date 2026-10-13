//! Beize's buildroot (ADR 0021): the Ubuntu, packages and Rust that bz and
//! every build run with, defined in `ci/buildroot/` and named by a hash of
//! that definition. `./bz` enters it locally; the CI's jobs run in it as
//! their container image.

use std::fs;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::paths::Paths;
use crate::util;

/// Written by `ci/buildroot/create.sh` into the buildroot.
const MARKER: &str = "/etc/beize-buildroot";

static TOOLS: OnceLock<String> = OnceLock::new();

/// What of the buildroot the builds use: its Ubuntu packages
/// (`packages.lock`) and the Rust toolchain. Part of every fingerprint, so a
/// change rebuilds everything; the buildroot's other contents (cargo-deny
/// for the CI's checks) build nothing and leave the builds alone.
pub fn tools() -> Result<&'static str> {
    if cfg!(test) {
        return Ok("test");
    }
    match TOOLS.get() {
        Some(tools) => Ok(tools),
        None => bail!("bz builds only in Beize's buildroot (ADR 0021): start it with ./bz"),
    }
}

/// Fails outside the buildroot and in a buildroot other than the one
/// `ci/buildroot/` defines now.
pub fn check(paths: &Paths) -> Result<()> {
    let Ok(current) = fs::read_to_string(MARKER) else {
        bail!(
            "bz builds only in Beize's buildroot (ADR 0021), where the CI builds too: \
             start it with ./bz"
        );
    };
    let out = Command::new(paths.root.join("ci/buildroot/hash.sh"))
        .output()
        .context("running ci/buildroot/hash.sh")?;
    if !out.status.success() {
        bail!(
            "ci/buildroot/hash.sh failed: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        );
    }
    let wanted = String::from_utf8(out.stdout)?.trim().to_owned();
    let current = current.trim();
    if current != wanted {
        bail!(
            "this is buildroot {current}, but ci/buildroot/ defines {wanted}: \
             leave it and start ./bz again, which creates the new one"
        );
    }
    let mut hasher = Sha256::new();
    for file in ["ci/buildroot/packages.lock", "rust-toolchain.toml"] {
        let path = paths.root.join(file);
        hasher.update(fs::read(&path).with_context(|| format!("reading {}", path.display()))?);
    }
    TOOLS.get_or_init(|| util::hex(&hasher.finalize()));
    Ok(())
}
