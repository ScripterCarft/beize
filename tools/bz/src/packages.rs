//! Builds recipes in dependency order, skipping those whose inputs are unchanged.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::install;
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::util;

/// Target triplet of the cross toolchain.
pub const TARGET: &str = "x86_64-beize-linux-gnu";
/// Compiler flags for everything built for Beize. Tuned per CPU in M2.
const TARGET_CFLAGS: &str = "-O2 -pipe";
/// Fixed timestamp handed to builds via SOURCE_DATE_EPOCH (2026-10-06 00:00 UTC).
pub const BUILD_EPOCH: u64 = 1_791_244_800;
/// Bump to force a full rebuild after changing the build environment below.
const ENV_REVISION: u32 = 1;
const HOST_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Builds the given recipes and everything they depend on.
pub fn build(paths: &Paths, names: &[String]) -> Result<Vec<Recipe>> {
    let order = resolve(paths, names)?;
    install::prepare_layout(&paths.sysroot)?;
    write_meson_cross_file(paths)?;

    let stamps = paths.work.join("stamps");
    fs::create_dir_all(&stamps)?;
    let mut fingerprints = BTreeMap::new();
    for recipe in &order {
        let fingerprint = fingerprint(recipe, &fingerprints);
        let stamp = stamps.join(&recipe.name);
        if fs::read_to_string(&stamp).is_ok_and(|s| s == fingerprint) {
            eprintln!("  {} {} is up to date", recipe.name, recipe.version);
        } else {
            let _ = fs::remove_file(&stamp);
            build_one(paths, recipe)?;
            fs::write(&stamp, &fingerprint)?;
        }
        fingerprints.insert(recipe.name.clone(), fingerprint);
    }
    Ok(order)
}

/// Loads the given recipes and their dependencies, dependencies first.
pub fn resolve(paths: &Paths, names: &[String]) -> Result<Vec<Recipe>> {
    let order = topological_order(names, |name| Ok(Recipe::load(paths, name)?.deps))?;
    order.iter().map(|name| Recipe::load(paths, name)).collect()
}

