//! Installs staged packages into the sysroot and keeps track of file ownership.
//!
//! Beize uses a merged `/usr`: `/bin`, `/sbin`, `/lib` and `/lib64` are
//! symlinks into `/usr`. Staged files are normalised to their `/usr` path, so
//! every file has exactly one canonical location and exactly one owner.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::say;

/// Merged-/usr symlinks and their targets, relative to the link's directory.
const MERGED_LINKS: [(&str, &str); 6] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/bin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib"),
    ("usr/sbin", "bin"),
    ("usr/lib64", "lib"),
];

/// Where paths below a merged-/usr symlink really live.
const CANONICAL: [(&str, &str); 6] = [
    ("usr/sbin", "usr/bin"),
    ("usr/lib64", "usr/lib"),
    ("bin", "usr/bin"),
    ("sbin", "usr/bin"),
    ("lib64", "usr/lib"),
    ("lib", "usr/lib"),
];

/// Creates the merged-/usr skeleton in an empty or existing root.
pub fn prepare_layout(root: &Path) -> Result<()> {
    for dir in ["usr/bin", "usr/lib", "usr/include", "usr/share"] {
        fs::create_dir_all(root.join(dir))?;
    }
    for (link, target) in MERGED_LINKS {
        let path = root.join(link);
        if path.symlink_metadata().is_err() {
            symlink(target, &path).with_context(|| format!("creating {}", path.display()))?;
        }
    }
    Ok(())
}

/// Maps a path relative to the root onto its canonical merged-/usr location.
pub fn canonical(rel: &Path) -> PathBuf {
    for (from, to) in CANONICAL {
        if let Ok(rest) = rel.strip_prefix(from) {
            return Path::new(to).join(rest);
        }
    }
    rel.to_owned()
}

/// Staged files that are never installed:
/// - the merged-/usr symlinks, which belong to the layout itself,
/// - libtool archives, which hard-code build paths,
/// - the info directory index, which every GNU package regenerates.
fn is_discarded(rel: &Path, target: &Path) -> bool {
    MERGED_LINKS.iter().any(|(link, _)| rel == Path::new(link))
        || rel.extension().is_some_and(|ext| ext == "la")
        || target == Path::new("usr/share/info/dir")
}

/// True if a staged path reproduces an absolute path of the build machine,
/// e.g. `home/joe/beize/build/sysroot/usr/lib/...`.
fn leaks_build_path(target: &Path, repo_root: &Path) -> bool {
    repo_root
        .strip_prefix("/")
        .is_ok_and(|root| target.starts_with(root))
}

/// Moves a package's staged files into the sysroot and records them in its manifest.
/// Fails if another package already owns one of the files.
pub fn merge(paths: &Paths, package: &str, staging: &Path) -> Result<()> {
    let mut files = BTreeMap::new();
    for rel in walk(staging)? {
        let target = canonical(&rel);
        if is_discarded(&rel, &target) {
            continue;
        }
        if leaks_build_path(&target, &paths.root) {
            bail!(
                "{package} installs into the build tree: /{}\n  \
                 Usually a path taken from pkg-config, which prefixes the sysroot \
                 when cross compiling; set the directory explicitly in the recipe.",
                target.display()
            );
        }
        if let Some(previous) = files.insert(target.clone(), rel.clone()) {
            bail!(
                "{package} installs {} twice ({} and {})",
                target.display(),
                previous.display(),
                rel.display()
            );
        }
    }

    let owners = owners(paths)?;
    for target in files.keys() {
        if let Some(owner) = owners.get(target).filter(|owner| *owner != package) {
            bail!(
                "{package} wants to install {}, which belongs to {owner}",
                target.display()
            );
        }
    }

    remove_installed(paths, package)?;
    for (target, rel) in &files {
        copy_entry(&staging.join(rel), &paths.sysroot.join(target))?;
    }

    let manifest: String = files
        .keys()
        .map(|path| format!("{}\n", path.display()))
        .collect();
    fs::write(manifest_path(paths, package), manifest)?;
    say!("  installed {} files", files.len());
    Ok(())
}

/// Files a package installed into the sysroot, from its manifest.
pub fn manifest(paths: &Paths, package: &str) -> Result<Vec<PathBuf>> {
    let path = manifest_path(paths, package);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("{package} is not installed ({})", path.display()))?;
    Ok(text.lines().map(PathBuf::from).collect())
}

/// Names the package that installed `path` (absolute or relative to the root).
pub fn owner(paths: &Paths, path: &str) -> Result<String> {
    let target = canonical(Path::new(path.trim_start_matches('/')));
    owners(paths)?
        .remove(&target)
        .with_context(|| format!("/{} is not owned by any package", target.display()))
}

