use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::paths::Paths;
#[cfg(test)]
use crate::source::Trust;
use crate::source::{Extra, RawSource, Source};
use crate::{say, util};

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
#[derive(Debug)]
pub struct Recipe {
    pub name: String,
    pub version: String,
    pub stage: Option<Stage>,
    pub deps: Vec<String>,
    pub source: Option<Source>,
    /// Reuse the version and sources of another recipe (e.g. kernel headers).
    pub source_from: Option<String>,
    /// Additional archives unpacked into the main source tree.
    pub extra: Vec<Extra>,
    /// Paths in this repository the recipe builds from (Beize's own code);
    /// their content is part of the build fingerprint.
    pub repo: Vec<String>,
    /// Beize's own changes to the source, applied in order after unpacking
    /// (`patch -p1`): paths in this repository, under recipes/patches/<name>/.
    /// Only with a reason in the recipe – upstream first.
    pub patches: Vec<String>,
    pub build: Option<Build>,
    /// The recipe text, part of the build fingerprint – without the `latest`
    /// lines: where releases are announced does not change the build.
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Shell script run with `bash -euo pipefail` inside a fresh build directory.
    pub script: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    name: String,
    #[serde(default)]
    version: String,
    stage: Option<Stage>,
    #[serde(default)]
    deps: Vec<String>,
    source: Option<RawSource>,
    source_from: Option<String>,
    #[serde(default)]
    extra: Vec<RawSource>,
    #[serde(default)]
    repo: Vec<String>,
    #[serde(default)]
    patches: Vec<String>,
    build: Option<Build>,
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

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let raw: RawRecipe = toml::from_str(text)?;
        if raw.source.is_some() && raw.source_from.is_some() {
            bail!("source and source_from are mutually exclusive");
        }
        if !raw.patches.is_empty() && raw.source.is_none() {
            bail!("patches need a [source] of the recipe's own");
        }
        if let Some(patch) = raw.patches.iter().find(|p| {
            !p.starts_with(&format!("recipes/patches/{}/", raw.name)) || !p.ends_with(".patch")
        }) {
            bail!(
                "patch {patch:?} is not a .patch file under recipes/patches/{}/",
                raw.name
            );
        }
        let source = raw
            .source
            .map(|s| s.into_source(&raw.version))
            .transpose()
            .context("in [source]")?;
        let extra = raw
            .extra
            .into_iter()
            .map(|s| s.into_extra(&raw.version))
            .collect::<Result<_>>()
            .context("in [[extra]]")?;
        Ok(Self {
            name: raw.name,
            version: raw.version,
            stage: raw.stage,
            deps: raw.deps,
            source,
            source_from: raw.source_from,
            extra,
            repo: raw.repo,
            patches: raw.patches,
            build: raw.build,
            text: text
                .split_inclusive('\n')
                .filter(|line| !line.trim_start().starts_with("latest = "))
                .collect(),
        })
    }

    /// All sources of this recipe: the main one and its extras.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.source
            .iter()
            .chain(self.extra.iter().map(|e| &e.source))
    }

    /// Digest over the paths and contents of the recipe's patches; empty
    /// without any.
    pub fn patch_digest(&self, paths: &Paths) -> Result<String> {
        if self.patches.is_empty() {
            return Ok(String::new());
        }
        let mut hasher = Sha256::new();
        for patch in &self.patches {
            let content = fs::read(paths.root.join(patch))
                .with_context(|| format!("{}: reading patch {patch}", self.name))?;
            hasher.update(format!("\npatch {patch} {}\n", content.len()));
            hasher.update(content);
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    /// Unpacks the sources into `work/src/<stem>` and returns that directory.
    /// Recipes sharing a source share the tree; builds always happen out of
    /// tree. A patched tree is the recipe's own (`<stem>+patched`).
    pub fn unpack(&self, paths: &Paths) -> Result<PathBuf> {
        let source = self
            .source
            .as_ref()
            .with_context(|| format!("{} has no source", self.name))?;
        let patches = self.patch_digest(paths)?;
        let mut stem = source.stem()?;
        if !patches.is_empty() {
            stem.push_str("+patched");
        }
        let dir = paths.work.join("src").join(stem);
        let stamp = dir.join(".beize-unpacked");
        let mut fingerprint = self
            .sources()
            .map(Source::pin)
            .collect::<Vec<_>>()
            .join("\n");
        if !patches.is_empty() {
            fingerprint.push_str(&format!("\npatches {patches}"));
        }
        if fs::read_to_string(&stamp).is_ok_and(|s| s == fingerprint) {
            return Ok(dir);
        }

        util::step(&format!("Unpacking {} {}", self.name, self.version));
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        source.extract_into(paths, &dir)?;
        for extra in &self.extra {
            extra.source.extract_into(paths, &dir.join(&extra.dest))?;
        }
        for patch in &self.patches {
            say!("  applying {patch}");
            util::run(
                Command::new("patch")
                    .args(["-p1", "--batch", "--forward", "--quiet", "-d"])
                    .arg(&dir)
                    .arg("-i")
                    .arg(paths.root.join(patch)),
            )
            .with_context(|| format!("{patch} does not apply to {} {}", self.name, self.version))?;
        }
        fs::write(&stamp, fingerprint)?;
        Ok(dir)
    }
}

/// Recipes in `recipes/`, sorted by name.
pub fn all_names(paths: &Paths) -> Result<Vec<String>> {
    names_in(&paths.root.join("recipes"))
}

fn names_in(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "b4c5dfbe51a364a6c7f03869200f88c8e1f77403539005f14b7fc6bc91b8d8ba";

    fn signed(url: &str, keyring: &str) -> String {
        format!(
            "url = \"{url}\"\nsha256 = \"{DIGEST}\"\nsignature = \"{url}.sig\"\nkeyring = \"{keyring}\"\n"
        )
    }

    #[test]
    fn parses_extras_and_deps() {
        let recipe = Recipe::parse(&format!(
            "name = \"gcc\"\nversion = \"16\"\nstage = \"toolchain\"\ndeps = [\"glibc\"]\n\
             [source]\n{}\n[[extra]]\n{}dest = \"gmp\"\n",
            signed("https://x/gcc-{version}.tar.xz", "gcc"),
            signed("https://x/gmp.tar.xz", "gmp"),
        ))
        .unwrap();
        assert_eq!(recipe.stage, Some(Stage::Toolchain));
        assert_eq!(recipe.deps, ["glibc"]);
        assert_eq!(recipe.extra[0].dest, "gmp");
        assert_eq!(recipe.sources().count(), 2);
        assert_eq!(recipe.source.unwrap().stem().unwrap(), "gcc-16");
    }

    #[test]
    fn where_releases_are_announced_is_not_part_of_the_build() {
        let plain = format!(
            "name = \"a\"\n[source]\n{}",
            signed("https://x/a.tar.xz", "a")
        );
        let monitored = plain.replace(
            "[source]\n",
            "[source]\nlatest = { release-monitoring = 1 }\n",
        );
        let recipe = Recipe::parse(&monitored).unwrap();
        assert_eq!(recipe.text, plain);
        assert_eq!(
            recipe.source.unwrap().latest,
            Some(crate::source::Latest::ReleaseMonitoring(1))
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Recipe::parse("name = \"a\"\npatchez = []\n").is_err());
    }

    #[test]
    fn patches_live_under_the_recipes_own_directory() {
        let source = format!(
            "[source]\nurl = \"https://example.org/a.tar.xz\"\nsha256 = \"{}\"\nkeyring = \"a\"\n\
             signature = \"https://example.org/a.tar.xz.asc\"\n",
            "0".repeat(64)
        );
        let with = |patch: &str| {
            Recipe::parse(&format!("name = \"a\"\npatches = [\"{patch}\"]\n{source}"))
        };
        assert!(with("recipes/patches/a/0001-x.patch").is_ok());
        assert!(with("recipes/patches/b/0001-x.patch").is_err());
        assert!(with("recipes/patches/a/0001-x.diff").is_err());
        assert!(with("/tmp/0001-x.patch").is_err());
        // Without a source of its own there is nothing to patch.
        assert!(
            Recipe::parse("name = \"a\"\npatches = [\"recipes/patches/a/0001-x.patch\"]\n")
                .is_err()
        );
    }

    #[test]
    fn all_repository_recipes_parse_and_have_keyrings() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let names = names_in(&root.join("recipes")).unwrap();
        assert!(!names.is_empty());
        for name in names {
            let path = root.join("recipes").join(format!("{name}.toml"));
            let recipe = Recipe::parse(&fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
            assert_eq!(recipe.name, name, "{}", path.display());
            for source in recipe.sources() {
                if let Trust::Signed { keyring } = &source.trust
                    && let Err(e) = crate::source::Keyring::find(&root, keyring)
                {
                    panic!("{name}: {e}");
                }
            }
            // `bz outdated` must be able to check every source: the main one
            // names where releases are announced, an extra too unless it is
            // part of the same release (its file carries the version).
            if let Some(source) = &recipe.source {
                assert!(source.latest.is_some(), "{name}: [source] needs `latest`");
            }
            for extra in &recipe.extra {
                assert!(
                    extra.source.latest.is_some()
                        || extra.source.stem().unwrap().contains(&recipe.version),
                    "{name}: [[extra]] {} needs `latest`",
                    extra.dest
                );
            }
        }
    }
}