fn topological_order(
    roots: &[String],
    mut deps_of: impl FnMut(&str) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    fn visit(
        name: &str,
        deps_of: &mut dyn FnMut(&str) -> Result<Vec<String>>,
        done: &mut BTreeMap<String, bool>,
        chain: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        match done.get(name) {
            Some(true) => return Ok(()),
            Some(false) => bail!("dependency cycle: {} -> {name}", chain.join(" -> ")),
            None => {}
        }
        done.insert(name.to_owned(), false);
        chain.push(name.to_owned());
        for dep in deps_of(name)? {
            visit(&dep, deps_of, done, chain, order)?;
        }
        chain.pop();
        done.insert(name.to_owned(), true);
        order.push(name.to_owned());
        Ok(())
    }

    let mut done = BTreeMap::new();
    let mut order = Vec::new();
    for root in roots {
        visit(root, &mut deps_of, &mut done, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
}

/// Everything that influences a build: the recipe itself, the build
/// environment and the fingerprints of all dependencies.
fn fingerprint(recipe: &Recipe, known: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "env {ENV_REVISION} {TARGET} {TARGET_CFLAGS} {BUILD_EPOCH}\n"
    ));
    hasher.update(&recipe.text);
    if let Some(source) = &recipe.source {
        hasher.update(&source.sha256);
    }
    for extra in &recipe.extra {
        hasher.update(&extra.source.sha256);
    }
    for dep in &recipe.deps {
        hasher.update(format!(
            "\ndep {dep} {}",
            known.get(dep).map_or("", String::as_str)
        ));
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn build_one(paths: &Paths, recipe: &Recipe) -> Result<()> {
    let stage = recipe
        .stage
        .with_context(|| format!("{} has no stage", recipe.name))?;
    let script = recipe
        .build
        .as_ref()
        .map(|build| build.script.as_str())
        .with_context(|| format!("{} has no build script", recipe.name))?;

    let src = match recipe.source {
        Some(_) => Some(recipe.unpack(paths)?),
        None => None,
    };
    util::step(&format!("Building {} {}", recipe.name, recipe.version));
    let build_dir = recreate(&paths.work.join("build").join(&recipe.name))?;
    let pkg_dir = recreate(&paths.work.join("pkg").join(&recipe.name))?;
    let home = recreate(&paths.work.join("home"))?;
    let logs = paths.work.join("logs");
    fs::create_dir_all(&logs)?;
    let log_path = logs.join(format!("{}.log", recipe.name));
    let log = File::create(&log_path)?;

    let jobs = thread::available_parallelism().map_or(1, |n| n.get());
    let mut cmd = Command::new("bash");
    cmd.args(["-euo", "pipefail", "-c", script])
        .current_dir(&build_dir)
        .env_clear()
        .env(
            "PATH",
            format!("{}:{HOST_PATH}", paths.tools.join("bin").display()),
        )
        .env("HOME", &home)
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", BUILD_EPOCH.to_string())
        .env("NAME", &recipe.name)
        .env("VERSION", &recipe.version)
        .env("BUILD", &build_dir)
        .env("PKG", &pkg_dir)
        .env("TOOLS", &paths.tools)
        .env("SYSROOT", &paths.sysroot)
        .env("TARGET", TARGET)
        .env("JOBS", jobs.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(src) = &src {
        cmd.env("SRC", src);
    }
    match stage {
        Stage::Toolchain => {
            cmd.env("CFLAGS", "-O2 -pipe").env("CXXFLAGS", "-O2 -pipe");
        }
        Stage::Target => {
            cmd.env("CC", format!("{TARGET}-gcc"))
                .env("CXX", format!("{TARGET}-g++"))
                .env("AR", format!("{TARGET}-ar"))
                .env("RANLIB", format!("{TARGET}-ranlib"))
                .env("NM", format!("{TARGET}-nm"))
                .env("STRIP", format!("{TARGET}-strip"))
                .env("CFLAGS", TARGET_CFLAGS)
                .env("CXXFLAGS", TARGET_CFLAGS)
                .env("PKG_CONFIG_SYSROOT_DIR", &paths.sysroot)
                .env(
                    "PKG_CONFIG_LIBDIR",
                    format!(
                        "{0}/usr/lib/pkgconfig:{0}/usr/share/pkgconfig",
                        paths.sysroot.display()
                    ),
                )
                .env("MESON_CROSS", meson_cross_file(paths));
        }
    }

    eprintln!("  log: {}", log_path.display());
    let started = Instant::now();
    let status = cmd.status().context("starting bash")?;
    if !status.success() {
        print_tail(&log_path, 40);
        bail!("building {} failed with {status}", recipe.name);
    }
    eprintln!("  built in {:.0}s", started.elapsed().as_secs_f64());

    if stage == Stage::Target {
        install::merge(paths, &recipe.name, &pkg_dir)?;
    }
    Ok(())
}

fn meson_cross_file(paths: &Paths) -> std::path::PathBuf {
    paths.work.join("meson-cross.ini")
}

fn write_meson_cross_file(paths: &Paths) -> Result<()> {
    let sysroot = paths.sysroot.display();
    let text = format!(
        "[binaries]\n\
         c = '{TARGET}-gcc'\n\
         cpp = '{TARGET}-g++'\n\
         ar = '{TARGET}-ar'\n\
         strip = '{TARGET}-strip'\n\
         pkg-config = 'pkg-config'\n\
         \n\
         [properties]\n\
         sys_root = '{sysroot}'\n\
         pkg_config_libdir = '{sysroot}/usr/lib/pkgconfig:{sysroot}/usr/share/pkgconfig'\n\
         \n\
         [host_machine]\n\
         system = 'linux'\n\
         cpu_family = 'x86_64'\n\
         cpu = 'x86_64'\n\
         endian = 'little'\n"
    );
    fs::write(meson_cross_file(paths), text)?;
    Ok(())
}

fn recreate(dir: &Path) -> Result<std::path::PathBuf> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    fs::create_dir_all(dir)?;
    Ok(dir.to_owned())
}

fn print_tail(log: &Path, lines: usize) {
    let Ok(file) = File::open(log) else { return };
    let all: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
    eprintln!("  last {lines} lines of {}:", log.display());
    for line in &all[all.len().saturating_sub(lines)..] {
        eprintln!("  | {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(graph: &[(&str, &[&str])], roots: &[&str]) -> Result<Vec<String>> {
        let graph: BTreeMap<_, _> = graph.iter().copied().collect();
        let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
        topological_order(&roots, |name| {
            Ok(graph[name].iter().map(|s| s.to_string()).collect())
        })
    }

    #[test]
    fn dependencies_come_first() {
        let result = order(
            &[
                ("systemd", &["glibc", "libcap"]),
                ("libcap", &["glibc"]),
                ("glibc", &["gcc-pass1"]),
                ("gcc-pass1", &[]),
            ],
            &["systemd"],
        )
        .unwrap();
        assert_eq!(result, ["gcc-pass1", "glibc", "libcap", "systemd"]);
    }

    #[test]
    fn shared_dependencies_are_built_once() {
        let result = order(&[("a", &["c"]), ("b", &["c"]), ("c", &[])], &["a", "b"]).unwrap();
        assert_eq!(result, ["c", "a", "b"]);
    }

    #[test]
    fn cycles_are_reported() {
        let err = order(&[("a", &["b"]), ("b", &["a"])], &["a"]).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }
}