/// Copies a regular file or symlink, replacing whatever is at `dest`.
pub fn copy_entry(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.symlink_metadata().is_ok() {
        fs::remove_file(dest).with_context(|| format!("replacing {}", dest.display()))?;
    }
    let meta = src.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        symlink(fs::read_link(src)?, dest)?;
    } else {
        fs::copy(src, dest).with_context(|| format!("copying {}", src.display()))?;
    }
    Ok(())
}

/// Fills `dest`, an empty directory, with the files of `packages` as hard
/// links into the sysroot: the sysroot of one package's build, which sees
/// exactly its dependencies (as Yocto's recipe-sysroot). Without this,
/// configure scripts pick up whatever else is installed, and the result
/// depends on build order and history: lvm2 rebuilt after systemd found
/// libudev and wrote a different udev rule than in a clean build, where it
/// comes first. Hard links cost neither space nor time; a package installed
/// later replaces its files with new ones and leaves running builds alone.
pub fn link_packages(paths: &Paths, packages: &BTreeSet<String>, dest: &Path) -> Result<()> {
    prepare_layout(dest)?;
    for package in packages {
        // Toolchain packages install into build/tools and have no manifest.
        if !manifest_path(paths, package).exists() {
            continue;
        }
        for file in manifest(paths, package)? {
            let target = dest.join(&file);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // A symlink is linked itself, not what it points to.
            fs::hard_link(paths.sysroot.join(&file), &target)
                .with_context(|| format!("linking /{} of {package}", file.display()))?;
        }
    }
    Ok(())
}

fn installed(paths: &Paths) -> Result<Vec<String>> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(&paths.manifests)? {
        let path = entry?.path();
        if let Some(package) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".list"))
        {
            packages.push(package.to_owned());
        }
    }
    packages.sort();
    Ok(packages)
}

fn manifest_path(paths: &Paths, package: &str) -> PathBuf {
    paths.manifests.join(format!("{package}.list"))
}

fn owners(paths: &Paths) -> Result<BTreeMap<PathBuf, String>> {
    let mut owners = BTreeMap::new();
    for package in installed(paths)? {
        for file in manifest(paths, &package)? {
            owners.insert(file, package.clone());
        }
    }
    Ok(owners)
}

/// Removes `package` from the sysroot – its files and its manifest – before
/// it is built again: the build sees the sysroot a build from scratch sees,
/// and a failed build leaves nothing of an older one behind.
pub fn uninstall(paths: &Paths, package: &str) -> Result<()> {
    remove_installed(paths, package)?;
    let manifest = manifest_path(paths, package);
    if manifest.exists() {
        fs::remove_file(&manifest)?;
    }
    Ok(())
}

