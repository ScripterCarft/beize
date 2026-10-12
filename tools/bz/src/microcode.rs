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

/// The archive [`build`] writes. In `work/` beside its stamp, like the
/// kernel's object tree: an intermediate the build state keeps, so CI need
/// not unpack linux-firmware (632 MB) for 0.3 MB of it on every run.
pub fn archive(paths: &Paths) -> PathBuf {
    paths.work.join("microcode.cpio")
}

/// Builds [`archive`] from the intel-microcode and linux-firmware sources.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let intel = Recipe::load(paths, "intel-microcode")?;
    let firmware = Recipe::load(paths, "linux-firmware")?;
    let archive = archive(paths);
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
    let mut intel_files = extract(paths, &intel, &work.join("intel"), &["*/intel-ucode/*"])?;
    intel_files.retain(|file| intel_runs_beize(&file_name(file)));
    // AMD: the microcode_amd*.bin containers, not their .asc signatures.
    let mut amd_files = extract(
        paths,
        &firmware,
        &work.join("amd"),
        &["*/amd-ucode/microcode_amd*.bin"],
    )?;
    amd_files.retain(|file| amd_runs_beize(&file_name(file)));

    let root = work.join("cpio");
    let dir = root.join("kernel/x86/microcode");
    fs::create_dir_all(&dir)?;
    concatenate(&intel_files, &dir.join("GenuineIntel.bin"))?;
    concatenate(&amd_files, &dir.join("AuthenticAMD.bin"))?;
    tree::normalize_mtimes(&root)?;
    initrd::pack_uncompressed(&root, &archive)?;
    fs::remove_dir_all(&work)?;
    say!(
        "  microcode: {} Intel and {} AMD update files -> {}",
        intel_files.len(),
        amd_files.len(),
        archive.display()
    );
    stamp.write()?;
    Ok(archive)
}

/// Intel family 6 models Beize leaves out (file names `06-<model>-<stepping>`):
/// servers (Xeon, server Atoms), and Atoms and Lakefield without AVX2, which
/// cannot run x86-64-v3 (ADR 0014). Everything before model 0x3c (Haswell)
/// and family 0x0f (Pentium 4) predates x86-64-v3 as well. Future models stay
/// in: new CPUs get their updates without a change here.
const INTEL_LEFT_OUT: [u8; 21] = [
    // Servers: Haswell-EP, Broadwell-EP/DE, Skylake/Cascade/Cooper Lake-SP,
    // Ice Lake-SP/D, Sapphire/Emerald Rapids, Granite Rapids, Sierra Forest,
    // and the server Atoms Avoton, Denverton, Snow Ridge.
    0x3f, 0x4f, 0x56, 0x55, 0x6a, 0x6c, 0x8f, 0xcf, 0xad, 0xae, 0xaf, 0x4d, 0x5f, 0x86,
    // Without AVX2: Airmont, Goldmont, Goldmont Plus, Lakefield, Elkhart
    // Lake, Jasper Lake, Silvermont tablets (Merrifield/Moorefield).
    0x4c, 0x5c, 0x7a, 0x8a, 0x96, 0x9c, 0x5a,
];

/// Whether Beize ships the Intel update file `name` (`ff-mm-ss`, hex).
fn intel_runs_beize(name: &str) -> bool {
    let mut parts = name.split('-').map(|part| u8::from_str_radix(part, 16));
    match (parts.next(), parts.next()) {
        (Some(Ok(0x06)), Some(Ok(model))) => model >= 0x3c && !INTEL_LEFT_OUT.contains(&model),
        // Family 0x0f is the Pentium 4; families after 6 are new CPUs.
        (Some(Ok(family)), Some(Ok(_))) => family != 0x0f && family > 0x06,
        _ => false,
    }
}

/// Whether Beize ships the AMD container `name`: family 15h (Excavator has
/// x86-64-v3) and the Zen families 17h, 19h, 1ah. Not the families 10h–14h
/// (`microcode_amd.bin`) nor 16h (Jaguar, no AVX2). AMD does not split
/// desktop and server (EPYC) parts, and all of it is 0.3 MB.
fn amd_runs_beize(name: &str) -> bool {
    name.strip_prefix("microcode_amd_fam")
        .and_then(|rest| rest.strip_suffix("h.bin"))
        .is_some_and(|family| family != "16")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ships_microcode_for_the_cpus_beize_runs_on() {
        // The owner's i7-13700K (Raptor Lake), Haswell, Alder Lake-N, a future family.
        for name in ["06-b7-01", "06-3c-03", "06-be-00", "13-01-00"] {
            assert!(intel_runs_beize(name), "{name}");
        }
        // Sapphire Rapids, Granite Rapids, Ivy Bridge, Goldmont, Pentium 4.
        for name in [
            "06-8f-08", "06-ad-01", "06-3a-09", "06-5c-09", "0f-04-0a", "README",
        ] {
            assert!(!intel_runs_beize(name), "{name}");
        }
        assert!(amd_runs_beize("microcode_amd_fam19h.bin"));
        assert!(amd_runs_beize("microcode_amd_fam1ah.bin"));
        assert!(!amd_runs_beize("microcode_amd_fam16h.bin"));
        assert!(!amd_runs_beize("microcode_amd.bin"));
    }
}
