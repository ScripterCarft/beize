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
//!
//! Everything is fetched over HTTPS only, redirects included: the pin keeps
//! a source intact, HTTPS keeps others from seeing or steering what is
//! fetched.

mod verify;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

pub use self::verify::Keyring;
use self::verify::{verify_detached, verify_tag};
use crate::paths::Paths;
use crate::tree;
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
    /// A package on npm's registry, for projects that publish there only
    /// and that release-monitoring.org does not list (Anitya has the same
    /// backend): `{ npm = "@phosphor-icons/core" }`.
    Npm(String),
    /// A project's latest release on GitHub, for projects that publish
    /// there only and that release-monitoring.org does not list (Anitya
    /// has a GitHub backend too): `{ github = "ful1e5/Bibata_Cursor" }`.
    Github(String),
    /// A URL that always serves upstream's current file; newer when its
    /// SHA-256 differs from the pin: `{ file = "https://…" }`.
    File(String),
    /// An `[[extra]]` the main source pins itself, by a file of it that
    /// names the archive's SHA-256 (a Meson wrap): it moves with the main
    /// source, and unpacking fails when the recipe's pin differs:
    /// `{ pinned-by = "subprojects/syn-2-rs.wrap" }`.
    PinnedBy(String),
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
        /// The tree has git submodules (`submodules = true`). Their commits
        /// are part of the pinned, signed tree, so they need no pin of their
        /// own; they are fetched by exactly that commit.
        submodules: bool,
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
    #[serde(default)]
    submodules: bool,
    keyring: Option<String>,
    unsigned: Option<String>,
    dest: Option<String>,
    latest: Option<Latest>,
}

