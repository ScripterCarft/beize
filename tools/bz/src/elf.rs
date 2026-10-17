//! The ELF check (ADR 0014): every program and library in an image tree must
//! carry Beize's hardening – like Fedora's annocheck. A package that ignores
//! the build flags breaks the build instead of shipping unhardened code.
//! Each also needs a GNU build ID, which names the exact build in core dumps
//! and crash reports (as Fedora requires); a file without one was linked by
//! something other than Beize's compiler. And each must come from Beize's
//! compilers, by what they write into `.comment`: the build host's gcc once
//! compiled libcap unnoticed. Each carries MiniDebugInfo instead of its
//! symbol table, so a crash names its functions (ADR 0023).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::packages::TARGET;
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::{debuginfo, say, tree, util};

/// Files that cannot carry the CET properties, with the reason.
const NO_CET: [(&str, &str); 11] = [
    ("usr/bin/beize-gpu-probe", "Rust"),
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
    // NVK's shader compiler (NAK) is Rust, linked into the driver.
    ("usr/lib/libvulkan_nouveau.so", "Rust"),
];

/// Where ELF files are firmware: code for the processors of devices (the
/// GPU's GSP is RISC-V), not for the CPU – none of this hardening applies.
const FIRMWARE: &str = "usr/lib/firmware";

/// Checks every ELF file under `root`; `what` names the tree in messages.
pub fn check_tree(paths: &Paths, root: &Path, what: &str) -> Result<()> {
    let readelf = readelf(paths);
    let gcc = format!("GCC: (GNU) {}", Recipe::load(paths, "gcc")?.version);
    let mut files = Vec::new();
    for file in tree::walk(root)? {
        if !file.starts_with(FIRMWARE) && tree::is_elf(&root.join(&file))? {
            files.push(root.join(file));
        }
    }
    let mut problems = Vec::new();
    let mut parsed = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(root)?.to_path_buf();
        let output = util::output(
            Command::new(&readelf)
                .args([
                    "--wide",
                    "--file-header",
                    "--program-headers",
                    "--section-headers",
                ])
                .args(["--dynamic", "--notes", "--string-dump=.comment"])
                .arg(file),
        )?;
        let needs_cet = !NO_CET.iter().any(|(path, _)| relative == Path::new(path));
        let hardening = Hardening::parse(&output)
            .with_context(|| format!("reading /{}", relative.display()))?;
        for problem in hardening.problems(needs_cet) {
            problems.push(format!("/{}: {problem}", relative.display()));
        }
        if let Some(foreign) = hardening.foreign_compiler(&gcc) {
            problems.push(format!("/{}: compiled by {foreign}", relative.display()));
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
        "{} of {} ELF files in {what} lack Beize's hardening, compilers (ADR 0014) or \
         MiniDebugInfo (ADR 0023):\n  {}",
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
        "  ok: all {} ELF files in {what} come from Beize's compilers, are hardened \
         (PIE, RELRO, BIND_NOW, CET), have a build ID and MiniDebugInfo and find their \
         libraries",
        files.len()
    );
    Ok(())
}

/// The cross readelf that reads Beize's ELF files.
pub fn readelf(paths: &Paths) -> PathBuf {
    paths.tools.join("bin").join(format!("{TARGET}-readelf"))
}

/// How an ELF file is linked, from readelf's `--program-headers --dynamic
/// --notes`: what the dynamic loader loads for it, and where it looks.
#[derive(Debug, Default, PartialEq)]
pub struct Linking {
    pub interpreter: Option<String>,
    /// Its name as a library (DT_SONAME).
    pub soname: Option<String>,
    /// The libraries it needs (DT_NEEDED).
    pub needed: Vec<String>,
    /// Where it looks for them first (DT_RUNPATH, or the older DT_RPATH).
    pub runpath: Vec<String>,
    /// dlopen features (`.note.dlopen`) and the sonames that provide them,
    /// in preference order.
    pub dlopen: Vec<(String, Vec<String>)>,
}

#[derive(Deserialize)]
struct DlopenNote {
    feature: String,
    soname: Vec<String>,
}

impl Linking {
    pub fn parse(output: &str) -> Result<Self> {
        let mut linking = Self::default();
        for line in output.lines() {
            let line = line.trim();
            if let Some(at) = line.find("Dlopen Metadata: ") {
                let json = line[at + "Dlopen Metadata: ".len()..].trim();
                let notes: Vec<DlopenNote> = serde_json::from_str(json)
                    .with_context(|| format!("parsing the dlopen note {json}"))?;
                linking
                    .dlopen
                    .extend(notes.into_iter().map(|n| (n.feature, n.soname)));
            } else if let Some(path) = line
                .strip_prefix("[Requesting program interpreter: ")
                .and_then(|rest| rest.strip_suffix(']'))
            {
                linking.interpreter = Some(path.to_owned());
            } else if line.contains("(SONAME)") {
                linking.soname = bracketed(line);
            } else if line.contains("(NEEDED)") {
                linking.needed.extend(bracketed(line));
            } else if line.contains("(RUNPATH)") || line.contains("(RPATH)") {
                linking.runpath.extend(
                    bracketed(line)
                        .iter()
                        .flat_map(|dirs| dirs.split(':'))
                        .map(str::to_owned),
                );
            }
        }
        Ok(linking)
    }

