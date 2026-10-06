//! Upstream sources: download, pin and verify.
//!
//! Every source is pinned (SHA-256 for archives, commit for git tags) and
//! signed by its upstream maintainers. Signatures are checked with `gpgv`
//! against a keyring committed under `keys/`, so a source can only enter the
//! build if both the pin and the maintainer's signature match.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::paths::Paths;
use crate::util;

#[derive(Debug, Clone)]
pub struct Source {
    pub origin: Origin,
    /// Name of the keyring in `keys/` that holds the maintainers' keys.
    pub keyring: String,
}

#[derive(Debug, Clone)]
pub enum Origin {
    Archive {
        url: String,
        sha256: String,
        /// Cache file name, for URLs that do not end in a meaningful one.
        file: Option<String>,
        /// Detached signature URL.
        signature: String,
        signed: SignedData,
    },
    /// An annotated, signed git tag.
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
    git: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    keyring: Option<String>,
    dest: Option<String>,
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
        let keyring = self
            .keyring
            .context("every source needs a keyring; unsigned sources are not accepted")?;

        let origin = match (self.url, self.git) {
            (Some(url), None) => {
                ensure!(
                    self.tag.is_none() && self.commit.is_none(),
                    "tag and commit belong to git sources"
                );
                let sha256 = self.sha256.context("archive sources need sha256")?;
                ensure!(is_hex(&sha256, 64), "sha256 must be 64 hex characters");
                Origin::Archive {
                    url: expand(url),
                    sha256,
                    file: self.file.map(expand),
                    signature: expand(self.signature.context("archive sources need signature")?),
                    signed: self.signed,
                }
            }
            (None, Some(url)) => {
                ensure!(
                    self.sha256.is_none() && self.file.is_none() && self.signature.is_none(),
                    "git sources are pinned by commit and verified by their signed tag"
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
        Ok(Source { origin, keyring })
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
                [".tar.xz", ".tar.gz", ".tar.zst", ".tar.bz2", ".tgz"]
                    .iter()
                    .find_map(|ext| name.strip_suffix(ext))
                    .unwrap_or(name)
                    .to_owned()
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
    /// and extracts it into `dir`.
    pub fn extract_into(&self, paths: &Paths, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        match &self.origin {
            Origin::Archive { .. } => {
                let archive = self.fetch(paths, false)?;
                util::run(
                    Command::new("tar")
                        .arg("--extract")
                        .arg("--file")
                        .arg(&archive)
                        .arg("--directory")
                        .arg(dir)
                        .arg("--strip-components=1"),
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
        let keyring = paths
            .root
            .join("keys")
            .join(format!("{}.gpg", self.keyring));
        ensure!(keyring.exists(), "keyring {} is missing", keyring.display());
        // Re-verify whenever the keyring changes.
        let verified_marker = format!("{} {}", self.pin(), util::sha256_file(&keyring)?);

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
                    let sig = archive.with_extension("sig");
                    download(signature, &sig)?;
                    verify_detached(&keyring, &sig, &archive, *signed)
                        .with_context(|| format!("verifying {url}"))?;
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
                    verify_tag(&keyring, &clone, tag, commit)
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

/// Checks that `tag` is signed by a key in `keyring` and points at `commit`.
fn verify_tag(keyring: &Path, clone: &Path, tag: &str, commit: &str) -> Result<()> {
    let git = |args: &[&str]| util::output(Command::new("git").arg("-C").arg(clone).args(args));
    let actual = git(&["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])?;
    ensure!(
        actual == commit,
        "tag {tag} points at {actual}, expected {commit}"
    );

    let raw = git(&["cat-file", "tag", tag])?;
    let (payload, signature) =
        split_tag_signature(&raw).with_context(|| format!("tag {tag} is not signed"))?;
    let dir = with_suffix(clone, "tagcheck");
    fs::create_dir_all(&dir)?;
    let (payload_path, sig_path) = (dir.join("payload"), dir.join("payload.asc"));
    fs::write(&payload_path, payload)?;
    fs::write(&sig_path, signature)?;
    let result = Command::new("gpgv")
        .arg("--keyring")
        .arg(keyring)
        .arg(&sig_path)
        .arg(&payload_path)
        .output()
        .context("starting gpgv");
    fs::remove_dir_all(&dir)?;
    check_gpgv(result?)
}

/// Splits `git cat-file tag` output into the signed payload and the signature.
fn split_tag_signature(raw: &str) -> Option<(String, String)> {
    let start = raw.find("-----BEGIN PGP SIGNATURE-----")?;
    // `util::output` trims the trailing newline that belongs to the signature.
    Some((raw[..start].to_owned(), format!("{}\n", &raw[start..])))
}

fn check_gpgv(out: std::process::Output) -> Result<()> {
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("signature check failed:\n{}", log.trim());
    }
    if let Some(good) = log.lines().find(|l| l.contains("Good signature")) {
        eprintln!("  {}", good.trim_start_matches("gpgv: "));
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
        assert_eq!(signature, "https://x/linux-7.2.9.tar.sign");
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
    fn unsigned_sources_are_rejected() {
        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nkeyring = \"a\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("signature"), "{err}");

        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nsignature = \"https://x/a.sig\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("keyring"), "{err}");
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
        let raw = "object abc\ntype commit\ntag v262\n\nsystemd v262\n-----BEGIN PGP SIGNATURE-----\n\nxyz\n-----END PGP SIGNATURE-----";
        let (payload, sig) = split_tag_signature(raw).unwrap();
        assert_eq!(
            payload,
            "object abc\ntype commit\ntag v262\n\nsystemd v262\n"
        );
        assert!(sig.starts_with("-----BEGIN PGP SIGNATURE-----"));
        assert!(sig.ends_with("-----END PGP SIGNATURE-----\n"));
        assert!(split_tag_signature("object abc\n\nunsigned").is_none());
    }
}
