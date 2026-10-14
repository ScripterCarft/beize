//! The digest of the code that decides what bz builds, as `BZ_BUILD_CODE`:
//! the stamps of the build steps take it instead of the whole binary (#27),
//! as Yocto's task signatures cover a task's code, not BitBake's. Modules
//! that only report, measure or test (the VMs, boot times, vulnerability
//! and release checks, scheduling) are left out: changing them redoes no
//! build step. Any other file counts, so a new module is covered from the
//! start – forgetting to list it here costs build time, never a stale step.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Paths below `src/` whose code never changes what a build step produces.
const NOT_BUILD_CODE: [&str; 8] = [
    "vm",
    "boottimes.rs",
    "cve.rs",
    "hwcheck.rs",
    "outdated.rs",
    // Order and parallelism of the builds, not their results.
    "schedule.rs",
    "jobserver.rs",
    "cgroup.rs",
];

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect(&root.join("src"), &root.join("src"), &mut files);
    files.sort();
    // The libraries bz uses and the compiler that builds it.
    for other in [
        "Cargo.toml",
        "../../Cargo.lock",
        "../../rust-toolchain.toml",
    ] {
        files.push(root.join(other));
    }
    let mut hasher = Sha256::new();
    for file in &files {
        let rel = file.strip_prefix(root).unwrap_or(file);
        let content = fs::read(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        hasher.update(format!("{} {}\n", rel.display(), content.len()));
        hasher.update(content);
        println!("cargo::rerun-if-changed={}", file.display());
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rustc-env=BZ_BUILD_CODE={digest}");
}

fn collect(src: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .path();
        let rel = path.strip_prefix(src).unwrap_or(&path);
        if NOT_BUILD_CODE.iter().any(|skip| rel == Path::new(skip)) {
            continue;
        }
        if path.is_dir() {
            collect(src, &path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}
