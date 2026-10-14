//! Builds recipes in dependency order, skipping those whose inputs are
//! unchanged; independent ones side by side.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::jobserver::{self, Jobserver};
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::say;
use crate::schedule::{self, Measured, Memory};
use crate::{buildroot, cgroup, debuginfo, install, stamp, util};

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
/// 3: not its own earlier build either.
/// 4: each package builds against a sysroot of its own (--sysroot).
/// 5: pkg-config variables without the sysroot.
/// 6: pkg-config for the target as pkgconf with a personality; the build
///    host's for tools that run while building.
/// 7: the repository's path mapped to /beize in what the compilers write
///    ([`prefix_map`]).
/// 8: what a package installs is stripped, with MiniDebugInfo (ADR 0023).
const ENV_REVISION: u32 = 8;
/// pkg-config for the target, as autoconf looks for it when cross-compiling.
const TARGET_PKG_CONFIG: &str = "x86_64-beize-linux-gnu-pkg-config";
const HOST_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Builds the given recipes and everything they depend on.
pub fn build(paths: &Paths, names: &[String]) -> Result<Vec<Recipe>> {
    let order = resolve(paths, names)?;
    install::prepare_layout(&paths.sysroot)?;

    let stamps = paths.work.join("stamps");
    fs::create_dir_all(&stamps)?;
    let environment = environment(buildroot::tools()?);
    let mut fingerprints = BTreeMap::new();
    let mut closures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut planned = Vec::new();
    for recipe in &order {
        // Everything the package depends on, directly or not: all its build
        // sees. Not its own earlier build either: a rebuild sees what a build
        // from scratch sees (glibc's configure once passed only because its
        // old libc was there).
        let mut visible = BTreeSet::new();
        for dep in &recipe.deps {
            visible.extend(closures[dep].iter().cloned());
        }

        let fingerprint = fingerprint(
            &environment,
            recipe,
            &repo_digest(paths, recipe)?,
            &fingerprints,
        );
        if fs::read_to_string(stamps.join(&recipe.name)).is_ok_and(|s| s == fingerprint) {
            if util::verbose() {
                say!("  {} {} is up to date", recipe.name, recipe.version);
            }
        } else {
            planned.push(Planned {
                recipe,
                fingerprint: fingerprint.clone(),
                visible: visible.clone(),
            });
        }
        fingerprints.insert(recipe.name.clone(), fingerprint);
        visible.insert(recipe.name.clone());
        closures.insert(recipe.name.clone(), visible);
    }
    let up_to_date = order.len() - planned.len();
    if up_to_date > 0 {
        say!("  {up_to_date} of {} packages up to date", order.len());
    }
    if !planned.is_empty() {
        build_planned(paths, &planned)?;
    }
    sweep(paths)?;
    Ok(order)
}

/// A package to build in this run.
struct Planned<'a> {
    recipe: &'a Recipe,
    fingerprint: String,
    /// Everything it depends on, directly or not.
    visible: BTreeSet<String>,
}

/// A build running beside others.
struct Running<'a> {
    recipe: &'a Recipe,
    /// What it needed last time, MiB.
    need_mib: u64,
    /// The anonymous memory it holds now, bytes (0 without a cgroup).
    holds: Arc<AtomicU64>,
}

