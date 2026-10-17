//! Releases: what beize-updated installs (ADR 0020). One directory per
//! version of a variant: `/usr`, its dm-verity hash tree and the UKI under
//! the names of the variant's stream ([`layout::release_patterns`], ADR
//! 0025), and the list of their SHA-256 sums, signed with the update key.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::image::{self, Target};
use crate::layout;
use crate::paths::Paths;
use crate::secureboot::{self, Keys};
use crate::tree;
use crate::{say, sbom, util};

/// The list of a release's files: `<sha256>  <name>` per line, as
/// `sha256sum` writes it.
pub const LIST: &str = "SHA256SUMS";
/// The update key's signature of [`LIST`].
pub const SIGNATURE: &str = "SHA256SUMS.sig";

/// Where `bz release` puts the variant's release of `version`.
pub fn dir(paths: &Paths, version: &str) -> PathBuf {
    paths.image.join("release").join(version)
}

/// `bz release`: the variant's release of this build's version for real
/// hardware, signed with the owner's keys from `store`. A version is
/// released once.
pub fn release(paths: &Paths, store: &secureboot::Store) -> Result<PathBuf> {
    let version = layout::OS_VERSION;
    let dir = dir(paths, version);
    ensure!(
        !dir.exists(),
        "{} exists: version {version} is released already; raise OS_VERSION \
         (tools/bz/src/layout.rs) for the next release",
        dir.display()
    );
    let keys = Keys::production(store, &secureboot::production_public_dir(paths))?
        .with_update_key(store)?;
    write(paths, &keys, version, Target::Hardware, &dir)?;
    Ok(dir)
}

/// Writes the release of `version` for `target`, signed with `keys`, into
/// `dir` (replacing what is there).
pub fn write(paths: &Paths, keys: &Keys, version: &str, target: Target, dir: &Path) -> Result<()> {
    tree::remove(dir)?;
    let work = PathBuf::from(format!("{}.work", dir.display()));
    tree::recreate(&work)?;
    let chain = image::boot_chain(paths, keys, version, target, &work)?;

    util::step(&format!("Writing release {version}"));
    fs::create_dir_all(dir)?;
    let (data_uuid, verity_uuid) = layout::verity_partition_uuids(&chain.usr.root_hash)?;
    let names = layout::release_patterns(&paths.variant.id);
    let files = [
        (&chain.usr.image, data_uuid.as_str()),
        (&chain.usr.verity, verity_uuid.as_str()),
        (&chain.uki, ""),
    ];
    let mut list = String::new();
    for ((file, uuid), pattern) in files.into_iter().zip(&names) {
        let name = layout::release_file(pattern, version, uuid);
        fs::rename(file, dir.join(&name))?;
        list += &format!("{}  {name}\n", util::sha256_file(&dir.join(&name))?);
    }
    fs::write(dir.join(LIST), list)?;
    keys.sign_update(&dir.join(LIST), &dir.join(SIGNATURE))?;
    // What the release consists of (ADR 0022); beside the signed files, not
    // one of them: beize-update fetches only what the list names.
    sbom::write(paths, version, &dir.join(sbom::FILE))?;
    fs::remove_dir_all(&work)?;
    say!("  release {version} -> {}", dir.display());
    Ok(())
}
