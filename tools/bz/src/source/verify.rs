//! Signature checks of upstream sources against the maintainer keyrings
//! under `keys/`: detached OpenPGP signatures of archives, signed git tags
//! and signed release commits (OpenPGP or git's SSH signing).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

use super::{SignedData, pipe_output, with_suffix};
use crate::say;
use crate::util;

pub(super) fn verify_detached(
    keyring: &Path,
    sig: &Path,
    archive: &Path,
    signed: SignedData,
) -> Result<()> {
    let mut gpgv = Command::new("gpgv");
    gpgv.arg("--keyring").arg(keyring).arg(sig);
    match signed {
        SignedData::Archive => {
            gpgv.arg(archive);
            check_gpgv(gpgv.output().context("starting gpgv")?)
        }
        SignedData::Uncompressed => {
            let decompressor = match archive.extension().and_then(|e| e.to_str()) {
                Some("xz") => "xz",
                Some("gz") => "gzip",
                Some("zst") => "zstd",
                other => bail!("cannot decompress .{}", other.unwrap_or("?")),
            };
            let decompress = Command::new(decompressor)
                .arg("--decompress")
                .arg("--stdout")
                .arg(archive)
                .stdout(Stdio::piped())
                .spawn()
                .with_context(|| format!("starting {decompressor}"))?;
            gpgv.arg("-");
            pipe_output(decompress, &mut gpgv).and_then(check_gpgv)
        }
    }
}

/// A maintainer keyring under `keys/`: OpenPGP keys (`<name>.gpg`, checked
/// with gpgv) or SSH keys (`<name>.allowed_signers`, ssh-keygen's format,
/// for projects that sign with git's SSH signing).
pub enum Keyring {
    Gpg(PathBuf),
    Ssh(PathBuf),
}

impl Keyring {
    pub fn find(root: &Path, name: &str) -> Result<Self> {
        let keys = root.join("keys");
        let gpg = keys.join(format!("{name}.gpg"));
        let ssh = keys.join(format!("{name}.allowed_signers"));
        match (gpg.exists(), ssh.exists()) {
            (true, false) => Ok(Self::Gpg(gpg)),
            (false, true) => Ok(Self::Ssh(ssh)),
            (true, true) => bail!("keyring {name} exists twice: .gpg and .allowed_signers"),
            (false, false) => bail!(
                "keyring {name} is missing: neither {} nor {}",
                gpg.display(),
                ssh.display()
            ),
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Gpg(path) | Self::Ssh(path) => path,
        }
    }

    /// Checks that `signature` (armored, as git stores it) signs `payload`.
    fn verify(&self, payload: &[u8], signature: &str, scratch: &Path) -> Result<()> {
        fs::create_dir_all(scratch)?;
        let (payload_path, sig_path) = (scratch.join("payload"), scratch.join("payload.sig"));
        fs::write(&payload_path, payload)?;
        fs::write(&sig_path, signature)?;
        let result = match self {
            Self::Gpg(keyring) => Command::new("gpgv")
                .arg("--keyring")
                .arg(keyring)
                .arg(&sig_path)
                .arg(&payload_path)
                .output()
                .context("starting gpgv")
                .and_then(check_gpgv),
            Self::Ssh(signers) => verify_ssh(signers, &sig_path, &payload_path),
        };
        fs::remove_dir_all(scratch)?;
        result
    }
}

