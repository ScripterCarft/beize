//! Builds recipes in dependency order, skipping those whose inputs are unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::install;
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::say;
use crate::{stamp, util};

/// Target triplet of the cross toolchain.
pub const TARGET: &str = "x86_64-beize-linux-gnu";
/// Compiler flags for everything built for Beize (ADR 0014): Fedora's
/// hardening and frame pointers. x86-64-v3 is the compiler's default
/// (`--with-arch`), so it also holds where a package overrides CFLAGS.
const TARGET_CFLAGS: &str = "-O2 -pipe -Wall -Werror=format-security \
     -Wp,-U_FORTIFY_SOURCE,-D_FORTIFY_SOURCE=3 -Wp,-D_GLIBCXX_ASSERTIONS \
     -fexceptions -fasynchronous-unwind-tables -fstack-protector-strong \
     -fstack-clash-protection -fcf-protection \
     -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer";
/// Linker flags for everything built for Beize (ADR 0014).
const TARGET_LDFLAGS: &str = "-Wl,-z,relro -Wl,-z,now -Wl,--as-needed -Wl,-z,pack-relative-relocs";
/// rustc flags for Beize's own Rust code and Rust packages (ADR 0014); PIE,
/// RELRO and BIND_NOW are rustc's defaults.
const TARGET_RUSTFLAGS: &str = "-C target-cpu=x86-64-v3 -C force-frame-pointers=yes";
/// Fixed timestamp handed to builds via SOURCE_DATE_EPOCH (2026-10-06 00:00 UTC).
pub const BUILD_EPOCH: u64 = 1_791_244_800;
/// Bump to force a full rebuild after changing the build environment below.
/// 2: each package sees only its dependencies in the sysroot.
const ENV_REVISION: u32 = 2;
const HOST_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Builds the given recipes and everything they depend on.
pub fn build(paths: &Paths, names: &[String]) -> Result<Vec<Recipe>> {
    let order = resolve(paths, names)?;
    install::unhide_all(paths)?;
    install::prepare_layout(&paths.sysroot)?;
    write_meson_cross_file(paths)?;

    let stamps = paths.work.join("stamps");
    fs::create_dir_all(&stamps)?;
    let mut fingerprints = BTreeMap::new();
    let mut closures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut up_to_date = 0;
    for (index, recipe) in order.iter().enumerate() {
        // The package itself and everything it depends on, directly or not.
        let mut visible = BTreeSet::from([recipe.name.clone()]);
        for dep in &recipe.deps {
            visible.extend(closures[dep].iter().cloned());
        }

        let fingerprint = fingerprint(recipe, &repo_digest(paths, recipe)?, &fingerprints);
        let stamp = stamps.join(&recipe.name);
        if fs::read_to_string(&stamp).is_ok_and(|s| s == fingerprint) {
            up_to_date += 1;
            if util::verbose() {
                say!("  {} {} is up to date", recipe.name, recipe.version);
            }
        } else {
            let _ = fs::remove_file(&stamp);
            install::hide_others(paths, &visible)?;
            let built = build_one(paths, recipe);
            install::unhide_all(paths)?;
            built?;
            fs::write(&stamp, &fingerprint)?;
            clean_up(paths, recipe, &order[index + 1..])?;
        }
        fingerprints.insert(recipe.name.clone(), fingerprint);
        closures.insert(recipe.name.clone(), visible);
    }
    if up_to_date > 0 {
        say!("  {up_to_date} of {} packages up to date", order.len());
    }
    Ok(order)
}

/// Removes what a successful build leaves in `work/`, as rpmbuild does: the
/// build tree always, the unpacked sources unless a later package or the
/// kernel (built incrementally from its tree) still needs them. Otherwise a
/// full build holds every package's trees at once – more than the ~14 GB a
/// CI runner has free. A failed build keeps both for debugging.
fn clean_up(paths: &Paths, recipe: &Recipe, later: &[Recipe]) -> Result<()> {
    remove(&paths.work.join("build").join(&recipe.name))?;
    let Some(source) = &recipe.source else {
        return Ok(());
    };
    let stem = source.stem()?;
    let kernel = Recipe::load(paths, "linux")?;
    for other in later.iter().chain([&kernel]) {
        if let Some(other) = &other.source
            && other.stem()? == stem
        {
            return Ok(());
        }
    }
    remove(&paths.work.join("src").join(stem))
}