fn remove_installed(paths: &Paths, package: &str) -> Result<()> {
    let Ok(files) = manifest(paths, package) else {
        return Ok(());
    };
    for file in files {
        let path = paths.sysroot.join(file);
        if path.symlink_metadata().is_ok() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(())
}

/// Lists regular files and symlinks below `root`, relative to it.
fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        for entry in fs::read_dir(root.join(&rel))? {
            let entry = entry?;
            let child = rel.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(child);
            } else if kind.is_file() || kind.is_symlink() {
                found.push(child);
            } else {
                bail!("{} is not a file, symlink or directory", child.display());
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_merged_usr_paths() {
        let cases = [
            ("sbin/init", "usr/bin/init"),
            ("bin/bash", "usr/bin/bash"),
            ("lib64/ld-linux-x86-64.so.2", "usr/lib/ld-linux-x86-64.so.2"),
            ("lib/libc.so.6", "usr/lib/libc.so.6"),
            ("usr/sbin/agetty", "usr/bin/agetty"),
            ("usr/lib64/libfoo.so", "usr/lib/libfoo.so"),
            ("usr/lib/systemd/systemd", "usr/lib/systemd/systemd"),
            ("libexec/x", "libexec/x"),
            ("etc/profile", "etc/profile"),
        ];
        for (input, expected) in cases {
            assert_eq!(canonical(Path::new(input)), Path::new(expected), "{input}");
        }
    }

    #[test]
    fn discards_generated_and_layout_files() {
        let discarded = |p: &str| is_discarded(Path::new(p), &canonical(Path::new(p)));
        assert!(discarded("lib64"));
        assert!(discarded("usr/lib/libblkid.la"));
        assert!(discarded("usr/share/info/dir"));
        assert!(!discarded("lib64/ld-linux-x86-64.so.2"));
        assert!(!discarded("usr/share/info/bash.info"));
    }

    #[test]
    fn detects_leaked_build_paths() {
        let root = Path::new("/home/joe/beize");
        let leaked = Path::new("home/joe/beize/build/sysroot/usr/lib/systemd/system/dbus.service");
        assert!(leaks_build_path(leaked, root));
        assert!(!leaks_build_path(
            Path::new("usr/lib/systemd/system/dbus.service"),
            root
        ));
        assert!(!leaks_build_path(Path::new("home/joe/notes"), root));
    }

    fn scratch_paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("bz-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = Paths {
            root: dir.clone(),
            cache: dir.join("cache"),
            work: dir.join("work"),
            tools: dir.join("tools"),
            sysroot: dir.join("sysroot"),
            manifests: dir.join("manifests"),
            out: dir.join("out"),
        };
        fs::create_dir_all(paths.sysroot.join("usr/lib/pkgconfig")).unwrap();
        fs::create_dir_all(paths.sysroot.join("usr/bin")).unwrap();
        fs::create_dir_all(&paths.manifests).unwrap();
        paths
    }

    #[test]
    fn a_build_sees_exactly_its_dependencies() {
        let paths = scratch_paths("link");
        fs::write(paths.sysroot.join("usr/lib/libc.so.6"), "libc").unwrap();
        fs::write(paths.sysroot.join("usr/lib/pkgconfig/libudev.pc"), "").unwrap();
        symlink("libudev.so.1", paths.sysroot.join("usr/lib/libudev.so")).unwrap();
        fs::write(paths.manifests.join("glibc.list"), "usr/lib/libc.so.6\n").unwrap();
        fs::write(
            paths.manifests.join("systemd.list"),
            "usr/lib/libudev.so\nusr/lib/pkgconfig/libudev.pc\n",
        )
        .unwrap();

        let view = paths.work.join("sysroot/lvm2");
        let deps = BTreeSet::from(["gcc".to_owned(), "glibc".to_owned()]);
        link_packages(&paths, &deps, &view).unwrap();
        let seen = walk(&view.join("usr")).unwrap();
        let libc = fs::read_to_string(view.join("usr/lib/libc.so.6")).unwrap();
        let usr_bin = fs::read_link(view.join("bin")).unwrap();

        let with_systemd = paths.work.join("sysroot/dbus");
        let deps = BTreeSet::from(["glibc".to_owned(), "systemd".to_owned()]);
        link_packages(&paths, &deps, &with_systemd).unwrap();
        let link = fs::read_link(with_systemd.join("usr/lib/libudev.so")).unwrap();
        fs::remove_dir_all(&paths.root).unwrap();

        // Besides the merged-/usr links (usr/lib64, usr/sbin) only glibc.
        assert_eq!(seen, ["lib/libc.so.6", "lib64", "sbin"].map(PathBuf::from));
        assert_eq!(libc, "libc");
        assert_eq!(usr_bin, Path::new("usr/bin"), "merged-/usr layout");
        assert_eq!(
            link,
            Path::new("libudev.so.1"),
            "a dangling symlink stays one"
        );
    }

    /// A package built again: its old files are gone, the new ones replace
    /// them, and the sysroot of a build that linked the old files keeps them.
    #[test]
    fn a_rebuilt_package_replaces_its_old_files() {
        let paths = scratch_paths("rebuild");
        fs::write(paths.sysroot.join("usr/bin/ls"), "old").unwrap();
        fs::write(paths.sysroot.join("usr/bin/dir"), "old").unwrap();
        fs::write(
            paths.manifests.join("coreutils.list"),
            "usr/bin/dir\nusr/bin/ls\n",
        )
        .unwrap();
        let running = paths.work.join("sysroot/bash");
        link_packages(&paths, &BTreeSet::from(["coreutils".to_owned()]), &running).unwrap();
        let staging = paths.root.join("staging");
        fs::create_dir_all(staging.join("usr/bin")).unwrap();
        fs::write(staging.join("usr/bin/ls"), "new").unwrap();

        uninstall(&paths, "coreutils").unwrap();
        merge(&paths, "coreutils", &staging).unwrap();
        let ls = fs::read_to_string(paths.sysroot.join("usr/bin/ls")).unwrap();
        let dir_left = paths.sysroot.join("usr/bin/dir").exists();
        let ls_running = fs::read_to_string(running.join("usr/bin/ls")).unwrap();
        fs::remove_dir_all(&paths.root).unwrap();

        assert_eq!(ls, "new");
        assert!(!dir_left, "a file the new build no longer installs stays");
        assert_eq!(ls_running, "old");
    }

    #[test]
    fn walk_lists_files_and_symlinks() {
        let dir = std::env::temp_dir().join(format!("bz-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("usr/bin")).unwrap();
        fs::write(dir.join("usr/bin/bash"), "").unwrap();
        symlink("bash", dir.join("usr/bin/sh")).unwrap();
        fs::create_dir_all(dir.join("empty")).unwrap();

        let files = walk(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            files,
            [PathBuf::from("usr/bin/bash"), PathBuf::from("usr/bin/sh")]
        );
    }
}