impl RawSource {
    pub fn into_source(self, version: &str) -> Result<Source> {
        ensure!(self.dest.is_none(), "dest is only valid in [[extra]]");
        ensure!(
            !matches!(self.latest, Some(Latest::PinnedBy(_))),
            "pinned-by is only valid in [[extra]]"
        );
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
        let urls = [&self.url, &self.signature, &self.git];
        if let Some(url) = urls.into_iter().flatten().find(|url| !is_https(url)) {
            bail!("{url}: sources are fetched over HTTPS only");
        }
        if let Some(Latest::File(url)) = &self.latest {
            ensure!(is_https(url), "{url}: sources are fetched over HTTPS only");
        }

        let origin = match (self.url, self.git) {
            (Some(url), None) => {
                ensure!(
                    self.tag.is_none() && self.commit.is_none() && !self.submodules,
                    "tag, commit and submodules belong to git sources"
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
                    submodules: self.submodules,
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
            Origin::Git {
                commit, submodules, ..
            } => {
                let clone = self.fetch(paths, false)?;
                extract_commit(&clone, commit, dir)?;
                if *submodules {
                    for (path, sub) in gitlinks(&clone, commit)? {
                        extract_commit(&clone, &sub, &dir.join(&path))?;
                    }
                }
                Ok(())
            }
        }
    }

    /// What `fetch` keeps of the source in the cache: the archive with its
    /// signature and its mark of verification, or the git clone with its
    /// mark.
    pub fn cache_entries(&self, paths: &Paths) -> Result<Vec<PathBuf>> {
        Ok(match &self.origin {
            Origin::Archive { .. } => {
                let archive = paths.cache.join(self.file_name()?);
                vec![
                    archive.with_extension("verified"),
                    archive.with_extension("sig"),
                    archive,
                ]
            }
            Origin::Git { .. } => {
                let clone = paths.cache.join("git").join(self.stem()?);
                vec![with_suffix(&clone, "verified"), clone]
            }
        })
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
            Origin::Git {
                url,
                tag,
                commit,
                submodules,
            } => {
                let clone = paths.cache.join("git").join(self.stem()?);
                let marker = with_suffix(&clone, "verified");
                if !clone.exists() {
                    util::step(&format!("Fetching {url} tag {tag}"));
                    // Fetch into a scratch directory and rename it once complete,
                    // so an interrupted fetch never looks like a finished one.
                    let partial = with_suffix(&clone, "partial");
                    tree::recreate(&partial)?;
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
                    if *submodules {
                        fetch_submodules(&partial, url, commit)?;
                    }
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

/// Extracts the tree of `commit` in the clone into `dir`.
fn extract_commit(clone: &Path, commit: &str, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let tar = Command::new("git")
        .arg("-C")
        .arg(clone)
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

/// The submodules of `commit`: path and the commit the tree records for it.
fn gitlinks(clone: &Path, commit: &str) -> Result<Vec<(String, String)>> {
    let tree = util::output(
        Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(["ls-tree", "-r", commit]),
    )?;
    let links: Vec<_> = tree
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut meta = meta.split(' ');
            if meta.next() != Some("160000") {
                return None;
            }
            Some((path.to_owned(), meta.nth(1)?.to_owned()))
        })
        .collect();
    ensure!(!links.is_empty(), "{commit} has no submodules");
    Ok(links)
}

/// Fetches every submodule of `commit` into the clone, each by the exact
/// commit the (signed) tree records. Nested submodules are refused: they
/// would silently be missing from the extracted tree.
fn fetch_submodules(clone: &Path, url: &str, commit: &str) -> Result<()> {
    let git = || {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(clone);
        cmd
    };
    let gitmodules = format!("{commit}:.gitmodules");
    for (path, sub) in gitlinks(clone, commit)? {
        let names = util::output(git().args([
            "config",
            "--blob",
            &gitmodules,
            "--get-regexp",
            r"^submodule\..*\.path$",
        ]))?;
        let name = names
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(' ')?;
                (value == path).then(|| key.strip_prefix("submodule.")?.strip_suffix(".path"))?
            })
            .with_context(|| format!("submodule {path} is not in .gitmodules"))?;
        let sub_url = util::output(git().args([
            "config",
            "--blob",
            &gitmodules,
            "--get",
            &format!("submodule.{name}.url"),
        ]))?;
        let sub_url = resolve_submodule_url(url, &sub_url);
        ensure!(
            is_https(&sub_url),
            "submodule {path}: {sub_url} is not fetched over HTTPS"
        );
        util::step(&format!("Fetching submodule {path} ({sub_url} {sub})"));
        util::run(git().args(["fetch", "--quiet", "--depth=1", "--no-tags", &sub_url, &sub]))?;
        let nested = util::output(git().args(["ls-tree", "-r", &sub]))?;
        ensure!(
            !nested.lines().any(|line| line.starts_with("160000 ")),
            "submodule {path} has submodules of its own"
        );
    }
    Ok(())
}

/// A submodule URL as git resolves it: `../x` and `./x` are relative to the
/// superproject's URL, each `../` dropping one path component of it.
fn resolve_submodule_url(base: &str, url: &str) -> String {
    if !url.starts_with("../") && !url.starts_with("./") {
        return url.to_owned();
    }
    let mut base = base.trim_end_matches('/').to_owned();
    let mut rest = url;
    loop {
        if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("../") {
            rest = r;
            base.truncate(base.rfind('/').unwrap_or(0));
        } else {
            break;
        }
    }
    format!("{base}/{rest}")
}

fn download(url: &str, dest: &Path) -> Result<()> {
    util::step(&format!("Fetching {url}"));
    let partial = with_suffix(dest, "partial");
    util::run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
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
pub(super) fn pipe_output(
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
pub(super) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
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

fn is_https(url: &str) -> bool {
    url.starts_with("https://")
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
    fn submodule_urls_resolve_like_git() {
        let glib = "https://gitlab.gnome.org/GNOME/glib.git";
        assert_eq!(
            resolve_submodule_url(glib, "../../GNOME/gvdb.git"),
            "https://gitlab.gnome.org/GNOME/gvdb.git"
        );
        assert_eq!(
            resolve_submodule_url(glib, "../gvdb.git"),
            "https://gitlab.gnome.org/GNOME/gvdb.git"
        );
        assert_eq!(
            resolve_submodule_url(glib, "https://example.org/x.git"),
            "https://example.org/x.git"
        );
    }

    #[test]
    fn submodules_belong_to_git_sources() {
        let err = raw(&format!(
            "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nsubmodules = true\nunsigned = \"r\"\n"
        ))
        .into_source("1")
        .unwrap_err();
        assert!(err.to_string().contains("submodules"));
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
    fn sources_are_fetched_over_https_only() {
        for text in [
            format!("url = \"http://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\nunsigned = \"r\"\n"),
            format!(
                "url = \"https://x/a.tar.xz\"\nsha256 = \"{DIGEST}\"\n\
                 signature = \"ftp://x/a.sig\"\nkeyring = \"a\"\n"
            ),
            format!(
                "git = \"git://x/a.git\"\ntag = \"v1\"\ncommit = \"{COMMIT}\"\nunsigned = \"r\"\n"
            ),
            format!(
                "url = \"https://x/a.pem\"\nsha256 = \"{DIGEST}\"\nunsigned = \"r\"\n\
                 latest = {{ file = \"http://x/a.pem\" }}\n"
            ),
        ] {
            let err = raw(&text).into_source("1").unwrap_err();
            assert!(err.to_string().contains("HTTPS"), "{text}: {err}");
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(toml::from_str::<RawSource>("url = \"x\"\nsha265 = \"typo\"\n").is_err());
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
