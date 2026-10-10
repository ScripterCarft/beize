//! Which files the initrd needs (ADR 0016).
//!
//! `images/initrd.toml` names functions; this module follows them to files:
//! units with their drop-ins and `.wants`/`.requires` links, the programs in
//! their `Exec*=` lines, udev rules and the programs they run, script
//! interpreters, and every library a program links (`DT_NEEDED`) or loads
//! with dlopen for a feature a function names (`.note.dlopen`). Files come
//! from the initrd's packages (the pool); Beize's own files (`system/initrd`)
//! are looked at first and followed the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::install;
use crate::say;
use crate::util;

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
        let mut path = install::canonical(&normalize(rel));
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
            path = install::canonical(&normalize(&next));
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

/// Resolves `.` and `..` lexically (paths relative to the root).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            _ => {}
        }
    }
    out
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
    for file in walk(&pool.own)? {
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
        let elf = ElfNeeds::parse(&output)?;
        if let Some(interpreter) = &elf.interpreter {
            self.require(Path::new(interpreter.trim_start_matches('/')))?;
        }
        let origin = file.path.parent().unwrap_or(Path::new(""));
        let mut dirs: Vec<PathBuf> = elf
            .runpath
            .iter()
            .map(|dir| {
                let dir = dir.replace("$ORIGIN", &format!("/{}", origin.display()));
                PathBuf::from(dir.trim_start_matches('/'))
            })
            .collect();
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
    fn libraries(&mut self, elf: &ElfNeeds, dirs: &[PathBuf]) -> Result<()> {
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

/// The template of an instance name: `foo@bar.service` → `foo@.service`.
fn template(name: &str) -> Option<String> {
    let (prefix, rest) = name.split_once('@')?;
    let (instance, suffix) = rest.rsplit_once('.')?;
    (!instance.is_empty()).then(|| format!("{prefix}@.{suffix}"))
}

/// Units a socket, path or timer unit starts.
fn triggered(name: &str, settings: &[(String, String)]) -> Vec<String> {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return Vec::new();
    };
    let setting = |key: &str| {
        settings
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    match kind {
        "socket" => {
            if let Some(service) = setting("Service") {
                vec![service]
            } else if setting("Accept").is_some_and(|v| v == "yes" || v == "true") {
                vec![format!("{stem}@.service")]
            } else {
                vec![format!("{stem}.service")]
            }
        }
        "path" | "timer" => vec![setting("Unit").unwrap_or_else(|| format!("{stem}.service"))],
        _ => Vec::new(),
    }
}

/// The `key=value` lines of a unit file, continuation lines joined.
fn unit_settings(text: &str) -> Vec<(String, String)> {
    let mut settings = Vec::new();
    let mut line = String::new();
    for raw in text.lines() {
        let raw = raw.trim();
        if line.is_empty() && (raw.starts_with('#') || raw.starts_with(';')) {
            continue;
        }
        if let Some(part) = raw.strip_suffix('\\') {
            line.push_str(part);
            line.push(' ');
            continue;
        }
        line.push_str(raw);
        if let Some((key, value)) = line.split_once('=') {
            settings.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        line.clear();
    }
    settings
}

/// Settings of a unit file and its drop-ins, in order, as systemd applies
/// them: an empty value (`ExecStart=`) drops what was set before.
fn effective(settings: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut kept: Vec<(String, String)> = Vec::new();
    for (key, value) in settings {
        if value.is_empty() {
            kept.retain(|(k, _)| *k != key);
        } else {
            kept.push((key, value));
        }
    }
    kept
}

/// The program of an `Exec*=` value and whether a failure is ignored (`-`).
fn exec_program(value: &str) -> Option<(bool, String)> {
    let mut optional = false;
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        match c {
            '-' => optional = true,
            '@' | ':' | '+' | '!' | '|' => {}
            _ => break,
        }
        rest = &rest[1..];
    }
    let program = rest.split_whitespace().next()?;
    Some((optional, program.to_owned()))
}

/// Programs a udev rules file runs (`PROGRAM`, `IMPORT{program}`, `RUN`),
/// not builtins and not programs named by a variable at run time
/// (`$env{REMOVE_CMD}`).
fn rule_programs(text: &str) -> Vec<String> {
    let mut programs = Vec::new();
    let mut line = String::new();
    for raw in text.lines() {
        if let Some(part) = raw.strip_suffix('\\') {
            line.push_str(part);
            continue;
        }
        line.push_str(raw);
        if !line.trim_start().starts_with('#') {
            for (key, value) in rule_pairs(&line) {
                let runs = matches!(
                    key.as_str(),
                    "PROGRAM" | "IMPORT{program}" | "RUN" | "RUN{program}"
                );
                let program = value.split_whitespace().next();
                if let Some(program) = program.filter(|p| runs && !p.starts_with(['$', '%'])) {
                    programs.push(program.to_owned());
                }
            }
        }
        line.clear();
    }
    programs.sort();
    programs.dedup();
    programs
}

/// The `KEY op "value"` pairs of one udev rule line.
fn rule_pairs(line: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == ',').is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| !"=!+-:".contains(*c) && !c.is_whitespace()) {
            key.push(c);
        }
        if key.is_empty() {
            break;
        }
        while chars
            .next_if(|c| "=!+-:".contains(*c) || c.is_whitespace())
            .is_some()
        {}
        if chars.next() != Some('"') {
            break;
        }
        let mut value = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => value.extend(chars.next()),
                '"' => break,
                _ => value.push(c),
            }
        }
        pairs.push((key, value));
    }
    pairs
}

