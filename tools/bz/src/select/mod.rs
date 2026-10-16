//! Which files the initrd needs (ADR 0016).
//!
//! `images/initrd.toml` names functions; this module follows them to files:
//! units with their drop-ins and `.wants`/`.requires` links, the programs in
//! their `Exec*=` lines, udev rules and the programs they run, script
//! interpreters, and every library a program links (`DT_NEEDED`) or loads
//! with dlopen for a feature a function names (`.note.dlopen`). Files come
//! from the initrd's packages (the pool); Beize's own files (`system/initrd`)
//! are looked at first and followed the same way.

mod udev;
mod units;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use self::udev::rule_programs;
use self::units::{effective, exec_program, template, triggered, unit_settings};
use crate::elf::Linking;
use crate::{install, say, tree, util};

/// The initrd definition, relative to the repository.
pub const DEFINITION: &str = "images/initrd.toml";

const UNIT_DIRS: [&str; 2] = ["etc/systemd/system", "usr/lib/systemd/system"];
const RULES_DIR: &str = "usr/lib/udev/rules.d";
/// Where udev looks for programs named without a path.
const UDEV_PROGRAMS: &str = "usr/lib/udev";
/// Where systemd looks for programs named without a path.
const SEARCH_PATH: &str = "usr/bin";
/// The library directory of the merged /usr.
const LIBRARY_DIR: &str = "usr/lib";
/// Units systemd provides without a unit file.
const BUILTIN_UNITS: [&str; 4] = ["-.slice", "system.slice", "-.mount", "init.scope"];
/// Unit settings whose units must exist: without them systemd fails the job.
const HARD_DEPENDENCIES: [&str; 5] = ["Requires", "Requisite", "BindsTo", "OnFailure", "OnSuccess"];
const EXEC_SETTINGS: [&str; 7] = [
    "ExecCondition",
    "ExecStartPre",
    "ExecStart",
    "ExecStartPost",
    "ExecReload",
    "ExecStop",
    "ExecStopPost",
];

/// `images/initrd.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Packages the files come from; their target dependencies come along.
    pub packages: Vec<String>,
    /// Kernel modules by name; their dependencies come along.
    pub modules: Vec<String>,
    pub functions: BTreeMap<String, Function>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    /// What the function does in the initrd; required, like every reason.
    pub why: String,
    #[serde(default)]
    pub units: Vec<String>,
    /// udev rules by file name.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Further files or directories, relative to the root.
    #[serde(default)]
    pub files: Vec<String>,
    /// dlopen features (`.note.dlopen`) whose libraries come along.
    #[serde(default)]
    pub dlopen: Vec<String>,
    /// Units that generators write at boot; units may require them.
    #[serde(default)]
    pub generated: Vec<String>,
}

/// The files of the initrd's packages, and Beize's own initrd files.
pub struct Pool {
    pub sysroot: PathBuf,
    pub own: PathBuf,
    pub files: BTreeSet<PathBuf>,
    pub readelf: PathBuf,
}

/// Where an entry was found.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Origin {
    Own,
    Package,
}

/// A path followed through its symlinks.
struct Resolved {
    /// Package entries on the way, the final one included.
    entries: Vec<PathBuf>,
    /// The final path, relative to the root.
    path: PathBuf,
    /// Where to read it on the build host.
    source: PathBuf,
}

impl Pool {
    fn locate(&self, rel: &Path) -> Option<(Origin, PathBuf)> {
        let own = self.own.join(rel);
        if own.symlink_metadata().is_ok() {
            return Some((Origin::Own, own));
        }
        // A file of the packages, or a directory with such files below it.
        let packaged = self
            .files
            .range(rel.to_path_buf()..)
            .next()
            .is_some_and(|first| first.starts_with(rel));
        packaged.then(|| (Origin::Package, self.sysroot.join(rel)))
    }

    /// Follows `rel` through symlinks to a file or directory of the pool.
    fn resolve(&self, rel: &Path) -> Option<Resolved> {
        let mut path = install::canonical(&tree::lexical(rel));
        let mut entries = Vec::new();
        for _ in 0..40 {
            let (origin, source) = self.locate(&path)?;
            if origin == Origin::Package && self.files.contains(&path) {
                entries.push(path.clone());
            }
            let meta = source.symlink_metadata().ok()?;
            if !meta.file_type().is_symlink() {
                return Some(Resolved {
                    entries,
                    path,
                    source,
                });
            }
            let target = fs::read_link(&source).ok()?;
            let next = match target.strip_prefix("/") {
                Ok(absolute) => absolute.to_owned(),
                Err(_) => path.parent().unwrap_or(Path::new("")).join(target),
            };
            path = install::canonical(&tree::lexical(&next));
        }
        None
    }

