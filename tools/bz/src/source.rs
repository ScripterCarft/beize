//! Upstream sources: download, pin and verify.
//!
//! Every source is pinned (SHA-256 for archives and single files, commit for
//! git tags) and signed by its upstream maintainers. Signatures are checked
//! against a keyring committed under `keys/` – OpenPGP keys with `gpgv`, SSH
//! keys (git's SSH signing) with `ssh-keygen` – so a source can only enter
//! the build if both the pin and the maintainer's signature match.
//!
//! The only exception is a project that publishes no signatures at all: its
//! recipe must say so with `unsigned = "<reason>"`, the pin is then the only
//! protection, and `bz verify` lists every such source.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::paths::Paths;
use crate::say;
use crate::util;

#[derive(Debug, Clone)]
pub struct Source {
    pub origin: Origin,
    pub trust: Trust,
    /// Where `bz outdated` learns of newer releases.
    pub latest: Option<Latest>,
}

/// Where upstream announces its releases (`latest = { … }` in a recipe).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Latest {
    /// A project on release-monitoring.org (Anitya), which Fedora tracks
    /// its packages with: `{ release-monitoring = 1517 }`.
    ReleaseMonitoring(u64),
    /// kernel.org's release list, the newest of a moniker: `{ kernel-org = "stable" }`.
    KernelOrg(String),
    /// A URL that always serves upstream's current file; newer when its
    /// SHA-256 differs from the pin: `{ file = "https://…" }`.
    File(String),
}

/// Why a source is trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Signed by upstream; checked against the keyring `keys/<keyring>.gpg`
    /// or `keys/<keyring>.allowed_signers` (see [`Keyring`]).
    Signed { keyring: String },
    /// Upstream publishes no signatures; only the pin protects the source.
    PinnedOnly { reason: String },
}

#[derive(Debug, Clone)]
pub enum Origin {
    Archive {
        url: String,
        sha256: String,
        /// Cache file name, for URLs that do not end in a meaningful one.
        file: Option<String>,
        /// Detached signature URL; present exactly for signed sources.
        signature: Option<String>,
        signed: SignedData,
        /// The tarball has no top-level directory (e.g. the tz releases).
        flat: bool,
    },
    /// A git tag: annotated and signed, or – for projects that publish
    /// lightweight tags – pointing at a signed release commit.
    Git {
        url: String,
        tag: String,
        commit: String,
    },
}

/// What a detached signature covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignedData {
    /// The archive file as downloaded.
    #[default]
    Archive,
    /// The uncompressed tarball (kernel.org convention).
    Uncompressed,
}

/// An additional archive unpacked into a directory of the main source tree.
#[derive(Debug, Clone)]
pub struct Extra {
    pub source: Source,
    pub dest: String,
}

/// TOML representation shared by `[source]` and `[[extra]]` tables.
/// serde's `flatten` does not support `deny_unknown_fields`, so both are
/// parsed from this struct and validated by hand.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSource {
    url: Option<String>,
    sha256: Option<String>,
    file: Option<String>,
    signature: Option<String>,
    #[serde(default)]
    signed: SignedData,
    #[serde(default)]
    flat: bool,
    git: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    keyring: Option<String>,
    unsigned: Option<String>,
    dest: Option<String>,
    latest: Option<Latest>,
}

impl RawSource {
    pub fn into_source(self, version: &str) -> Result<Source> {
        ensure!(self.dest.is_none(), "dest is only valid in [[extra]]");
        self.build(version)
    }

    pub fn into_extra(mut self, version: &str) -> Result<Extra> {
        let dest = self.dest.take().context("[[extra]] needs dest")?;
        Ok(Extra {
            source: self.build(version)?,
            dest,
        })
    }