fn remove(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// The fingerprint a package was last built with: what a step that uses
/// the package puts into its own fingerprint.
pub fn built_fingerprint(paths: &Paths, name: &str) -> Result<String> {
    fs::read_to_string(paths.work.join("stamps").join(name))
        .with_context(|| format!("{name} has not been built"))
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

/// The build environment every fingerprint starts with.
fn environment() -> String {
    format!(
        "env {ENV_REVISION} {TARGET} {TARGET_CFLAGS} {TARGET_LDFLAGS} {TARGET_RUSTFLAGS} \
         {BUILD_EPOCH}\n"
    )
}

/// Everything that influences a build: the recipe itself, the build
/// environment, the repository content it builds from and the fingerprints
/// of all dependencies.
fn fingerprint(recipe: &Recipe, repo: &str, known: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(environment());
    hasher.update(&recipe.text);
    hasher.update(repo);
    for source in recipe.sources() {
        hasher.update(source.pin());
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

/// Digest over the paths and contents of the repository files a recipe
/// builds from (`repo = [...]`); empty for recipes without any, so their
/// fingerprints stay what they were before `repo` existed.
fn repo_digest(paths: &Paths, recipe: &Recipe) -> Result<String> {
    if recipe.repo.is_empty() {
        return Ok(String::new());
    }
    let mut hasher = Sha256::new();
    for entry in &recipe.repo {
        stamp::hash_tree(&mut hasher, &paths.root, &paths.root.join(entry))
            .with_context(|| format!("{}: repo path {entry}", recipe.name))?;
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Cargo for building Beize's own Rust code for the target: the toolchain
/// running bz (rust-toolchain.toml), crates cached under build/cache.
fn cargo_env(cmd: &mut Command, paths: &Paths, build_dir: &Path) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let cargo = Path::new(&cargo);
    cmd.env("CARGO", cargo)
        .env("CARGO_HOME", paths.cache.join("cargo"))
        .env("CARGO_TARGET_DIR", build_dir.join("target"))
        .env("REPO", &paths.root)
        // Only target artifacts link with the cross compiler against the
        // sysroot; build scripts and proc macros stay with the host's cc.
        .env(
            "RUSTFLAGS",
            format!("-C linker={TARGET}-gcc {TARGET_RUSTFLAGS}"),
        );
    if let Some(rustc) = cargo
        .parent()
        .map(|bin| bin.join("rustc"))
        .filter(|rustc| rustc.exists())
    {
        cmd.env("RUSTC", rustc);
    }
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
    let log = paths.work.join("logs").join(format!("{}.log", recipe.name));

    let jobs = util::job_count();
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
        .stdin(Stdio::null());
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
                .env("LDFLAGS", TARGET_LDFLAGS)
                .env("PKG_CONFIG_SYSROOT_DIR", &paths.sysroot)
                .env(
                    "PKG_CONFIG_LIBDIR",
                    format!(
                        "{0}/usr/lib/pkgconfig:{0}/usr/share/pkgconfig",
                        paths.sysroot.display()
                    ),
                )
                .env("MESON_CROSS", meson_cross_file(paths));
            cargo_env(&mut cmd, paths, &build_dir);
        }
    }

    let started = Instant::now();
    util::run_logged(&mut cmd, &recipe.name, &log)?;
    say!("  built in {:.0}s", started.elapsed().as_secs_f64());

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
    remove(dir)?;
    fs::create_dir_all(dir)?;
    Ok(dir.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recipes without `repo` must keep their fingerprints: anything else
    /// rebuilds the whole system (and invalidates every CI cache).
    #[test]
    fn recipes_without_repo_sources_keep_their_fingerprint() {
        let paths = Paths::new().unwrap();
        let recipe = Recipe::parse("name = \"a\"\nversion = \"1\"\n").unwrap();
        let digest = repo_digest(&paths, &recipe).unwrap();
        assert_eq!(digest, "");
        let mut legacy = Sha256::new();
        legacy.update(environment());
        legacy.update(&recipe.text);
        let legacy: String = legacy
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(fingerprint(&recipe, &digest, &BTreeMap::new()), legacy);
    }

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