/// What an ELF file needs at run time, from readelf.
#[derive(Debug, Default, PartialEq)]
struct ElfNeeds {
    interpreter: Option<String>,
    needed: Vec<String>,
    runpath: Vec<String>,
    /// dlopen features and the sonames that provide them, in preference order.
    dlopen: Vec<(String, Vec<String>)>,
}

#[derive(Deserialize)]
struct DlopenNote {
    feature: String,
    soname: Vec<String>,
}

impl ElfNeeds {
    /// Parses `readelf --wide --program-headers --dynamic --notes`.
    fn parse(output: &str) -> Result<Self> {
        let mut needs = Self::default();
        let bracketed = |line: &str, marker: &str| {
            let start = line.find(marker)? + marker.len();
            let end = line[start..].find(']')? + start;
            Some(line[start..end].to_owned())
        };
        for line in output.lines() {
            if let Some(path) = bracketed(line, "[Requesting program interpreter: ") {
                needs.interpreter = Some(path);
            } else if line.contains("(NEEDED)") {
                needs.needed.extend(bracketed(line, "Shared library: ["));
            } else if line.contains("(RUNPATH)") || line.contains("(RPATH)") {
                let dirs = bracketed(line, "runpath: [").or_else(|| bracketed(line, "rpath: ["));
                needs
                    .runpath
                    .extend(dirs.iter().flat_map(|d| d.split(':')).map(str::to_owned));
            } else if let Some(at) = line.find("Dlopen Metadata: ") {
                let json = &line[at + "Dlopen Metadata: ".len()..];
                let notes: Vec<DlopenNote> = serde_json::from_str(json.trim())
                    .with_context(|| format!("parsing the dlopen note {json}"))?;
                needs
                    .dlopen
                    .extend(notes.into_iter().map(|n| (n.feature, n.soname)));
            }
        }
        Ok(needs)
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

/// Every file and symlink below `root`, relative to it.
fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(dir) = pending.pop() {
        for entry in
            fs::read_dir(root.join(&dir)).with_context(|| format!("reading {}", dir.display()))?
        {
            let entry = entry?;
            let rel = dir.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                pending.push(rel);
            } else {
                files.push(rel);
            }
        }
    }
    files.sort();
    Ok(files)
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