/// Builds `planned` (in dependency order), as many side by side as CPUs and
/// memory allow ([`schedule`]). After a build fails no new one starts; its
/// error comes once the running ones are done.
fn build_planned(paths: &Paths, planned: &[Planned]) -> Result<()> {
    let jobs = jobserver::shared(paths)?;
    let names: BTreeSet<&str> = planned.iter().map(|p| p.recipe.name.as_str()).collect();
    let deps: BTreeMap<String, Vec<String>> = planned
        .iter()
        .map(|p| {
            let within = p
                .recipe
                .deps
                .iter()
                .filter(|dep| names.contains(dep.as_str()));
            (p.recipe.name.clone(), within.cloned().collect())
        })
        .collect();
    let chain = schedule::chain_secs_of(paths, &deps);
    let mut waiting: Vec<&Planned> = planned.iter().collect();
    // Stable: equally long chains keep the dependency order.
    waiting.sort_by_key(|p| std::cmp::Reverse(chain[&p.recipe.name]));
    let limit_mib = schedule::limit_mib();
    say!(
        "  {} packages to build, {} jobs at a time{}",
        planned.len(),
        jobs.size(),
        if cgroup::available() {
            format!(", each in a cgroup of at most {limit_mib} MiB")
        } else {
            String::new()
        }
    );

    let (done, results) = mpsc::channel();
    let mut running: BTreeMap<String, Running> = BTreeMap::new();
    let mut finished: BTreeSet<String> = BTreeSet::new();
    let mut failed = None;
    thread::scope(|scope| -> Result<()> {
        loop {
            while failed.is_none() && running.len() < jobs.size() {
                let Some(index) = waiting
                    .iter()
                    .position(|p| deps[&p.recipe.name].iter().all(|d| finished.contains(d)))
                else {
                    break;
                };
                let next = waiting[index];
                let name = &next.recipe.name;
                let need_mib = schedule::measured(paths, name)
                    .unwrap_or_default()
                    .memory_mib();
                let pending_mib = running
                    .values()
                    .map(|r| {
                        r.need_mib
                            .saturating_sub(r.holds.load(Ordering::Relaxed) >> 20)
                    })
                    .sum();
                // The first build always starts: nothing else can make room.
                if !running.is_empty()
                    && Memory::now().is_some_and(|m| !m.admits(need_mib, pending_mib))
                {
                    break;
                }
                waiting.remove(index);

                let _ = fs::remove_file(paths.work.join("stamps").join(name));
                install::uninstall(paths, name)?;
                let sysroot = recreate(&sysroot_dir(paths, name))?;
                install::link_packages(paths, &next.visible, &sysroot)?;
                let holds = Arc::new(AtomicU64::new(0));
                running.insert(
                    name.clone(),
                    Running {
                        recipe: next.recipe,
                        need_mib,
                        holds: Arc::clone(&holds),
                    },
                );
                let done = done.clone();
                scope.spawn(move || {
                    let result = jobs.acquire().and_then(|token| {
                        let built =
                            build_one(paths, next.recipe, &sysroot, jobs, limit_mib, &holds);
                        drop(token);
                        built
                    });
                    let _ = done.send((next, result));
                });
            }
            if running.is_empty() {
                break;
            }
            let (built, result): (&Planned, _) = match results.recv_timeout(Duration::from_secs(1))
            {
                Ok(message) => message,
                // Memory may have become free for the next build.
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => unreachable!("the sender is held here"),
            };
            let name = &built.recipe.name;
            running.remove(name);
            match result {
                Ok(measured) => {
                    if built.recipe.stage == Some(Stage::Target) {
                        install::merge(paths, name, &paths.work.join("pkg").join(name))?;
                    }
                    fs::write(paths.work.join("stamps").join(name), &built.fingerprint)?;
                    schedule::remember(paths, name, measured)?;
                    finished.insert(name.clone());
                    let still_needed = waiting.iter().map(|p| p.recipe);
                    clean_up(
                        paths,
                        built.recipe,
                        still_needed.chain(running.values().map(|r| r.recipe)),
                    )?;
                }
                Err(error) => {
                    failed.get_or_insert(error);
                }
            }
        }
        Ok(())
    })?;
    failed.map_or(Ok(()), Err)
}

/// Removes what a successful build leaves in `work/`, as rpmbuild does: the
/// build and staging trees, its sysroot and home always (the files are in
/// the sysroot now), the unpacked sources unless a package still to be built
/// or the kernel (built incrementally from its tree) still needs them.
/// Otherwise a full build holds every package's trees at once – more than
/// the ~14 GB a CI runner has free. A failed build keeps them for
/// debugging, until [`sweep`].
fn clean_up<'a>(
    paths: &Paths,
    recipe: &Recipe,
    still_needed: impl Iterator<Item = &'a Recipe>,
) -> Result<()> {
    for dir in ["build", "pkg", "home"] {
        remove(&paths.work.join(dir).join(&recipe.name))?;
    }
    remove(&sysroot_dir(paths, &recipe.name))?;
    for machine in ["cross", "native"] {
        remove(&meson_file(paths, &recipe.name, machine))?;
    }
    remove(&pkg_config_dir(paths, &recipe.name))?;
    let Some(source) = &recipe.source else {
        return Ok(());
    };
    let stem = source.stem()?;
    let kernel = Recipe::load(paths, "linux")?;
    let still_needed: Vec<&Recipe> = still_needed.collect();
    for other in still_needed.into_iter().chain([&kernel]) {
        if let Some(other) = &other.source
            && other.stem()? == stem
        {
            return Ok(());
        }
    }
    remove(&paths.work.join("src").join(stem))
}

