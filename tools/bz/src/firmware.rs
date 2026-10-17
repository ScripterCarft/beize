//! Firmware for the kernel's modules (ADR 0015): exactly the files the
//! modules request (`modinfo --field firmware`), from linux-firmware, laid
//! out as its `WHENCE` describes – a name upstream lists as a `Link:` becomes
//! a relative symlink to the one copy, as `copy-firmware.sh` installs it.
//! Uncompressed: `/usr`'s EROFS compresses them already (zstd), and the
//! kernel need not unpack them before handing them to the device.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::kernel::{self, Kernel};
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::say;
use crate::stamp::{Fingerprint, Stamp};
use crate::tree;
use crate::usr::sysroot_command;
use crate::util;

/// Where the kernel looks for firmware, in the `/usr` tree.
const FIRMWARE_DIR: &str = "usr/lib/firmware";

/// Installs the firmware `kernel`'s modules request into `root`.
pub fn install(paths: &Paths, kernel: &Kernel, root: &Path) -> Result<()> {
    let requested = requested(paths, kernel)?;
    if requested.is_empty() {
        return Ok(());
    }
    let tree = build(paths, &requested)?;
    let dest = root.join(FIRMWARE_DIR);
    fs::create_dir_all(&dest)?;
    util::run(
        Command::new("cp")
            .arg("--recursive")
            .arg("--no-dereference")
            .arg(format!("{}/.", tree.display()))
            .arg(&dest),
    )
}

/// The firmware names all modules request, sorted, each once.
fn requested(paths: &Paths, kernel: &Kernel) -> Result<BTreeSet<String>> {
    let modules = kernel::module_files(&kernel.modules)?;
    let output = util::output(
        sysroot_command(paths, "usr/bin/modinfo")
            .args(["--field", "firmware"])
            .args(&modules),
    )?;
    Ok(output.split_whitespace().map(str::to_owned).collect())
}