    fn build(self, version: &str) -> Result<Source> {
        let expand = |s: String| s.replace("{version}", version);
        let trust = match (self.keyring, self.unsigned) {
            (Some(keyring), None) => Trust::Signed { keyring },
            (None, Some(reason)) if !reason.trim().is_empty() => Trust::PinnedOnly { reason },
            (Some(_), Some(_)) => {
                bail!("a source is either signed (keyring) or unsigned, not both")
            }
            _ => bail!(
                "every source needs a keyring; a project without any signatures needs \
                 an explicit `unsigned = \"<reason>\"`"
            ),
        };
        let signed = matches!(trust, Trust::Signed { .. });

        let origin = match (self.url, self.git) {
            (Some(url), None) => {
                ensure!(
                    self.tag.is_none() && self.commit.is_none(),
                    "tag and commit belong to git sources"
                );
                let sha256 = self.sha256.context("archive sources need sha256")?;
                ensure!(is_hex(&sha256, 64), "sha256 must be 64 hex characters");
                ensure!(
                    self.signature.is_some() == signed,
                    "signed archive sources need a signature, unsigned ones must not have one"
                );
                Origin::Archive {
                    url: expand(url),
                    sha256,
                    file: self.file.map(expand),
                    signature: self.signature.map(expand),
                    signed: self.signed,
                    flat: self.flat,
                }
            }
            (None, Some(url)) => {
                ensure!(
                    self.sha256.is_none()
                        && self.file.is_none()
                        && self.signature.is_none()
                        && !self.flat,
                    "git sources are pinned by commit (and verified by their signed tag)"
                );
                let commit = self.commit.context("git sources need commit")?;
                ensure!(is_hex(&commit, 40), "commit must be 40 hex characters");
                Origin::Git {
                    url,
                    tag: expand(self.tag.context("git sources need tag")?),
                    commit,
                }
            }
            _ => bail!("a source needs exactly one of url or git"),
        };
        Ok(Source {
            origin,
            trust,
            latest: self.latest,
        })
    }
}

impl Source {
    /// The immutable identity of the source: archive digest or commit.
    pub fn pin(&self) -> &str {
        match &self.origin {
            Origin::Archive { sha256, .. } => sha256,
            Origin::Git { commit, .. } => commit,
        }
    }

    /// Directory name for the unpacked tree, e.g. `gcc-16.2.0` or `systemd-v262`.
    pub fn stem(&self) -> Result<String> {
        Ok(match &self.origin {
            Origin::Archive { .. } => {
                let name = self.file_name()?;
                tarball_stem(name).unwrap_or(name).to_owned()
            }
            Origin::Git { url, tag, .. } => format!("{}-{tag}", repo_name(url)),
        })
    }

    fn file_name(&self) -> Result<&str> {
        let Origin::Archive { url, file, .. } = &self.origin else {
            bail!("git sources have no file name");
        };
        match file {
            Some(file) => Ok(file),
            None => url
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .with_context(|| format!("{url} has no file name")),
        }
    }

    /// Downloads and verifies the source (once; later calls use the cache)
    /// and extracts it into `dir`. A single file (anything but a tarball,
    /// e.g. a certificate bundle) is copied into `dir` under its name.
    pub fn extract_into(&self, paths: &Paths, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        match &self.origin {
            Origin::Archive { .. } if tarball_stem(self.file_name()?).is_none() => {
                let file = self.fetch(paths, false)?;
                fs::copy(&file, dir.join(self.file_name()?))?;
                Ok(())
            }
            Origin::Archive { flat, .. } => {
                let archive = self.fetch(paths, false)?;
                util::run(
                    Command::new("tar")
                        .arg("--extract")
                        .arg("--file")
                        .arg(&archive)
                        .arg("--directory")
                        .arg(dir)
                        .arg(format!("--strip-components={}", u8::from(!flat))),
                )
            }
            Origin::Git { commit, .. } => {
                let clone = self.fetch(paths, false)?;
                let tar = Command::new("git")
                    .arg("-C")
                    .arg(&clone)
                    .args(["archive", "--format=tar", commit])
                    .stdout(Stdio::piped())
                    .spawn()
                    .context("starting git archive")?;
                pipe_into(
                    tar,
                    Command::new("tar")
                        .arg("--extract")
                        .arg("--directory")
                        .arg(dir),
                )
            }
        }
    }