/// After all packages built: removes the trees of earlier failed builds and
/// the sources of versions no recipe uses any more – only the kernel's
/// sources stay.
fn sweep(paths: &Paths) -> Result<()> {
    let kernel = Recipe::load(paths, "linux")?;
    let keep = kernel
        .source
        .as_ref()
        .context("linux has no source")?
        .stem()?;
    for dir in ["src", "build", "pkg", "home", "sysroot"] {
        let Ok(entries) = fs::read_dir(paths.work.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if dir != "src" || path.file_name().is_none_or(|name| *name != *keep) {
                remove(&path)?;
            }
        }
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    let removed = match path.symlink_metadata() {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => return Ok(()),
    };
    removed.with_context(|| format!("removing {}", path.display()))
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

/// The build environment every fingerprint starts with: bz's own settings
/// and the buildroot the build runs in.
fn environment(buildroot: &str) -> String {
    format!(
        "env {ENV_REVISION} {TARGET} {TARGET_CFLAGS} {TARGET_LDFLAGS} {TARGET_RUSTFLAGS} \
         {BUILD_EPOCH} buildroot {buildroot}\n"
    )
}

/// Everything that influences a build: the recipe itself, the build
/// environment, the repository content it builds from and the fingerprints
/// of all dependencies.
fn fingerprint(
    environment: &str,
    recipe: &Recipe,
    repo: &str,
    known: &BTreeMap<String, String>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(environment);
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
    util::hex(&hasher.finalize())
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
    Ok(util::hex(&hasher.finalize()))
}

/// The sysroot of one package's build: its dependencies, nothing else.
fn sysroot_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.work.join("sysroot").join(name)
}

/// Where the repository lies on the build machine (/home/joe/beize locally,
/// another path in the CI) must not show in what is built: assert() and
/// panic messages name their source file. The compilers write /beize
/// instead, as reproducible-builds.org recommends and Debian's
/// dpkg-buildflags does (-ffile-prefix-map; rustc's --remap-path-prefix).
/// `$PREFIX_MAP` gives it to recipes that set their own CFLAGS.
fn prefix_map(paths: &Paths) -> String {
    format!("{}=/beize", paths.root.display())
}

/// Cargo for building Beize's own Rust code for the target: the toolchain
/// running bz (rust-toolchain.toml), crates cached under build/cache.
fn cargo_env(cmd: &mut Command, paths: &Paths, build_dir: &Path, sysroot: &Path) {
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
            format!(
                "-C linker={TARGET}-gcc -C link-arg=--sysroot={} {TARGET_RUSTFLAGS} \
                 --remap-path-prefix={}",
                sysroot.display(),
                prefix_map(paths)
            ),
        );
    if let Some(rustc) = cargo
        .parent()
        .map(|bin| bin.join("rustc"))
        .filter(|rustc| rustc.exists())
    {
        cmd.env("RUSTC", rustc);
    }
}

