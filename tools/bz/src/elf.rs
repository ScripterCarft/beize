//! The ELF check (ADR 0014): every program and library in an image tree must
//! carry Beize's hardening – like Fedora's annocheck. A package that ignores
//! the build flags breaks the build instead of shipping unhardened code.
//! Each also needs a GNU build ID, which names the exact build in core dumps
//! and crash reports (as Fedora requires); a file without one was linked by
//! something other than Beize's compiler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, ensure};

use crate::packages::TARGET;
use crate::paths::Paths;
use crate::{say, tree, util};

/// Files that cannot carry the CET properties, with the reason.
const NO_CET: [(&str, &str); 9] = [
    // Stable Rust cannot generate CET code yet (ADR 0014).
    ("usr/lib/beize/beize-setup", "Rust"),
    ("usr/lib/beize/beize-tpm", "Rust"),
    ("usr/bin/beize-account", "Rust"),
    ("usr/lib/beize/beize-accountsd", "Rust"),
    ("usr/bin/beize-update", "Rust"),
    ("usr/lib/beize/beize-updated", "Rust"),
    ("usr/lib/beize/beize-update-fetch", "Rust"),
    ("usr/bin/ntp-daemon", "Rust"),
    ("usr/bin/ntp-ctl", "Rust"),
];

/// Checks every ELF file under `root`; `what` names the tree in messages.
pub fn check_tree(paths: &Paths, root: &Path, what: &str) -> Result<()> {
    let readelf = paths.tools.join("bin").join(format!("{TARGET}-readelf"));
    let mut files = Vec::new();
    collect_elf(root, &mut files)?;
    let mut problems = Vec::new();
    let mut parsed = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(root)?.to_path_buf();
        let output = util::output(
            Command::new(&readelf)
                .args(["--wide", "--file-header", "--program-headers"])
                .args(["--dynamic", "--notes"])
                .arg(file),
        )?;
        let needs_cet = !NO_CET.iter().any(|(path, _)| relative == Path::new(path));
        let hardening = Hardening::parse(&output);
        for problem in hardening.problems(needs_cet) {
            problems.push(format!("/{}: {problem}", relative.display()));
        }
        parsed.push((relative, hardening));
    }
    let mut unresolved = Vec::new();
    for (relative, hardening) in &parsed {
        for library in hardening.unresolved(root, relative, &parsed) {
            unresolved.push(format!("/{} needs {library}", relative.display()));
        }
    }
    ensure!(
        problems.is_empty(),
        "{} of {} ELF files in {what} lack Beize's hardening (ADR 0014):\n  {}",
        problems.len(),
        files.len(),
        problems.join("\n  ")
    );
    ensure!(
        unresolved.is_empty(),
        "libraries missing in {what}:\n  {}",
        unresolved.join("\n  ")
    );
    say!(
        "  ok: all {} ELF files in {what} are hardened (PIE, RELRO, BIND_NOW, CET), \
         have a build ID and find their libraries",
        files.len()
    );
    Ok(())
}

fn collect_elf(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_elf(&path, files)?;
        } else if tree::is_elf(&path)? {
            files.push(path);
        }
    }
    files.sort();
    Ok(())
}

/// What readelf says about one file.
#[derive(Debug, Default, PartialEq)]
struct Hardening {
    /// `EXEC`, `DYN` or `REL`.
    kind: String,
    dynamic: bool,
    relro: bool,
    bind_now: bool,
    textrel: bool,
    /// None without a GNU_STACK header (the stack is then executable).
    executable_stack: Option<bool>,
    ibt: bool,
    shstk: bool,
    build_id: bool,
    /// Its name as a library (DT_SONAME), the libraries it needs
    /// (DT_NEEDED) and where it looks for them first (DT_RUNPATH).
    soname: Option<String>,
    needed: Vec<String>,
    runpath: Vec<String>,
}