    /// Makes sure the source is in the cache, pinned and signature-verified.
    /// With `force`, the signature is checked again even if it was before.
    /// Returns the archive file or the git clone.
    pub fn fetch(&self, paths: &Paths, force: bool) -> Result<PathBuf> {
        let keyring = match &self.trust {
            Trust::Signed { keyring } => Some(Keyring::find(&paths.root, keyring)?),
            Trust::PinnedOnly { .. } => None,
        };
        // Re-verify whenever the keyring changes.
        let verified_marker = match &keyring {
            Some(keyring) => format!("{} {}", self.pin(), util::sha256_file(keyring.path())?),
            None => format!("{} unsigned", self.pin()),
        };

        match &self.origin {
            Origin::Archive {
                url,
                sha256,
                signature,
                signed,
                ..
            } => {
                let archive = paths.cache.join(self.file_name()?);
                let marker = archive.with_extension("verified");
                if !archive.exists() || util::sha256_file(&archive)? != *sha256 {
                    download(url, &archive)?;
                    let actual = util::sha256_file(&archive)?;
                    if actual != *sha256 {
                        fs::remove_file(&archive)?;
                        bail!("checksum mismatch for {url}: expected {sha256}, got {actual}");
                    }
                }
                if force || fs::read_to_string(&marker).ok().as_deref() != Some(&verified_marker) {
                    if let (Some(keyring), Some(signature)) = (&keyring, signature) {
                        let Keyring::Gpg(keyring) = keyring else {
                            bail!("{url}: detached signatures are OpenPGP; use a .gpg keyring");
                        };
                        let sig = archive.with_extension("sig");
                        download(signature, &sig)?;
                        verify_detached(keyring, &sig, &archive, *signed)
                            .with_context(|| format!("verifying {url}"))?;
                    }
                    fs::write(&marker, &verified_marker)?;
                }
                Ok(archive)
            }
            Origin::Git { url, tag, commit } => {
                let clone = paths.cache.join("git").join(self.stem()?);
                let marker = with_suffix(&clone, "verified");
                if !clone.exists() {
                    util::step(&format!("Fetching {url} tag {tag}"));
                    // Fetch into a scratch directory and rename it once complete,
                    // so an interrupted fetch never looks like a finished one.
                    let partial = with_suffix(&clone, "partial");
                    if partial.exists() {
                        fs::remove_dir_all(&partial)?;
                    }
                    fs::create_dir_all(&partial)?;
                    let git = || {
                        let mut cmd = Command::new("git");
                        cmd.arg("-C").arg(&partial);
                        cmd
                    };
                    // Exactly the tag object and its commit; nothing is checked out.
                    util::run(git().args(["init", "--quiet"]))?;
                    util::run(
                        git()
                            .args(["fetch", "--quiet", "--depth=1", "--no-tags", url])
                            .arg(format!("refs/tags/{tag}:refs/tags/{tag}")),
                    )?;
                    fs::rename(&partial, &clone)?;
                }
                if force || fs::read_to_string(&marker).ok().as_deref() != Some(&verified_marker) {
                    verify_tag(keyring.as_ref(), &clone, tag, commit)
                        .with_context(|| format!("verifying {url} {tag}"))?;
                    fs::write(&marker, &verified_marker)?;
                }
                Ok(clone)
            }
        }
    }
}

fn download(url: &str, dest: &Path) -> Result<()> {
    util::step(&format!("Fetching {url}"));
    let partial = with_suffix(dest, "partial");
    util::run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--retry",
                "3",
                "--silent",
                "--show-error",
            ])
            .arg("--output")
            .arg(&partial)
            .arg(url),
    )?;
    fs::rename(&partial, dest)?;
    Ok(())
}

fn verify_detached(keyring: &Path, sig: &Path, archive: &Path, signed: SignedData) -> Result<()> {
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
fn verify_tag(keyring: Option<&Keyring>, clone: &Path, tag: &str, commit: &str) -> Result<()> {
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

fn pipe_into(producer: std::process::Child, consumer: &mut Command) -> Result<()> {
    let out = pipe_output(producer, consumer)?;
    ensure!(
        out.status.success(),
        "{consumer:?} failed with {}",
        out.status
    );
    Ok(())
}

/// Feeds the producer's stdout into the consumer and returns the consumer's output.
fn pipe_output(
    mut producer: std::process::Child,
    consumer: &mut Command,
) -> Result<std::process::Output> {
    let stdout = producer.stdout.take().context("producer stdout")?;
    let out = consumer
        .stdin(stdout)
        .output()
        .with_context(|| format!("starting {consumer:?}"))?;
    let status = producer.wait()?;
    ensure!(status.success(), "producer failed with {status}");
    Ok(out)
}

/// `path` with `.suffix` appended; unlike `with_extension`, keeps dots in the name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", path.display()))
}

fn repo_name(url: &str) -> &str {
    let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last)
}