/// Builds one package against `sysroot` into its staging tree, in a cgroup
/// of at most `limit_mib` where there is one; keeps the anonymous memory it
/// holds in `holds`. The cross compiler has no usable sysroot of its own
/// (recipes/gcc.toml): a build that calls it without `$CC` fails instead of
/// seeing other packages.
fn build_one(
    paths: &Paths,
    recipe: &Recipe,
    sysroot: &Path,
    jobs: &Jobserver,
    limit_mib: u64,
    holds: &AtomicU64,
) -> Result<Measured> {
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
    let home = recreate(&paths.work.join("home").join(&recipe.name))?;
    let log = paths.work.join("logs").join(format!("{}.log", recipe.name));

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
        .env("SYSROOT", sysroot)
        .env("TARGET", TARGET)
        // MAKEFLAGS for the steps that may run jobs in parallel: make,
        // meson compile (ninja), cmake --build and cargo then take their
        // jobs from the shared pool. Everything else runs one job at a time,
        // as before: installing in parallel races in some packages (lvm2
        // linked dmsetup twice at once) – Fedora's %make_install is serial.
        .env("JOBSERVER", jobs.makeflags());
    if let Some(src) = &src {
        cmd.env("SRC", src);
    }
    let file_prefix_map = format!("-ffile-prefix-map={}", prefix_map(paths));
    let target_cflags = format!("{TARGET_CFLAGS} {file_prefix_map}");
    cmd.env("PREFIX_MAP", &file_prefix_map);
    match stage {
        Stage::Toolchain => {
            cmd.env("CFLAGS", "-O2 -pipe").env("CXXFLAGS", "-O2 -pipe");
        }
        Stage::Target => {
            let pkg_config_bin = write_pkg_config(paths, &recipe.name, sysroot)?;
            let meson_cross =
                write_meson_cross_file(paths, &recipe.name, sysroot, &pkg_config_bin)?;
            cmd.env(
                "CC",
                format!("{TARGET}-gcc --sysroot={}", sysroot.display()),
            )
            .env(
                "CXX",
                format!("{TARGET}-g++ --sysroot={}", sysroot.display()),
            )
            .env("AR", format!("{TARGET}-ar"))
            .env("RANLIB", format!("{TARGET}-ranlib"))
            .env("NM", format!("{TARGET}-nm"))
            .env("STRIP", format!("{TARGET}-strip"))
            .env("CFLAGS", &target_cflags)
            .env("CXXFLAGS", &target_cflags)
            .env("LDFLAGS", TARGET_LDFLAGS)
            // pkg-config for the target comes first: a build that calls
            // plain pkg-config gets the target's answer, never the host's.
            .env(
                "PATH",
                format!(
                    "{}:{}:{HOST_PATH}",
                    pkg_config_bin.display(),
                    paths.tools.join("bin").display()
                ),
            )
            .env("PKG_CONFIG", pkg_config_bin.join(TARGET_PKG_CONFIG))
            .env("MESON_CROSS", meson_cross)
            .env(
                "MESON_NATIVE",
                write_meson_native_file(paths, &recipe.name)?,
            );
            cargo_env(&mut cmd, paths, &build_dir, sysroot);
        }
    }

    let in_cgroup = cgroup::available();
    let unit = format!("bz-{}-{}", recipe.name, std::process::id());
    let mut cmd = if in_cgroup {
        cgroup::wrap(&cmd, &unit, limit_mib)
    } else {
        cmd
    };
    cmd.stdin(Stdio::null());
    let mut peak = cgroup::Usage::default();
    let started = Instant::now();
    let ran = util::run_logged_watched(&mut cmd, &recipe.name, &log, |pid| {
        if let Some(now) = cgroup::usage(pid, &unit) {
            holds.store(now.anon, Ordering::Relaxed);
            peak.anon = peak.anon.max(now.anon);
            peak.oom_kills = now.oom_kills;
        }
    });
    if let Err(error) = ran {
        if peak.oom_kills > 0 {
            bail!(
                "{error:#}\n  {} needed more than its limit of {limit_mib} MiB",
                recipe.name
            );
        }
        return Err(error);
    }
    // As rpmbuild runs find-debuginfo on what a package installed (ADR 0023).
    if stage == Stage::Target {
        debuginfo::prepare(paths, &pkg_dir)?;
    }
    let secs = started.elapsed().as_secs();
    say!("  {} built in {secs}s", recipe.name);
    Ok(Measured {
        anon_mib: (peak.anon > 0).then(|| peak.anon.div_ceil(1 << 20)),
        secs,
    })
}

fn meson_file(paths: &Paths, name: &str, machine: &str) -> PathBuf {
    paths
        .work
        .join("sysroot")
        .join(format!("{name}.meson-{machine}.ini"))
}

/// Where a package's pkg-config for the target lives.
fn pkg_config_dir(paths: &Paths, name: &str) -> PathBuf {
    paths.work.join("sysroot").join(format!("{name}.pkgconf"))
}