    #[test]
    fn reads_unit_settings() {
        let text = "[Unit]\n# Requires=commented.service\nRequires=a.service \\\n  b.service\n\
                    [Service]\nExecStart=-/usr/bin/foo --bar\nExecStart=\n";
        assert_eq!(
            unit_settings(text),
            [
                ("Requires".to_owned(), "a.service  b.service".to_owned()),
                ("ExecStart".to_owned(), "-/usr/bin/foo --bar".to_owned()),
                ("ExecStart".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn an_empty_value_resets_a_setting() {
        let unit = unit_settings(
            "ExecStartPre=-plymouth\nExecStart=-/usr/lib/systemd/systemd-sulogin-shell\n",
        );
        let dropin = unit_settings(
            "ExecStartPre=\nExecStart=\nExecStart=/usr/lib/beize/beize-tpm emergency\n",
        );
        assert_eq!(
            effective([unit, dropin].concat()),
            [(
                "ExecStart".to_owned(),
                "/usr/lib/beize/beize-tpm emergency".to_owned()
            )]
        );
    }

    #[test]
    fn finds_the_program_of_exec_lines() {
        assert_eq!(
            exec_program("-/usr/lib/systemd/systemd-sulogin-shell emergency"),
            Some((true, "/usr/lib/systemd/systemd-sulogin-shell".to_owned()))
        );
        assert_eq!(
            exec_program("!!@/usr/bin/foo foo"),
            Some((false, "/usr/bin/foo".to_owned()))
        );
        assert_eq!(
            exec_program("udevadm info --cleanup-db"),
            Some((false, "udevadm".to_owned()))
        );
        assert_eq!(exec_program(""), None);
    }

    #[test]
    fn knows_templates_and_triggered_units() {
        assert_eq!(
            template("systemd-cryptsetup@root.service").as_deref(),
            Some("systemd-cryptsetup@.service")
        );
        assert_eq!(template("systemd-pcrextend@.service"), None);
        assert_eq!(template("initrd.target"), None);
        let accept = vec![("Accept".to_owned(), "yes".to_owned())];
        assert_eq!(
            triggered("systemd-pcrextend.socket", &accept),
            ["systemd-pcrextend@.service"]
        );
        assert_eq!(
            triggered("systemd-journald.socket", &[]),
            ["systemd-journald.service"]
        );
        let service = vec![("Service".to_owned(), "x.service".to_owned())];
        assert_eq!(triggered("y.socket", &service), ["x.service"]);
        assert_eq!(
            triggered("systemd-ask-password-console.path", &[]),
            ["systemd-ask-password-console.service"]
        );
        assert!(triggered("initrd.target", &[]).is_empty());
    }

    #[test]
    fn finds_the_programs_of_udev_rules() {
        let rules = r#"
# PROGRAM="commented"
KERNEL=="sd*", IMPORT{program}="ata_id --export $devnode", ENV{X}="1"
SUBSYSTEM=="block", IMPORT{builtin}="blkid", RUN{builtin}+="kmod load"
ACTION=="add", RUN+="/usr/bin/foo \"quoted\"", \
  PROGRAM=="scsi_id -g"
ENV{MODALIAS}=="?*", RUN{program}+="bar"
ENV{REMOVE_CMD}!="", RUN+="$env{REMOVE_CMD}"
"#;
        assert_eq!(
            rule_programs(rules),
            ["/usr/bin/foo", "ata_id", "bar", "scsi_id"]
        );
    }

    #[test]
    fn parses_what_an_elf_file_needs() {
        let output = r#"
      [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]
 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-shared-262.so]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x000000000000001d (RUNPATH)            Library runpath: [/usr/lib/systemd]
    Dlopen Metadata: [{"feature":"libcrypto","description":"x","priority":"suggested","soname":["libcrypto.so.4","libcrypto.so.3"]}]
"#;
        assert_eq!(
            ElfNeeds::parse(output).unwrap(),
            ElfNeeds {
                interpreter: Some("/lib64/ld-linux-x86-64.so.2".to_owned()),
                needed: vec![
                    "libsystemd-shared-262.so".to_owned(),
                    "libc.so.6".to_owned()
                ],
                runpath: vec!["/usr/lib/systemd".to_owned()],
                dlopen: vec![(
                    "libcrypto".to_owned(),
                    vec!["libcrypto.so.4".to_owned(), "libcrypto.so.3".to_owned()]
                )],
            }
        );
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(
            normalize(Path::new("usr/lib/../bin/./x")),
            Path::new("usr/bin/x")
        );
    }
}
