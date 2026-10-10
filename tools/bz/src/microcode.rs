//! CPU microcode for the UKI (ADR 0015).
//!
//! The kernel looks for an uncompressed cpio archive with
//! `kernel/x86/microcode/{GenuineIntel,AuthenticAMD}.bin` in front of the
//! initrd and applies the update for its CPU before anything else runs. ukify
//! puts it into the UKI's `.ucode` section, which systemd-stub hands over
//! first. Each `.bin` is the vendor's update files concatenated; the kernel
//! picks the one for its CPU.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::initrd;
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::say;
use crate::stamp::{Fingerprint, Stamp};
use crate::tree;
use crate::util;

/// Builds `out/microcode.cpio` from the intel-microcode and linux-firmware sources.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let intel = Recipe::load(paths, "intel-microcode")?;
    let firmware = Recipe::load(paths, "linux-firmware")?;
    let archive = paths.out.join("microcode.cpio");
    let stamp = Stamp::new(
        paths,
        "microcode",
        Fingerprint::new()?.text(&intel.text).text(&firmware.text),
        &[&archive],
    );
    if stamp.is_current() {
        say!("  microcode is up to date");
        return Ok(archive);
    }
    stamp.clear()?;

    util::step("Collecting CPU microcode");
    let work = tree::recreate(&paths.work.join("microcode"))?;
    // Intel: intel-ucode/ only; intel-ucode-with-caveats/ needs special
    // handling (as in Arch and Fedora).
    let intel_files = extract(paths, &intel, &work.join("intel"), &["*/intel-ucode/*"])?;
    // AMD: the microcode_amd*.bin containers, not their .asc signatures.
    let amd_files = extract(
        paths,
        &firmware,
        &work.join("amd"),
        &["*/amd-ucode/microcode_amd*.bin"],
    )?;

    let root = work.join("cpio");
    let dir = root.join("kernel/x86/microcode");
    fs::create_dir_all(&dir)?;
    concatenate(&intel_files, &dir.join("GenuineIntel.bin"))?;
    concatenate(&amd_files, &dir.join("AuthenticAMD.bin"))?;
    tree::normalize_mtimes(&root)?;
    initrd::pack_uncompressed(&root, &archive)?;
    say!(
        "  microcode: {} Intel and {} AMD update files -> {}",
        intel_files.len(),
        amd_files.len(),
        archive.display()
    );
    stamp.write()?;
    Ok(archive)
}

/// Extracts the members matching `patterns` from the recipe's verified
/// archive into `dir`; returns the extracted files, sorted.
fn extract(paths: &Paths, recipe: &Recipe, dir: &Path, patterns: &[&str]) -> Result<Vec<PathBuf>> {
    let source = recipe
        .source
        .as_ref()
        .with_context(|| format!("{} has no source", recipe.name))?;
    let archive = source.fetch(paths, false)?;
    fs::create_dir_all(dir)?;
    util::run(
        Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(&archive)
            .arg("--directory")
            .arg(dir)
            .arg("--wildcards")
            .args(patterns),
    )?;
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    ensure!(
        !files.is_empty(),
        "{} has no files matching {patterns:?}",
        recipe.name
    );
    Ok(files)
}

fn concatenate(files: &[PathBuf], dest: &Path) -> Result<()> {
    let mut data = Vec::new();
    for file in files {
        data.extend(fs::read(file)?);
    }
    fs::write(dest, data)?;
    Ok(())
}