    /// The directories of its run path, relative to the root; `$ORIGIN` is
    /// the directory of `file` (relative to the root).
    pub fn runpath_dirs(&self, file: &Path) -> Vec<PathBuf> {
        let origin = Path::new("/").join(file.parent().unwrap_or(Path::new("")));
        self.runpath
            .iter()
            .map(|dir| {
                tree::lexical(Path::new(
                    &dir.replace("$ORIGIN", &origin.to_string_lossy()),
                ))
            })
            .collect()
    }
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
    linking: Linking,
    /// What the compilers wrote into `.comment`, e.g. `GCC: (GNU) 16.2.0`.
    compilers: Vec<String>,
    /// MiniDebugInfo, and the full symbol table it replaces (ADR 0023).
    mini_debug_info: bool,
    symbol_table: bool,
}

impl Hardening {
    /// Parses `readelf --wide --file-header --program-headers --section-headers
    /// --dynamic --notes --string-dump=.comment`.
    fn parse(output: &str) -> Result<Self> {
        let mut h = Self {
            linking: Linking::parse(output)?,
            ..Self::default()
        };
        let mut in_comment = false;
        for line in output.lines() {
            let line = line.trim();
            if line.starts_with("String dump of section '.comment'") {
                in_comment = true;
            } else if in_comment && line.starts_with('[') {
                if let Some((_, text)) = line.split_once(']') {
                    h.compilers.push(text.trim().to_owned());
                }
            } else if let Some(kind) = line.strip_prefix("Type:") {
                h.kind = kind
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            } else if let Some(name) = section_name(line) {
                h.mini_debug_info |= name == debuginfo::SECTION;
                h.symbol_table |= name == ".symtab";
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
            } else if line.contains("NT_GNU_BUILD_ID") {
                h.build_id = true;
            } else if let Some(at) = line.find("x86 feature:") {
                let features = &line[at + "x86 feature:".len()..];
                let features = features.split(", x86 ").next().unwrap_or_default();
                h.ibt |= features.split(',').any(|f| f.trim() == "IBT");
                h.shstk |= features.split(',').any(|f| f.trim() == "SHSTK");
            }
        }
        Ok(h)
    }

    /// The directories, relative to the root, where the dynamic loader
    /// looks for the libraries `file` needs: its runpath, then /usr/lib.
    fn search_dirs(&self, file: &Path) -> Vec<PathBuf> {
        let mut dirs = self.linking.runpath_dirs(file);
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
        if let Some(soname) = &self.linking.soname {
            for (loader, parsed) in all {
                if parsed.linking.needed.contains(soname) {
                    dirs.extend(parsed.search_dirs(loader));
                }
            }
        }
        self.linking
            .needed
            .iter()
            .filter(|library| {
                !dirs
                    .iter()
                    .any(|dir| root.join(dir).join(library).symlink_metadata().is_ok())
            })
            .cloned()
            .collect()
    }

    /// A compiler other than Beize's gcc (`gcc`, as it names itself) and
    /// rustc, or a note that none named itself. Kernel modules come from the
    /// kernel's build, with the build host's compiler (ADR 0015).
    fn foreign_compiler(&self, gcc: &str) -> Option<String> {
        if self.kind == "REL" {
            return None;
        }
        if self.compilers.is_empty() {
            return Some("an unknown compiler (no .comment)".into());
        }
        self.compilers
            .iter()
            .find(|c| *c != gcc && !c.starts_with("rustc version "))
            .map(|c| format!("\"{c}\""))
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
        if !self.mini_debug_info {
            problems.push("no MiniDebugInfo");
        }
        if self.symbol_table {
            problems.push("a full symbol table");
        }
        problems
    }
}

/// The name in a line of readelf's section headers: `[ 1] .interp PROGBITS ...`.
fn section_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let (index, rest) = rest.split_once(']')?;
    index.trim().parse::<u32>().ok()?;
    rest.split_whitespace().next()
}

