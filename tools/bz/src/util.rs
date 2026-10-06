use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Runs a command, echoing it first, and fails on a non-zero exit status.
pub fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("  $ {}", describe(cmd));
    let status = cmd
        .status()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    if !status.success() {
        bail!("{} failed with {status}", describe(cmd));
    }
    Ok(())
}

/// Runs a command and returns its trimmed standard output.
pub fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("starting {}", describe(cmd)))?;
    if !out.status.success() {
        bail!(
            "{} failed with {}: {}",
            describe(cmd),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Prints a step header so long builds stay readable.
pub fn step(title: &str) {
    eprintln!("\n==> {title}");
}

fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().into_owned()];
    parts.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    parts.join(" ")
}
