//! What the image weighs, against the variant's `images/*/sizes.toml`: its
//! `/usr` image and each of Beize's own programs. A ratchet like
//! `security`, with room for the small changes of every build: more than
//! `TOLERANCE` larger fails until the value is raised (the reason in the
//! commit), more than `TOLERANCE` smaller fails until the new value is
//! written, so the file always says what the image weighs.

use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use crate::paths::Paths;
use crate::tree;

/// How far a size may move before the file has to follow, in percent.
const TOLERANCE: u64 = 3;

/// Where Beize's own programs lie in `/usr`.
const PROGRAMS: [(&str, &str); 2] = [("usr/lib/beize", ""), ("usr/bin", "beize-")];

/// Checks each of Beize's own programs in the `/usr` tree under `root`: an
/// ELF file in `usr/lib/beize` or a `beize-*` one in `usr/bin`.
pub fn check_programs(paths: &Paths, root: &Path) -> Result<()> {
    let sizes = paths.variant.sizes()?;
    let mut problems = Vec::new();
    let mut found = Vec::new();
    for (dir, prefix) in PROGRAMS {
        for entry in fs::read_dir(root.join(dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix)
                || !entry.file_type()?.is_file()
                || !tree::is_elf(&entry.path())?
            {
                continue;
            }
            let program = format!("{dir}/{name}");
            let actual = entry.metadata()?.len().div_ceil(1024);
            match sizes.programs.get(&program) {
                Some(&expected) => problems.extend(compare(&program, actual, expected)),
                None => problems.push(format!(
                    "{program}: Beize's program has no entry ({actual} KiB)"
                )),
            }
            found.push(program);
        }
    }
    for program in sizes.programs.keys() {
        if !found.contains(program) {
            problems.push(format!("{program}: not in /usr"));
        }
    }
    report(problems)
}

/// Checks the size of the `/usr` image, `bytes`.
pub fn check_usr(paths: &Paths, bytes: u64) -> Result<()> {
    let expected = paths.variant.sizes()?.usr;
    report(
        compare("the /usr image", bytes.div_ceil(1024), expected)
            .into_iter()
            .collect(),
    )
}

/// What is wrong with a size of `actual` KiB against `expected`, if it
/// moved further than `TOLERANCE`.
fn compare(what: &str, actual: u64, expected: u64) -> Option<String> {
    if actual.abs_diff(expected) * 100 <= expected * TOLERANCE {
        return None;
    }
    let change = actual as f64 / expected.max(1) as f64 - 1.0;
    let how = if actual > expected {
        "larger: undo it, or raise the value with the reason in the commit"
    } else {
        "smaller: write the new value"
    };
    Some(format!(
        "{what}: {actual} KiB instead of {expected} ({:+.1} %, {how})",
        change * 100.0
    ))
}

fn report(problems: Vec<String>) -> Result<()> {
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "sizes differ from the variant's sizes.toml:\n  {}",
        problems.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_may_move_by_the_tolerance() {
        assert_eq!(compare("x", 103, 100), None);
        assert_eq!(compare("x", 97, 100), None);
        assert!(compare("x", 104, 100).is_some_and(|m| m.contains("larger")));
        assert!(compare("x", 96, 100).is_some_and(|m| m.contains("smaller")));
    }
}
