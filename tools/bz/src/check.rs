//! `bz check` and `bz gate`: one list of what must hold before a commit and
//! before a push, the same here and in the CI's `check` job, so a red only
//! the CI would find (a test, a lint, a banned crate) never reaches it.
//!
//! `bz gate` records the tree it passed on, and `.githooks/pre-push` refuses
//! to push another one to `main`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::paths::Paths;
use crate::{build_all, say, util, vm};

/// What the CI's `check` job runs (`.github/workflows/ci.yml`).
const CHECKS: [(&str, &[&str]); 5] = [
    ("Format", &["fmt", "--all", "--check"]),
    (
        "Lints",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ),
    // The compositor alone, as its recipe builds it: beize-draw without
    // `icons`, which the workspace turns on for Beize UI.
    (
        "Lints of the compositor alone",
        &[
            "clippy",
            "-p",
            "beize-compositor",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("Tests", &["test", "--workspace"]),
    // RustSec advisories, licenses, origin, layers (deny.toml).
    ("Dependencies", &["deny", "check"]),
];

/// Formats, lints, tests and checks the dependencies.
pub fn check() -> Result<()> {
    for (title, args) in CHECKS {
        util::step(title);
        util::run(Command::new("cargo").args(args))?;
    }
    Ok(())
}

/// All of it: `check`, the build as the CI runs it, the whole smoke test.
/// On success, remembers the working tree for `.githooks/pre-push`.
pub fn gate(paths: &Paths) -> Result<()> {
    let began = Instant::now();
    let record = paths.work.join("gate-passed");
    match fs::remove_file(&record) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("removing {}", record.display()));
        }
        _ => {}
    }
    check()?;
    util::set_locked(true);
    build_all(paths)?;
    vm::smoke(paths, false)?;
    fs::write(&record, working_tree(paths)?)?;
    say!("\n  ok: the gate passed in {} s", began.elapsed().as_secs());
    Ok(())
}

/// The tree a commit of everything here would have, without touching the
/// real index: a commit may come after the gate.
fn working_tree(paths: &Paths) -> Result<String> {
    let index = paths.work.join("gate-index");
    let real = util::output(Command::new("git").args(["rev-parse", "--git-path", "index"]))?;
    fs::copy(paths.root.join(real), &index).context("copying git's index")?;
    let git = |args: &[&str], index: &Path| {
        util::output(Command::new("git").args(args).env("GIT_INDEX_FILE", index))
    };
    git(&["add", "--all"], &index)?;
    let tree = git(&["write-tree"], &index)?;
    fs::remove_file(&index)?;
    Ok(tree)
}