impl Hardening {
    /// Parses `readelf --wide --file-header --program-headers --dynamic --notes`.
    fn parse(output: &str) -> Self {
        let mut h = Self::default();
        for line in output.lines() {
            let line = line.trim();
            if let Some(kind) = line.strip_prefix("Type:") {
                h.kind = kind
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            } else if line.starts_with("DYNAMIC ") {
                h.dynamic = true;
            } else if line.starts_with("GNU_RELRO ") {
                h.relro = true;
            } else if line.starts_with("GNU_STACK ") {
                // The flags follow the sizes: "RW " or "RWE".
                h.executable_stack = Some(
                    line.split_whitespace()
                        .any(|f| f.contains('E') && f.starts_with('R')),
                );
            } else if line.contains("(FLAGS)") {
                h.bind_now |= line.contains("BIND_NOW");
                h.textrel |= line.contains("TEXTREL");
            } else if line.contains("(FLAGS_1)") {
                h.bind_now |= line.split_whitespace().any(|f| f == "NOW");
            } else if line.contains("(TEXTREL)") {
                h.textrel = true;
            } else if line.contains("(SONAME)") {
                h.soname = bracketed(line);
            } else if line.contains("(NEEDED)") {
                h.needed.extend(bracketed(line));
            } else if line.contains("(RUNPATH)") || line.contains("(RPATH)") {
                h.runpath.extend(
                    bracketed(line)
                        .iter()
                        .flat_map(|dirs| dirs.split(':'))
                        .map(str::to_owned),
                );
            } else if line.contains("NT_GNU_BUILD_ID") {
                h.build_id = true;
            } else if let Some(at) = line.find("x86 feature:") {
                let features = &line[at + "x86 feature:".len()..];
                let features = features.split(", x86 ").next().unwrap_or_default();
                h.ibt |= features.split(',').any(|f| f.trim() == "IBT");
                h.shstk |= features.split(',').any(|f| f.trim() == "SHSTK");
            }
        }
        h
    }

    /// The directories, relative to the root, where the dynamic loader
    /// looks for the libraries `file` needs: its runpath, then /usr/lib.
    fn search_dirs(&self, file: &Path) -> Vec<PathBuf> {
        let origin = Path::new("/").join(file.parent().unwrap_or(Path::new("")));
        let mut dirs = Vec::new();
        for dir in &self.runpath {
            dirs.push(lexical(&dir.replace("$ORIGIN", &origin.to_string_lossy())));
        }
        dirs.push(PathBuf::from("usr/lib"));
        dirs
    }

    /// The needed libraries that `file` (relative to `root`) would not find
    /// in `root`. As for the dynamic loader, a library loaded by a program
    /// also finds what that program's search path found (`all`: every ELF
    /// file of the tree): systemd's libsystemd-core has no runpath and gets
    /// libsystemd-shared from systemd's.
    fn unresolved(&self, root: &Path, file: &Path, all: &[(PathBuf, Hardening)]) -> Vec<String> {
        let mut dirs = self.search_dirs(file);
        if let Some(soname) = &self.soname {
            for (loader, parsed) in all {
                if parsed.needed.contains(soname) {
                    dirs.extend(parsed.search_dirs(loader));
                }
            }
        }
        self.needed
            .iter()
            .filter(|library| {
                !dirs
                    .iter()
                    .any(|dir| root.join(dir).join(library).symlink_metadata().is_ok())
            })
            .cloned()
            .collect()
    }

    fn problems(&self, needs_cet: bool) -> Vec<&'static str> {
        let mut problems = Vec::new();
        // Relocatable objects (kernel modules) follow the kernel's hardening.
        if self.kind == "REL" {
            return problems;
        }
        if self.kind != "DYN" {
            problems.push("not position-independent");
        }
        if self.dynamic {
            if !self.relro {
                problems.push("no RELRO");
            }
            if !self.bind_now {
                problems.push("no BIND_NOW");
            }
            if self.textrel {
                problems.push("text relocations");
            }
        }
        if self.executable_stack != Some(false) {
            problems.push("executable stack");
        }
        if needs_cet && !(self.ibt && self.shstk) {
            problems.push("no CET (IBT and SHSTK)");
        }
        if !self.build_id {
            problems.push("no build ID");
        }
        problems
    }
}

/// An absolute path made relative to the root, `..` resolved by name.
fn lexical(path: &str) -> PathBuf {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.iter().collect()
}