/// The text between the brackets of a readelf line: `... [libc.so.6]`.
fn bracketed(line: &str) -> Option<String> {
    let start = line.find('[')? + 1;
    let end = line.rfind(']')?;
    line.get(start..end).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn parse(output: &str) -> Hardening {
        Hardening::parse(output).unwrap()
    }

    const HARDENED: &str = "\
  Type:                              DYN (Position-Independent Executable file)
  DYNAMIC        0x14dcd8 0x000000000014ecd8 0x000000000014ecd8 0x0001e0 0x0001e0 RW  0x8
  GNU_STACK      0x000000 0x0000000000000000 0x0000000000000000 0x000000 0x000000 RW  0x10
  GNU_RELRO      0x14af70 0x000000000014bf70 0x000000000014bf70 0x003090 0x003090 R   0x1
  [ 1] .interp           PROGBITS        0000000000000318 000318 00001c 00   A  0   0  1
  [29] .comment          PROGBITS        0000000000000000 14f010 00001b 01  MS  0   0  1
  [30] .gnu_debugdata    PROGBITS        0000000000000000 14f02b 0012a8 00      0   0  1
 0x000000000000001e (FLAGS)              BIND_NOW
 0x000000006ffffffb (FLAGS_1)            Flags: NOW PIE
  GNU                  0x00000014	NT_GNU_BUILD_ID (unique build ID bitstring)
  GNU                  0x00000030	NT_GNU_PROPERTY_TYPE_0	      Properties: x86 feature: IBT, SHSTK, x86 ISA needed: x86-64-baseline, x86 feature used: x86, XMM
";

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
            Linking::parse(output).unwrap(),
            Linking {
                interpreter: Some("/lib64/ld-linux-x86-64.so.2".to_owned()),
                soname: None,
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
    fn a_hardened_program_passes() {
        let h = parse(HARDENED);
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
            parse(&lazy).problems(true),
            ["no BIND_NOW", "no CET (IBT and SHSTK)"]
        );
        // A Rust program: CET is not required.
        assert_eq!(parse(&lazy).problems(false), ["no BIND_NOW"]);

        let exec = HARDENED.replace("DYN (Position-Independent", "EXEC (");
        assert_eq!(parse(&exec).problems(true), ["not position-independent"]);
        let stack = HARDENED.replace("0x000000 RW  0x10", "0x000000 RWE 0x10");
        assert_eq!(parse(&stack).problems(true), ["executable stack"]);
        let no_stack = HARDENED.replace("  GNU_STACK", "  XNU_STACK");
        assert_eq!(parse(&no_stack).problems(true), ["executable stack"]);
        // Linked by a compiler without --enable-linker-build-id.
        let no_id = HARDENED.replace("NT_GNU_BUILD_ID", "NT_GNU_HWCAP");
        assert_eq!(parse(&no_id).problems(true), ["no build ID"]);
        // Stripped by its own build, or never prepared (ADR 0023).
        let no_mini = HARDENED.replace(".gnu_debugdata", ".gnu_debuglink");
        assert_eq!(parse(&no_mini).problems(true), ["no MiniDebugInfo"]);
        let symtab = HARDENED.replace(".interp ", ".symtab ");
        assert_eq!(parse(&symtab).problems(true), ["a full symbol table"]);
        let textrel = HARDENED.replace("BIND_NOW\n", "BIND_NOW TEXTREL\n");
        assert_eq!(parse(&textrel).problems(true), ["text relocations"]);
        // "x86 feature used: ... IBT" is not the property.
        let used_only = HARDENED.replace(
            "x86 feature: IBT, SHSTK, ",
            "x86 feature used: IBT, SHSTK, ",
        );
        assert_eq!(parse(&used_only).problems(true), ["no CET (IBT and SHSTK)"]);
    }

    #[test]
    fn only_beizes_compilers_are_accepted() {
        let gcc = "GCC: (GNU) 16.2.0";
        let dump = |comments: &str| {
            parse(&format!(
                "{HARDENED}\nString dump of section '.comment':\n{comments}\n"
            ))
        };
        assert_eq!(
            dump("  [     0]  GCC: (GNU) 16.2.0").foreign_compiler(gcc),
            None
        );
        assert_eq!(
            dump("  [     0]  rustc version 1.99.0 (b940084d7 2026-09-28)\n  [    2d]  GCC: (GNU) 16.2.0")
                .foreign_compiler(gcc),
            None
        );
        assert_eq!(
            dump("  [     0]  GCC: (Ubuntu 15.2.0-16ubuntu1) 15.2.0").foreign_compiler(gcc),
            Some("\"GCC: (Ubuntu 15.2.0-16ubuntu1) 15.2.0\"".into())
        );
        assert!(parse(HARDENED).foreign_compiler(gcc).is_some());
        // "[Requesting program interpreter: ...]" is no compiler.
        let interpreter = "      [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\n";
        assert!(
            parse(&format!("{interpreter}{HARDENED}"))
                .compilers
                .is_empty()
        );
    }

    #[test]
    fn kernel_modules_are_left_to_the_kernel() {
        let module = "  Type:                              REL (Relocatable file)\n";
        assert!(parse(module).problems(true).is_empty());
        assert_eq!(parse(module).foreign_compiler("GCC: (GNU) 16.2.0"), None);
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
        let h = parse(program);
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
        let core = parse(
            " 0x000000000000000e (SONAME)             Library soname: [libsystemd-core-262.so]
 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-shared-262.so]
",
        );
        let core_path = Path::new("usr/lib/systemd/libsystemd-core-262.so");
        assert_eq!(
            core.unresolved(&root, core_path, &[]),
            ["libsystemd-shared-262.so"]
        );
        let systemd = parse(&format!(
            "{program} 0x0000000000000001 (NEEDED)             Shared library: [libsystemd-core-262.so]\n"
        ));
        let all = [(PathBuf::from("usr/bin/systemd-tool"), systemd)];
        assert!(core.unresolved(&root, core_path, &all).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