/// The firmware tree for `requested`, made once per request list and
/// linux-firmware release: unpacking linux-firmware (632 MB) takes a minute.
fn build(paths: &Paths, requested: &BTreeSet<String>) -> Result<PathBuf> {
    let recipe = Recipe::load(paths, "linux-firmware")?;
    let source = recipe
        .source
        .as_ref()
        .context("linux-firmware has no source")?;
    let out = paths.work.join("firmware");
    let list = requested.iter().cloned().collect::<Vec<_>>().join("\n");
    let stamp = Stamp::new(
        paths,
        "firmware",
        Fingerprint::new()?.text(&recipe.text).text(&list),
        &[&out],
    );
    if stamp.is_current() {
        say!("  firmware is up to date");
        return Ok(out);
    }
    stamp.clear()?;

    util::step("Collecting firmware");
    let archive = source.fetch(paths, false)?;
    let top = source.stem()?;
    let work = tree::recreate(&paths.work.join("firmware-src"))?;
    extract(&archive, &work, &[format!("{top}/WHENCE")])?;
    let links = parse_links(&fs::read_to_string(work.join(&top).join("WHENCE"))?);

    let resolved = requested
        .iter()
        .map(|name| Ok((name.clone(), resolve(&links, name)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let files: BTreeSet<&String> = resolved.values().collect();
    extract(
        &archive,
        &work,
        &files
            .iter()
            .map(|file| format!("{top}/{file}"))
            .collect::<Vec<_>>(),
    )?;

    tree::recreate(&out)?;
    for file in &files {
        let target = out.join(file);
        fs::create_dir_all(target.parent().context("firmware without a directory")?)?;
        fs::copy(work.join(&top).join(file), &target)?;
    }
    for (name, file) in &resolved {
        if name != file {
            let link = out.join(name);
            fs::create_dir_all(link.parent().context("firmware without a directory")?)?;
            symlink(relative(name, file), &link)
                .with_context(|| format!("linking {name} to {file}"))?;
        }
    }
    tree::normalize_mtimes(&out)?;
    fs::remove_dir_all(&work)?;
    say!(
        "  firmware: {} files requested, {} of them links -> {}",
        resolved.len(),
        resolved.len() - files.len(),
        out.display()
    );
    stamp.write()?;
    Ok(out)
}

/// Extracts exactly `members` from `archive` into `dir`; tar fails when one
/// is missing, so a module requesting firmware linux-firmware lacks stops
/// the build.
fn extract(archive: &Path, dir: &Path, members: &[String]) -> Result<()> {
    util::run(
        Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(archive)
            .arg("--directory")
            .arg(dir)
            .args(members),
    )
}

/// The `Link: <name> -> <target>` lines of `WHENCE`, the target relative to
/// the name's directory as a symlink's, resolved to a path from the top.
fn parse_links(whence: &str) -> BTreeMap<String, String> {
    whence
        .lines()
        .filter_map(|line| line.strip_prefix("Link:"))
        .filter_map(|rest| rest.split_once("->"))
        .map(|(name, target)| {
            let name = name.trim();
            let dir = Path::new(name).parent().unwrap_or(Path::new(""));
            let target = tree::lexical(&dir.join(target.trim()));
            (name.to_owned(), target.to_string_lossy().into_owned())
        })
        .collect()
}

/// The file `name` is in the archive: following links, which may name a
/// directory (`nvidia/ga104/gsp -> ../ga102/gsp`) or another link.
fn resolve(links: &BTreeMap<String, String>, name: &str) -> Result<String> {
    let mut path = name.to_owned();
    'follow: for _ in 0..16 {
        let mut prefix = PathBuf::new();
        let parts: Vec<_> = Path::new(&path).components().collect();
        for (at, part) in parts.iter().enumerate() {
            prefix.push(part);
            if let Some(target) = links.get(&*prefix.to_string_lossy()) {
                let mut followed = PathBuf::from(target);
                followed.extend(&parts[at + 1..]);
                path = followed.to_string_lossy().into_owned();
                continue 'follow;
            }
        }
        return Ok(path);
    }
    bail!("{name}: more than 16 links in linux-firmware's WHENCE")
}

/// The symlink at `name` that points to `file`, both from the top.
fn relative(name: &str, file: &str) -> PathBuf {
    let up = Path::new(name)
        .parent()
        .map_or(0, |dir| dir.components().count());
    let mut link: PathBuf = (0..up).map(|_| Component::ParentDir).collect();
    link.push(file);
    link
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHENCE: &str = "Driver: nouveau\n\
                          File: nvidia/ga102/gsp/gsp-570.144.bin\n\
                          Link: nvidia/ga104/gsp -> ../ga102/gsp\n\
                          Link: nvidia/ga104/gr/fecs_bl.bin -> ../../ga102/gr/fecs_bl.bin\n\
                          Link: nvidia/ga107/gsp -> ../ga104/gsp\n";

    #[test]
    fn links_resolve_to_the_one_file() {
        let links = parse_links(WHENCE);
        assert_eq!(links["nvidia/ga104/gsp"], "nvidia/ga102/gsp");
        for (name, file) in [
            (
                "nvidia/ga102/gsp/gsp-570.144.bin",
                "nvidia/ga102/gsp/gsp-570.144.bin",
            ),
            (
                "nvidia/ga104/gsp/gsp-570.144.bin",
                "nvidia/ga102/gsp/gsp-570.144.bin",
            ),
            ("nvidia/ga104/gr/fecs_bl.bin", "nvidia/ga102/gr/fecs_bl.bin"),
            // A link to a link.
            (
                "nvidia/ga107/gsp/gsp-570.144.bin",
                "nvidia/ga102/gsp/gsp-570.144.bin",
            ),
        ] {
            assert_eq!(resolve(&links, name).unwrap(), file, "{name}");
        }
    }

    #[test]
    fn link_loops_end() {
        let links = parse_links("Link: a -> b\nLink: b -> a\n");
        assert!(resolve(&links, "a").is_err());
    }

    #[test]
    fn links_are_relative_to_their_directory() {
        assert_eq!(
            relative("nvidia/ga104/gsp/gsp.bin", "nvidia/ga102/gsp/gsp.bin"),
            Path::new("../../../nvidia/ga102/gsp/gsp.bin")
        );
        assert_eq!(relative("a.bin", "b.bin"), Path::new("b.bin"));
    }
}