/// The text between the brackets of a readelf line: `... [libc.so.6]`.
fn bracketed(line: &str) -> Option<String> {
    let start = line.find('[')? + 1;
    let end = line.rfind(']')?;
    line.get(start..end).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARDENED: &str = "\
  Type:                              DYN (Position-Independent Executable file)
  DYNAMIC        0x14dcd8 0x000000000014ecd8 0x000000000014ecd8 0x0001e0 0x0001e0 RW  0x8
  GNU_STACK      0x000000 0x0000000000000000 0x0000000000000000 0x000000 0x000000 RW  0x10
  GNU_RELRO      0x14af70 0x000000000014bf70 0x000000000014bf70 0x003090 0x003090 R   0x1
 0x000000000000001e (FLAGS)              BIND_NOW
 0x000000006ffffffb (FLAGS_1)            Flags: NOW PIE
  GNU                  0x00000014	NT_GNU_BUILD_ID (unique build ID bitstring)
  GNU                  0x00000030	NT_GNU_PROPERTY_TYPE_0	      Properties: x86 feature: IBT, SHSTK, x86 ISA needed: x86-64-baseline, x86 feature used: x86, XMM
";

    #[test]
    fn a_hardened_program_passes() {
        let h = Hardening::parse(HARDENED);
        assert!(h.ibt && h.shstk && h.bind_now && h.relro, "{h:?}");
        assert!(h.problems(true).is_empty(), "{:?}", h.problems(true));
    }

    #[test]
    fn each_missing_hardening_is_named() {
        // What bash looked like before ADR 0014.
        let lazy = HARDENED
            .replace(" 0x000000000000001e (FLAGS)              BIND_NOW\n", "")
            .replace("Flags: NOW PIE", "Flags: PIE")
            .replace("x86 feature: IBT, SHSTK, ", "");
        assert_eq!(
            Hardening::parse(&lazy).problems(true),
            ["no BIND_NOW", "no CET (IBT and SHSTK)"]
        );
        // A Rust program: CET is not required.
        assert_eq!(Hardening::parse(&lazy).problems(false), ["no BIND_NOW"]);

        let exec = HARDENED.replace("DYN (Position-Independent", "EXEC (");
        assert_eq!(
            Hardening::parse(&exec).problems(true),
            ["not position-independent"]
        );
        let stack = HARDENED.replace("0x000000 RW  0x10", "0x000000 RWE 0x10");
        assert_eq!(
            Hardening::parse(&stack).problems(true),
            ["executable stack"]
        );
        let no_stack = HARDENED.replace("  GNU_STACK", "  XNU_STACK");
        assert_eq!(
            Hardening::parse(&no_stack).problems(true),
            ["executable stack"]
        );
        // Linked by a compiler without --enable-linker-build-id.
        let no_id = HARDENED.replace("NT_GNU_BUILD_ID", "NT_GNU_HWCAP");
        assert_eq!(Hardening::parse(&no_id).problems(true), ["no build ID"]);
        let textrel = HARDENED.replace("BIND_NOW\n", "BIND_NOW TEXTREL\n");
        assert_eq!(
            Hardening::parse(&textrel).problems(true),
            ["text relocations"]
        );
        // "x86 feature used: ... IBT" is not the property.
        let used_only = HARDENED.replace(
            "x86 feature: IBT, SHSTK, ",
            "x86 feature used: IBT, SHSTK, ",
        );
        assert_eq!(
            Hardening::parse(&used_only).problems(true),
            ["no CET (IBT and SHSTK)"]
        );
    }

    #[test]
    fn kernel_modules_are_left_to_the_kernel() {
        let module = "  Type:                              REL (Relocatable file)\n";
        assert!(Hardening::parse(module).problems(true).is_empty());
    }

    #[test]
    fn needed_libraries_are_found_in_usr_lib_or_the_runpath() {
        let root = std::env::temp_dir().join(format!("bz-needed-{}", std::process::id()));
        fs::create_dir_all(root.join("usr/lib/systemd")).unwrap();
        fs::write(root.join("usr/lib/libc.so.6"), "").unwrap();
        fs::write(root.join("usr/lib/systemd/libsystemd-shared-262.so"), "").unwrap();
        let program = "\
 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-shared-262.so]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libstdc++.so.6]
 0x000000000000001d (RUNPATH)            Library runpath: [$ORIGIN/../lib/systemd]
";
        let h = Hardening::parse(program);
        assert_eq!(
            h.unresolved(&root, Path::new("usr/bin/systemctl"), &[]),
            ["libstdc++.so.6"]
        );
        // The same runpath seen from elsewhere does not lead there.
        assert_eq!(
            h.unresolved(&root, Path::new("usr/lib/beize/tool"), &[]),
            ["libsystemd-shared-262.so", "libstdc++.so.6"]
        );

        // A library without a runpath finds what the program loading it
        // found (libsystemd-core, loaded by systemd); alone it would not.
        let core = Hardening::parse(
            " 0x000000000000000e (SONAME)             Library soname: [libsystemd-core-262.so]
 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-shared-262.so]
",
        );
        let core_path = Path::new("usr/lib/systemd/libsystemd-core-262.so");
        assert_eq!(
            core.unresolved(&root, core_path, &[]),
            ["libsystemd-shared-262.so"]
        );
        let systemd = Hardening::parse(&format!(
            "{program} 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-core-262.so]\n"
        ));
        let all = [(PathBuf::from("usr/bin/systemd-tool"), systemd)];
        assert!(core.unresolved(&root, core_path, &all).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