    /// Package files below the directory `dir`.
    fn files_below<'a>(&'a self, dir: &'a Path) -> impl Iterator<Item = &'a PathBuf> {
        self.files
            .range(dir.to_path_buf()..)
            .skip_while(move |f| *f == dir)
            .take_while(move |f| f.starts_with(dir))
    }
}

/// Selects the package files the initrd needs, sorted. Beize's own files are
/// not part of the result: they are copied as they are.
pub fn files(definition: &Definition, pool: &Pool) -> Result<Vec<PathBuf>> {
    let mut selection = Selection {
        pool,
        files: BTreeSet::new(),
        followed: BTreeSet::new(),
        features: BTreeSet::new(),
        noted: BTreeSet::new(),
        inherited: Vec::new(),
    };
    let mut units = Vec::new();
    let mut generated = BTreeSet::new();
    for (name, function) in &definition.functions {
        ensure!(
            !function.why.trim().is_empty(),
            "function {name} in {DEFINITION} does not say why it is there"
        );
        selection.features.extend(function.dlopen.iter().cloned());
        units.extend(function.units.iter().cloned());
        generated.extend(function.generated.iter().cloned());
    }
    units.extend(own_units(&pool.own)?);

    for (name, function) in &definition.functions {
        for rule in &function.rules {
            selection
                .rule(rule)
                .with_context(|| format!("udev rule {rule} of function {name}"))?;
        }
        for file in &function.files {
            selection
                .require(Path::new(file))
                .with_context(|| format!("{file} of function {name}"))?;
        }
    }
    for file in tree::walk(&pool.own)? {
        selection
            .require(&file)
            .with_context(|| format!("Beize's own initrd file /{}", file.display()))?;
    }
    let selected = selection.units(units, &generated)?;

    let unnoted: Vec<_> = selection.features.difference(&selection.noted).collect();
    ensure!(
        unnoted.is_empty(),
        "{DEFINITION} names dlopen features no program in the initrd has: {unnoted:?}"
    );
    say!(
        "  {} functions: {} units, {} files from packages",
        definition.functions.len(),
        selected,
        selection.files.len()
    );
    Ok(selection.files.into_iter().collect())
}

struct Selection<'a> {
    pool: &'a Pool,
    /// Package files to copy.
    files: BTreeSet<PathBuf>,
    /// Files whose own dependencies were followed already.
    followed: BTreeSet<PathBuf>,
    /// dlopen features the functions name.
    features: BTreeSet<String>,
    /// dlopen features seen in the notes of selected programs.
    noted: BTreeSet<String>,
    /// Library directories of the objects that load the one being followed.
    inherited: Vec<PathBuf>,
}