/// pkg-config for the target, as Debian, Yocto and Buildroot have it: pkgconf
/// with a personality (sysroot and search paths of this package's sysroot),
/// called as `x86_64-beize-linux-gnu-pkg-config` – what autoconf looks for
/// when cross-compiling – and as `pkg-config`. The build host's pkg-config
/// stays as it is, for tools that run while building. Returns the directory
/// of the two commands.
fn write_pkg_config(paths: &Paths, name: &str, sysroot: &Path) -> Result<PathBuf> {
    let dir = recreate(&pkg_config_dir(paths, name))?;
    let personality = dir.join(format!("{TARGET}.personality"));
    fs::write(&personality, personality_text(sysroot))?;
    // The FDO rule puts the sysroot only in front of -I and -L, as
    // freedesktop's pkg-config does; pkgconf would put it in front of
    // variables too, which name places on the target (unit directories,
    // xkeyboard-config's data), and builds wrote it into /usr.
    let script = format!(
        "#!/bin/sh\n\
         PKG_CONFIG_FDO_SYSROOT_RULES=1 exec /usr/bin/pkgconf --personality={} \"$@\"\n",
        personality.display()
    );
    let bin = dir.join("bin");
    fs::create_dir(&bin)?;
    for command in [TARGET_PKG_CONFIG, "pkg-config"] {
        let file = bin.join(command);
        fs::write(&file, &script)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
    }
    Ok(bin)
}

fn personality_text(sysroot: &Path) -> String {
    let sysroot = sysroot.display();
    format!(
        "Triplet: {TARGET}\n\
         SysrootDir: {sysroot}\n\
         DefaultSearchPaths: {sysroot}/usr/lib/pkgconfig:{sysroot}/usr/share/pkgconfig\n\
         SystemIncludePaths: /usr/include\n\
         SystemLibraryPaths: /usr/lib\n\
         WantDefaultStatic: false\n\
         WantDefaultPure: true\n"
    )
}

/// Meson's file for the build machine: tools that run while building
/// (`dependency(…, native: true)`, glib-mkenums for polkit) come from the
/// build host, as in Debian's cross builds – with the host's pkg-config,
/// named by its path, since `pkg-config` in PATH is the target's.
fn write_meson_native_file(paths: &Paths, name: &str) -> Result<PathBuf> {
    let file = meson_file(paths, name, "native");
    fs::write(&file, "[binaries]\npkg-config = '/usr/bin/pkg-config'\n")?;
    Ok(file)
}

fn write_meson_cross_file(
    paths: &Paths,
    name: &str,
    sysroot: &Path,
    pkg_config_bin: &Path,
) -> Result<PathBuf> {
    let sysroot = sysroot.display();
    let pkg_config = pkg_config_bin.join(TARGET_PKG_CONFIG);
    let pkg_config = pkg_config.display();
    let text = format!(
        "[binaries]\n\
         c = ['{TARGET}-gcc', '--sysroot={sysroot}']\n\
         cpp = ['{TARGET}-g++', '--sysroot={sysroot}']\n\
         ar = '{TARGET}-ar'\n\
         strip = '{TARGET}-strip'\n\
         pkg-config = '{pkg_config}'\n\
         \n\
         [host_machine]\n\
         system = 'linux'\n\
         cpu_family = 'x86_64'\n\
         cpu = 'x86_64'\n\
         endian = 'little'\n"
    );
    let file = meson_file(paths, name, "cross");
    fs::write(&file, text)?;
    Ok(file)
}

fn recreate(dir: &Path) -> Result<PathBuf> {
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
        let environment = environment("0123456789abcdef");
        let mut legacy = Sha256::new();
        legacy.update(&environment);
        legacy.update(&recipe.text);
        let legacy = util::hex(&legacy.finalize());
        assert_eq!(
            fingerprint(&environment, &recipe, &digest, &BTreeMap::new()),
            legacy
        );
    }

    /// The target's pkg-config searches only the package's sysroot, and
    /// treats the sysroot's /usr/include and /usr/lib as the system's (no
    /// -I/-L for them).
    #[test]
    fn personality_names_only_the_sysroot() {
        let text = personality_text(Path::new("/b/sysroots/zlib"));
        assert!(text.contains(&format!("Triplet: {TARGET}\n")));
        assert!(text.contains("SysrootDir: /b/sysroots/zlib\n"));
        assert!(text.contains(
            "DefaultSearchPaths: /b/sysroots/zlib/usr/lib/pkgconfig:/b/sysroots/zlib/usr/share/pkgconfig\n"
        ));
        assert!(text.contains("SystemIncludePaths: /usr/include\n"));
        assert!(text.contains("SystemLibraryPaths: /usr/lib\n"));
        assert!(TARGET_PKG_CONFIG.starts_with(TARGET));
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
