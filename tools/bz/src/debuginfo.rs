//! Symbols of every program and library (ADR 0023), as Fedora's
//! find-debuginfo prepares each package: a crashed program's stack trace
//! needs the names of its functions, but not the full symbol table.
//!
//! Each ELF file a package installs gets MiniDebugInfo – its function
//! symbols that the dynamic symbol table lacks, in an xz-compressed ELF file
//! inside the section `.gnu_debugdata` – and loses its symbol table and
//! debug information. systemd-coredump reads the names from there with
//! libdw. `.comment` stays: the ELF check (ADR 0014) tells Beize's compilers
//! by it.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::packages::TARGET;
use crate::paths::Paths;
use crate::{tree, util};

/// The section MiniDebugInfo lives in (what gdb and libdw look for).
pub const SECTION: &str = ".gnu_debugdata";

/// Gives every program and library below `pkg` MiniDebugInfo and strips it.
/// Returns how many files it prepared.
pub fn prepare(paths: &Paths, pkg: &Path) -> Result<usize> {
    let tools = Tools::new(paths);
    let scratch = scratch_dir(pkg)?;
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for file in linked_files(pkg)? {
        // A hard link is prepared once, through its first name.
        let meta = fs::metadata(&file)?;
        if !seen.insert((meta.dev(), meta.ino())) {
            continue;
        }
        writable(&file, || prepare_file(&tools, &file, &scratch))
            .with_context(|| format!("preparing the symbols of {}", file.display()))?;
        count += 1;
    }
    fs::remove_dir_all(&scratch)?;
    Ok(count)
}

/// The cross binutils that read and write Beize's ELF files.
struct Tools {
    objcopy: PathBuf,
    nm: PathBuf,
    strip: PathBuf,
}

impl Tools {
    fn new(paths: &Paths) -> Self {
        let tool = |name: &str| paths.tools.join("bin").join(format!("{TARGET}-{name}"));
        Self {
            objcopy: tool("objcopy"),
            nm: tool("nm"),
            strip: tool("strip"),
        }
    }
}

/// find-debuginfo's `add_minidebug`, then the strip.
fn prepare_file(tools: &Tools, file: &Path, scratch: &Path) -> Result<()> {
    let symbols = scratch.join("symbols");
    let keep = scratch.join("keep");
    let mini = scratch.join("mini");
    // A build that strips by itself leaves no names to keep.
    let functions_in_file = names(tools, file, &[], |kind| matches!(kind, "T" | "t"))?;
    ensure!(
        !functions_in_file.is_empty(),
        "it has no symbol table: its build stripped it, which is bz's work (ADR 0023)"
    );
    // The symbol table and debug sections, without code or data.
    util::run(
        Command::new(&tools.objcopy)
            .arg("--only-keep-debug")
            .arg(file)
            .arg(&symbols),
    )?;
    // Functions the dynamic symbol table does not name already.
    let dynamic = names(tools, file, &["--dynamic"], |_| true)?;
    let functions = names(tools, &symbols, &[], |kind| matches!(kind, "T" | "t" | "D"))?;
    let wanted: Vec<&str> = functions
        .iter()
        .filter(|name| !dynamic.contains(*name))
        .map(String::as_str)
        .collect();
    fs::write(&keep, wanted.join("\n") + "\n")?;
    util::run(
        Command::new(&tools.objcopy)
            .args([
                "--strip-all",
                "--remove-section=.gdb_index",
                "--remove-section=.comment",
            ])
            .arg(format!("--keep-symbols={}", keep.display()))
            .arg(&symbols)
            .arg(&mini),
    )?;
    // One thread: the same input always gives the same archive.
    util::run(
        Command::new("xz")
            .args(["--threads=1", "--force", "--keep"])
            .arg(&mini),
    )?;
    util::run(Command::new(&tools.strip).arg("--strip-all").arg(file))?;
    util::run(
        Command::new(&tools.objcopy)
            .arg(format!("--add-section={SECTION}={}.xz", mini.display()))
            .arg(file),
    )?;
    for done in [&symbols, &keep, &mini, &mini.with_extension("xz")] {
        fs::remove_file(done)?;
    }
    Ok(())
}

/// The defined symbols of `file` whose kind `wanted` accepts, by name.
fn names(
    tools: &Tools,
    file: &Path,
    args: &[&str],
    wanted: impl Fn(&str) -> bool,
) -> Result<BTreeSet<String>> {
    let out = util::output(
        Command::new(&tools.nm)
            .args(args)
            .args(["--format=posix", "--defined-only"])
            .arg(file),
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let name = fields.next()?;
            let kind = fields.next()?;
            wanted(kind).then(|| name.to_owned())
        })
        .collect())
}

/// Programs and shared libraries below `dir`: ELF files of type `ET_DYN`
/// (Beize links everything as PIE). Object files and archives are only for
/// building.
fn linked_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for file in tree::walk(dir)? {
        let path = dir.join(file);
        if path.symlink_metadata()?.is_file() && elf_type(&path)? == Some(ET_DYN) {
            found.push(path);
        }
    }
    Ok(found)
}

const ET_DYN: u16 = 3;

/// `e_type` of an ELF file, `None` for anything else.
fn elf_type(path: &Path) -> Result<Option<u16>> {
    let mut header = [0; 18];
    let read = fs::File::open(path)?.read(&mut header)?;
    if read < header.len() || header[..4] != *b"\x7fELF" {
        return Ok(None);
    }
    Ok(Some(u16::from_le_bytes([header[16], header[17]])))
}

/// Runs `work` on `file`, made writable for it if a package installed it
/// read-only, with its mode restored afterwards.
fn writable(file: &Path, work: impl FnOnce() -> Result<()>) -> Result<()> {
    let mode = fs::metadata(file)?.permissions().mode();
    let read_only = mode & 0o200 == 0;
    if read_only {
        fs::set_permissions(file, fs::Permissions::from_mode(mode | 0o200))?;
    }
    let result = work();
    if read_only {
        fs::set_permissions(file, fs::Permissions::from_mode(mode))?;
    }
    result
}

/// A scratch directory beside the staging tree, not inside it.
fn scratch_dir(pkg: &Path) -> Result<PathBuf> {
    let name = pkg
        .file_name()
        .context("a staging tree without a name")?
        .to_string_lossy();
    let dir = pkg.with_file_name(format!("{name}.debuginfo"));
    tree::recreate(&dir)?;
    Ok(dir)
}