impl Selection<'_> {
    /// Adds `rel` (a file or directory) and what it needs; fails if the pool
    /// does not have it.
    fn require(&mut self, rel: &Path) -> Result<PathBuf> {
        self.add(rel)?
            .with_context(|| format!("/{} is in none of the initrd's packages", rel.display()))
    }

    /// Adds `rel` and what it needs; None if the pool does not have it.
    fn add(&mut self, rel: &Path) -> Result<Option<PathBuf>> {
        let Some(resolved) = self.pool.resolve(rel) else {
            return Ok(None);
        };
        self.files.extend(resolved.entries.iter().cloned());
        if resolved.source.is_dir() {
            let below: Vec<PathBuf> = self.pool.files_below(&resolved.path).cloned().collect();
            for file in below {
                self.add(&file)?;
            }
        } else if self.followed.insert(resolved.path.clone()) {
            self.follow(&resolved)
                .with_context(|| format!("following /{}", resolved.path.display()))?;
        }
        Ok(Some(resolved.path))
    }

    /// Adds what a program needs: its interpreter, libraries and dlopen
    /// libraries of named features.
    fn follow(&mut self, file: &Resolved) -> Result<()> {
        let mut head = [0u8; 4];
        let read = {
            use std::io::Read;
            fs::File::open(&file.source)?.read(&mut head)?
        };
        if read >= 2 && head[..2] == *b"#!" {
            let text = fs::read(&file.source)?;
            let first = String::from_utf8_lossy(text.split(|&b| b == b'\n').next().unwrap_or(&[]))
                .into_owned();
            let interpreter = first[2..].split_whitespace().next().unwrap_or_default();
            ensure!(
                interpreter.starts_with('/') && !interpreter.ends_with("/env"),
                "the script names no absolute interpreter ({first})"
            );
            self.require(Path::new(interpreter.trim_start_matches('/')))?;
            return Ok(());
        }
        if read < 4 || head != *b"\x7fELF" {
            return Ok(());
        }
        let output = util::output(
            Command::new(&self.pool.readelf)
                .args(["--wide", "--program-headers", "--dynamic", "--notes"])
                .arg(&file.source),
        )?;
        let elf = Linking::parse(&output)?;
        if let Some(interpreter) = &elf.interpreter {
            self.require(Path::new(interpreter.trim_start_matches('/')))?;
        }
        let mut dirs = elf.runpath_dirs(&file.path);
        // A library's dependencies may be found through the run path of the
        // program that loads it (libsystemd-core needs libsystemd-shared,
        // which systemd's run path finds): the loader matches loaded objects
        // by soname. Inherit the loading object's directories.
        dirs.extend(self.inherited.iter().cloned());
        dirs.push(PathBuf::from(LIBRARY_DIR));
        dirs.dedup();
        let outer = std::mem::replace(&mut self.inherited, dirs.clone());
        let result = self.libraries(&elf, &dirs);
        self.inherited = outer;
        result
    }

    /// Adds the libraries an ELF file links and the dlopen libraries of named
    /// features, searched in `dirs`.
    fn libraries(&mut self, elf: &Linking, dirs: &[PathBuf]) -> Result<()> {
        for soname in &elf.needed {
            self.library(dirs, std::slice::from_ref(soname))?
                .with_context(|| format!("library {soname} is in none of the initrd's packages"))?;
        }
        for (feature, sonames) in &elf.dlopen {
            self.noted.insert(feature.clone());
            if self.features.contains(feature) {
                self.library(dirs, sonames)?.with_context(|| {
                    format!(
                        "dlopen feature {feature}: none of {sonames:?} is in the initrd's packages"
                    )
                })?;
            }
        }
        Ok(())
    }

    /// Adds the first of `sonames` found in `dirs`.
    fn library(&mut self, dirs: &[PathBuf], sonames: &[String]) -> Result<Option<PathBuf>> {
        for soname in sonames {
            for dir in dirs {
                if let Some(path) = self.add(&dir.join(soname))? {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    /// Adds a udev rule file and the programs it runs.
    fn rule(&mut self, name: &str) -> Result<()> {
        let path = self.require(&Path::new(RULES_DIR).join(name))?;
        let (_, source) = self.pool.locate(&path).context("vanished")?;
        for program in rule_programs(&fs::read_to_string(source)?) {
            let rel = match program.strip_prefix('/') {
                Some(absolute) => PathBuf::from(absolute),
                None => Path::new(UDEV_PROGRAMS).join(&program),
            };
            self.require(&rel)
                .with_context(|| format!("{name} runs {program}"))?;
        }
        Ok(())
    }

    /// Adds `names` and the units they bring along (sockets their services,
    /// paths and timers their units), their drop-ins, programs and the
    /// `.wants`/`.requires` links between selected units. Fails if a selected
    /// unit needs one that is neither selected, generated nor built in.
    /// Returns the number of units.
    fn units(&mut self, names: Vec<String>, generated: &BTreeSet<String>) -> Result<usize> {
        let mut selected = BTreeSet::new();
        let mut needs: Vec<(String, String)> = Vec::new();
        let mut pending = names;
        while let Some(name) = pending.pop() {
            if !selected.insert(name.clone()) {
                continue;
            }
            let settings = self.unit(&name).with_context(|| format!("unit {name}"))?;
            for (key, value) in &settings {
                if HARD_DEPENDENCIES.contains(&key.as_str()) {
                    needs.extend(
                        value
                            .split_whitespace()
                            .filter(|dep| !dep.contains('%'))
                            .map(|dep| (name.clone(), dep.to_owned())),
                    );
                }
            }
            pending.extend(triggered(&name, &settings));
        }

        // Links between selected units, as systemd installed them.
        let links: Vec<PathBuf> = UNIT_DIRS
            .iter()
            .flat_map(|dir| {
                self.pool.files_below(Path::new(dir)).filter(move |file| {
                    file.parent().and_then(Path::parent) == Some(Path::new(dir))
                })
            })
            .cloned()
            .collect();
        for link in links {
            let (Some(from), Some(to)) = (
                link.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str()),
                link.file_name().and_then(|n| n.to_str()),
            ) else {
                continue;
            };
            for kind in [".wants", ".requires"] {
                let Some(unit) = from.strip_suffix(kind) else {
                    continue;
                };
                if !selected.contains(unit) {
                    continue;
                }
                if selected.contains(to) {
                    self.files.insert(link.clone());
                } else if kind == ".requires" {
                    needs.push((unit.to_owned(), to.to_owned()));
                }
            }
        }

        let missing: Vec<String> = needs
            .iter()
            .filter(|(_, dep)| {
                !(selected.contains(dep)
                    || generated.contains(dep)
                    || BUILTIN_UNITS.contains(&dep.as_str())
                    || dep.ends_with(".device")
                    || template(dep).is_some_and(|t| selected.contains(&t)))
            })
            .map(|(unit, dep)| format!("{unit} needs {dep}"))
            .collect();
        ensure!(
            missing.is_empty(),
            "units the initrd needs are missing from {DEFINITION}:\n  {}",
            missing.join("\n  ")
        );
        Ok(selected.len())
    }

    /// Adds one unit (or its template), its drop-ins and programs; returns
    /// its settings.
    fn unit(&mut self, name: &str) -> Result<Vec<(String, String)>> {
        let mut candidates = vec![name.to_owned()];
        candidates.extend(template(name));
        let mut texts = Vec::new();
        let mut found = None;
        'search: for candidate in &candidates {
            for dir in UNIT_DIRS {
                let rel = Path::new(dir).join(candidate);
                if let Some(path) = self.add(&rel)? {
                    found = Some(path);
                    break 'search;
                }
            }
        }
        texts.push(found.with_context(|| format!("no unit file for {name}"))?);
        for candidate in &candidates {
            for dir in UNIT_DIRS {
                let dropins = Path::new(dir).join(format!("{candidate}.d"));
                let mut confs: Vec<PathBuf> = self.pool.files_below(&dropins).cloned().collect();
                if let Ok(own) = fs::read_dir(self.pool.own.join(&dropins)) {
                    for entry in own {
                        confs.push(dropins.join(entry?.file_name()));
                    }
                }
                for conf in confs {
                    texts.push(self.require(&conf)?);
                }
            }
        }
        let mut settings = Vec::new();
        for text in texts {
            let (_, source) = self.pool.locate(&text).context("vanished")?;
            settings.extend(unit_settings(&fs::read_to_string(&source)?));
        }
        let settings = effective(settings);
        for (key, value) in &settings {
            if !EXEC_SETTINGS.contains(&key.as_str()) {
                continue;
            }
            let Some((optional, program)) = exec_program(value) else {
                continue;
            };
            let rel = match program.strip_prefix('/') {
                Some(absolute) => PathBuf::from(absolute),
                None => Path::new(SEARCH_PATH).join(&program),
            };
            if self.add(&rel)?.is_none() && !optional {
                bail!("{key}= runs {program}, which is in none of the initrd's packages");
            }
        }
        Ok(settings)
    }
}

/// Units in Beize's own unit directories and the units their `.wants` and
/// `.requires` links name.
fn own_units(own: &Path) -> Result<Vec<String>> {
    let mut units = Vec::new();
    for dir in UNIT_DIRS {
        let Ok(entries) = fs::read_dir(own.join(dir)) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".wants") || name.ends_with(".requires") {
                for link in fs::read_dir(entry.path())? {
                    units.push(link?.file_name().to_string_lossy().into_owned());
                }
            } else if !name.ends_with(".d") && name.contains('.') {
                units.push(name);
            }
        }
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_initrd_definition_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DEFINITION);
        let def: Definition = crate::tree::read_toml(&path).unwrap();
        assert!(def.packages.contains(&"systemd".to_owned()));
        assert!(def.functions.values().all(|f| !f.why.is_empty()));
        assert!(def.modules.contains(&"nvme".to_owned()));
    }
}
