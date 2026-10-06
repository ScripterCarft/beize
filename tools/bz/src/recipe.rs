use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::paths::Paths;
use crate::util;

/// Where a recipe installs its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Runs on the build machine; installs straight into `build/tools`.
    Toolchain,
    /// Runs on Beize; installs into a staging directory that is merged into
    /// the sysroot and recorded in a manifest.
    Target,
}

/// A package description, read from `recipes/<name>.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub stage: Option<Stage>,
    #[serde(default)]
    pub deps: Vec<String>,
    pub source: Option<Source>,
    /// Reuse the version and sources of another recipe (e.g. kernel headers).
    pub source_from: Option<String>,
    /// Additional archives unpacked into the main source tree.
    #[serde(default)]
    pub extra: Vec<Extra>,
    pub build: Option<Build>,
    /// Raw recipe text; part of the build fingerprint.
    #[serde(skip)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub url: String,
    pub sha256: String,
    /// Cache file name, for URLs that do not end in a meaningful one.
    pub file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Shell script run with `bash -euo pipefail` inside a fresh build directory.
    pub script: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extra {
    #[serde(flatten)]
    pub source: Source,
    /// Directory inside the main source tree to unpack into.
    pub dest: String,
}

impl Recipe {
    pub fn load(paths: &Paths, name: &str) -> Result<Self> {
        let path = paths.recipe_file(name);
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut recipe =
            Self::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        if recipe.name != name {
            bail!("{} declares name {:?}", path.display(), recipe.name);
        }
        if let Some(other) = recipe.source_from.clone() {
            let base = Self::load(paths, &other)?;
            recipe.version = base.version;
            recipe.source = base.source;
            recipe.extra = base.extra;
        }
        Ok(recipe)
    }

    fn parse(text: &str) -> Result<Self> {
        let mut recipe: Self = toml::from_str(text)?;
        recipe.text = text.to_owned();
        if recipe.source.is_some() && recipe.source_from.is_some() {
            bail!("source and source_from are mutually exclusive");
        }
        let version = recipe.version.clone();
        for source in recipe
            .source
            .iter_mut()
            .chain(recipe.extra.iter_mut().map(|e| &mut e.source))
        {
            source.expand(&version)?;
        }
        Ok(recipe)
    }

    /// Unpacks the sources into `work/src/<archive>` and returns that directory.
    /// Recipes sharing an archive share the tree; builds always happen out of tree.
    pub fn unpack(&self, paths: &Paths) -> Result<PathBuf> {
        let source = self
            .source
            .as_ref()
            .with_context(|| format!("{} has no source", self.name))?;
        let dir = paths.work.join("src").join(source.stem()?);
        let stamp = dir.join(".beize-unpacked");
        let fingerprint = std::iter::once(source)
            .chain(self.extra.iter().map(|e| &e.source))
            .map(|s| s.sha256.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if fs::read_to_string(&stamp).is_ok_and(|s| s == fingerprint) {
            return Ok(dir);
        }

        util::step(&format!("Unpacking {} {}", self.name, self.version));
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        extract(&source.fetch(paths)?, &dir)?;
        for extra in &self.extra {
            extract(&extra.source.fetch(paths)?, &dir.join(&extra.dest))?;
        }
        fs::write(&stamp, fingerprint)?;
        Ok(dir)
    }
}

impl Source {
    fn expand(&mut self, version: &str) -> Result<()> {
        self.url = self.url.replace("{version}", version);
        self.file = self.file.as_ref().map(|f| f.replace("{version}", version));
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("sha256 of {} must be a 64 character hex digest", self.url);
        }
        Ok(())
    }

    fn file_name(&self) -> Result<&str> {
        match &self.file {
            Some(file) => Ok(file),
            None => self
                .url
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .with_context(|| format!("{} has no file name", self.url)),
        }
    }

    fn stem(&self) -> Result<&str> {
        let name = self.file_name()?;
        Ok([".tar.xz", ".tar.gz", ".tar.zst", ".tar.bz2", ".tgz"]
            .iter()
            .find_map(|ext| name.strip_suffix(ext))
            .unwrap_or(name))
    }

    /// Downloads the archive into the cache and verifies its digest.
    pub fn fetch(&self, paths: &Paths) -> Result<PathBuf> {
        let archive = paths.cache.join(self.file_name()?);
        if archive.exists() && util::sha256_file(&archive)? == self.sha256 {
            return Ok(archive);
        }

        util::step(&format!("Fetching {}", self.url));
        let partial = archive.with_extension("partial");
        util::run(
            Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--retry",
                    "3",
                    "--progress-bar",
                    "--output",
                ])
                .arg(&partial)
                .arg(&self.url),
        )?;
        let actual = util::sha256_file(&partial)?;
        if actual != self.sha256 {
            fs::remove_file(&partial)?;
            bail!(
                "checksum mismatch for {}: expected {}, got {actual}",
                self.url,
                self.sha256
            );
        }
        fs::rename(&partial, &archive)?;
        Ok(archive)
    }
}

fn extract(archive: &Path, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    util::run(
        Command::new("tar")
            .arg("--extract")
            .arg("--file")
            .arg(archive)
            .arg("--directory")
            .arg(dir)
            .arg("--strip-components=1"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "b4c5dfbe51a364a6c7f03869200f88c8e1f77403539005f14b7fc6bc91b8d8ba";

    fn parse(body: &str) -> Result<Recipe> {
        Recipe::parse(body)
    }

    #[test]
    fn expands_version_in_url_and_file() {
        let recipe = parse(&format!(
            "name = \"systemd\"\nversion = \"262\"\n[source]\nurl = \"https://x/v{{version}}.tar.gz\"\nfile = \"systemd-{{version}}.tar.gz\"\nsha256 = \"{DIGEST}\"\n"
        ))
        .unwrap();
        let source = recipe.source.unwrap();
        assert_eq!(source.url, "https://x/v262.tar.gz");
        assert_eq!(source.file_name().unwrap(), "systemd-262.tar.gz");
        assert_eq!(source.stem().unwrap(), "systemd-262");
    }

    #[test]
    fn parses_extras_and_deps() {
        let recipe = parse(&format!(
            "name = \"gcc\"\nversion = \"16\"\nstage = \"toolchain\"\ndeps = [\"glibc\"]\n\
             [source]\nurl = \"https://x/gcc-{{version}}.tar.xz\"\nsha256 = \"{DIGEST}\"\n\
             [[extra]]\nurl = \"https://x/gmp.tar.xz\"\nsha256 = \"{DIGEST}\"\ndest = \"gmp\"\n"
        ))
        .unwrap();
        assert_eq!(recipe.stage, Some(Stage::Toolchain));
        assert_eq!(recipe.deps, ["glibc"]);
        assert_eq!(recipe.extra[0].dest, "gmp");
    }

    #[test]
    fn rejects_malformed_digest() {
        let err = parse("name = \"a\"\nversion = \"1\"\n[source]\nurl = \"https://x/a.tar\"\nsha256 = \"abc\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("sha256"));
    }

    #[test]
    fn all_repository_recipes_parse() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../recipes");
        let mut count = 0;
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let text = fs::read_to_string(&path).unwrap();
            let recipe = parse(&text).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            let stem = path.file_stem().unwrap().to_str().unwrap();
            assert_eq!(recipe.name, stem, "{}", path.display());
            count += 1;
        }
        assert!(count > 0);
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(parse("name = \"a\"\npatches = []\n").is_err());
    }
}