/// Checks an SSH signature in git's namespace against an allowed-signers
/// file: first which listed signer made it, then the signature itself.
fn verify_ssh(signers: &Path, sig: &Path, payload: &Path) -> Result<()> {
    let ssh_keygen = || {
        let mut cmd = Command::new("ssh-keygen");
        cmd.arg("-Y");
        cmd
    };
    let principal = util::output(
        ssh_keygen()
            .arg("find-principals")
            .arg("-f")
            .arg(signers)
            .arg("-s")
            .arg(sig),
    )
    .context("the signature is not from a key in the keyring")?;
    let principal = principal.lines().next().unwrap_or_default().to_owned();
    let out = ssh_keygen()
        .arg("verify")
        .arg("-f")
        .arg(signers)
        .args(["-I", &principal, "-n", "git", "-s"])
        .arg(sig)
        .stdin(fs::File::open(payload)?)
        .output()
        .context("starting ssh-keygen")?;
    let log = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!(
            "signature check failed:\n{}{}",
            log.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    say!("  {}", log.trim());
    Ok(())
}

/// Checks that `tag` points at `commit` and, for signed sources, that the
/// tag – or for a lightweight tag the release commit, which is pinned – is
/// signed by a key in `keyring`.
pub(super) fn verify_tag(
    keyring: Option<&Keyring>,
    clone: &Path,
    tag: &str,
    commit: &str,
) -> Result<()> {
    let git = |args: &[&str]| util::output(Command::new("git").arg("-C").arg(clone).args(args));
    let actual = git(&["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])?;
    ensure!(
        actual == commit,
        "tag {tag} points at {actual}, expected {commit}"
    );
    let Some(keyring) = keyring else {
        return Ok(());
    };

    // The raw object, byte for byte: the signed payload includes the
    // trailing newline that `util::output` would trim.
    let object = |kind: &str, name: &str| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(["cat-file", kind, name])
            .output()
            .context("starting git cat-file")?;
        ensure!(out.status.success(), "git cat-file {kind} {name} failed");
        Ok(String::from_utf8(out.stdout)?)
    };
    let (payload, signature) = if git(&["cat-file", "-t", &format!("refs/tags/{tag}")])? == "tag" {
        split_tag_signature(&object("tag", tag)?)
            .with_context(|| format!("tag {tag} is not signed"))?
    } else {
        split_commit_signature(&object("commit", commit)?).with_context(|| {
            format!("tag {tag} is lightweight and its commit {commit} is not signed")
        })?
    };
    keyring.verify(
        payload.as_bytes(),
        &signature,
        &with_suffix(clone, "tagcheck"),
    )
}

/// Splits a raw tag object into the signed payload and the signature, which
/// git appends to the tag message.
fn split_tag_signature(raw: &str) -> Option<(String, String)> {
    let start = [
        "-----BEGIN PGP SIGNATURE-----",
        "-----BEGIN SSH SIGNATURE-----",
    ]
    .iter()
    .find_map(|marker| raw.find(marker))?;
    Some((raw[..start].to_owned(), raw[start..].to_owned()))
}

/// Splits a raw commit object into the signed payload (the commit without
/// its `gpgsig` header) and the signature (that header, unindented).
fn split_commit_signature(raw: &str) -> Option<(String, String)> {
    let (header, message) = raw.split_once("\n\n")?;
    let mut payload = String::new();
    let mut signature = String::new();
    let mut in_signature = false;
    for line in header.lines() {
        if let Some(first) = line.strip_prefix("gpgsig ") {
            in_signature = true;
            signature.push_str(first);
        } else if let Some(more) = line.strip_prefix(' ').filter(|_| in_signature) {
            signature.push('\n');
            signature.push_str(more);
        } else {
            in_signature = false;
            payload.push_str(line);
            payload.push('\n');
        }
    }
    if signature.is_empty() {
        return None;
    }
    signature.push('\n');
    Some((format!("{payload}\n{message}"), signature))
}

fn check_gpgv(out: std::process::Output) -> Result<()> {
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("signature check failed:\n{}", log.trim());
    }
    if let Some(good) = log.lines().find(|l| l.contains("Good signature")) {
        say!("  {}", good.trim_start_matches("gpgv: "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_signed_tag() {
        let raw = "object abc\ntype commit\ntag v262\n\nsystemd v262\n-----BEGIN PGP SIGNATURE-----\n\nxyz\n-----END PGP SIGNATURE-----\n";
        let (payload, sig) = split_tag_signature(raw).unwrap();
        assert_eq!(
            payload,
            "object abc\ntype commit\ntag v262\n\nsystemd v262\n"
        );
        assert!(sig.starts_with("-----BEGIN PGP SIGNATURE-----"));
        assert!(sig.ends_with("-----END PGP SIGNATURE-----\n"));
        assert!(split_tag_signature("object abc\n\nunsigned\n").is_none());
        let ssh =
            "object abc\n\nv1\n-----BEGIN SSH SIGNATURE-----\nU1NI\n-----END SSH SIGNATURE-----\n";
        assert_eq!(split_tag_signature(ssh).unwrap().0, "object abc\n\nv1\n");
    }

    #[test]
    fn splits_signed_commit() {
        let raw = "tree 7e\nparent 6b\nauthor A <a@x> 1 +0200\ncommitter A <a@x> 1 +0200\n\
                   gpgsig -----BEGIN SSH SIGNATURE-----\n U1NI\n \n -----END SSH SIGNATURE-----\n\
                   \nRelease 1.9.0\n";
        let (payload, sig) = split_commit_signature(raw).unwrap();
        assert_eq!(
            payload,
            "tree 7e\nparent 6b\nauthor A <a@x> 1 +0200\ncommitter A <a@x> 1 +0200\n\nRelease 1.9.0\n"
        );
        assert_eq!(
            sig,
            "-----BEGIN SSH SIGNATURE-----\nU1NI\n\n-----END SSH SIGNATURE-----\n"
        );
        assert!(split_commit_signature("tree 7e\nauthor A\n\nunsigned\n").is_none());
    }
}