/// The name without its tarball extension; `None` if it is no tarball.
fn tarball_stem(name: &str) -> Option<&str> {
    [".tar.xz", ".tar.gz", ".tar.zst", ".tar.bz2", ".tgz"]
        .iter()
        .find_map(|ext| name.strip_suffix(ext))
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "b4c5dfbe51a364a6c7f03869200f88c8e1f77403539005f14b7fc6bc91b8d8ba";
    const COMMIT: &str = "8cc40e0c5e9234bf45084751ac53b1fbfe70b492";

    fn raw(text: &str) -> RawSource {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn archive_source_expands_version() {
        let source = raw(&format!(
            "url = \"https://x/linux-{{version}}.tar.xz\"\nsha256 = \"{DIGEST}\"\n\
             signature = \"https://x/linux-{{version}}.tar.sign\"\nsigned = \"uncompressed\"\nkeyring = \"linux\"\n"
        ))
        .into_source("7.2.9")
        .unwrap();
        let Origin::Archive {
            url,
            signature,
            signed,
            ..
        } = &source.origin
        else {
            panic!("expected archive")
        };
        assert_eq!(url, "https://x/linux-7.2.9.tar.xz");
        assert_eq!(signature.as_deref(), Some("https://x/linux-7.2.9.tar.sign"));
        assert_eq!(*signed, SignedData::Uncompressed);
        assert_eq!(source.stem().unwrap(), "linux-7.2.9");
        assert_eq!(source.pin(), DIGEST);
    }

    #[test]
    fn git_source_is_pinned_by_commit() {
        let source = raw(&format!(
            "git = \"https://github.com/systemd/systemd.git\"\ntag = \"v{{version}}\"\n\
             commit = \"{COMMIT}\"\nkeyring = \"systemd\"\n"
        ))
        .into_source("262")
        .unwrap();
        assert_eq!(source.stem().unwrap(), "systemd-v262");
        assert_eq!(source.pin(), COMMIT);
    }

    #[test]
    fn sources_without_signature_or_reason_are_rejected() {
        // A keyring without a signature to check.
        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nkeyring = \"a\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("signature"), "{err}");

        // Neither a keyring nor an explicit reason.
        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nsignature = \"https://x/a.sig\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("keyring"), "{err}");

        // An empty reason does not count.
        assert!(
            raw(&format!(
                "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nunsigned = \" \"\n"
            ))
            .into_source("1")
            .is_err()
        );
    }

    #[test]
    fn unsigned_sources_need_a_reason_and_nothing_to_verify() {
        let source = raw(&format!(
            "git = \"https://github.com/json-c/json-c.git\"\ntag = \"json-c-{{version}}\"\n\
             commit = \"{COMMIT}\"\nunsigned = \"upstream publishes no signatures\"\n"
        ))
        .into_source("0.19")
        .unwrap();
        assert_eq!(
            source.trust,
            Trust::PinnedOnly {
                reason: "upstream publishes no signatures".to_owned()
            }
        );

        // A signature without a keyring to check it against makes no sense.
        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nsignature = \"https://x/a.sig\"\n\
             unsigned = \"why\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("signature"), "{err}");

        // Signed and unsigned at once is contradictory.
        assert!(
            raw(&format!(
                "git = \"https://x/a.git\"\ntag = \"v1\"\ncommit = \"{COMMIT}\"\n\
                 keyring = \"a\"\nunsigned = \"why\"\n"
            ))
            .into_source("1")
            .is_err()
        );
    }

    #[test]
    fn extras_need_dest_and_sources_reject_it() {
        let text = format!(
            "url = \"https://x/gmp.tar.xz\"\nsha256 = \"{DIGEST}\"\nsignature = \"https://x/gmp.sig\"\n\
             keyring = \"gmp\"\ndest = \"gmp\"\n"
        );
        assert_eq!(raw(&text).into_extra("1").unwrap().dest, "gmp");
        assert!(raw(&text).into_source("1").is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(toml::from_str::<RawSource>("url = \"x\"\nsha265 = \"typo\"\n").is_err());
    }

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

    #[test]
    fn single_files_are_sources_too() {
        let source = raw(&format!(
            "url = \"https://x/cacert-{{version}}.pem\"\nsha256 = \"{DIGEST}\"\nunsigned = \"why\"\n"
        ))
        .into_source("2026-09-01")
        .unwrap();
        assert_eq!(source.stem().unwrap(), "cacert-2026-09-01.pem");
        assert_eq!(tarball_stem("a-1.tar.xz"), Some("a-1"));
        assert_eq!(tarball_stem("cacert.pem"), None);
    }
}
