use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::paths::Paths;
use crate::source::{Extra, RawSource, Source};
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
    pub build: Option<Build>,
    /// Raw recipe text; part of the build fingerprint.
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

    fn parse(text: &str) -> Result<Self> {
        let raw: RawRecipe = toml::from_str(text)?;
        if raw.source.is_some() && raw.source_from.is_some() {
            bail!("source and source_from are mutually exclusive");
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
            build: raw.build,
            text: text.to_owned(),
        })
    }

    /// All sources of this recipe: the main one and its extras.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.source
            .iter()
            .chain(self.extra.iter().map(|e| &e.source))
    }

    /// Unpacks the sources into `work/src/<stem>` and returns that directory.
    /// Recipes sharing a source share the tree; builds always happen out of tree.
    pub fn unpack(&self, paths: &Paths) -> Result<PathBuf> {
        let source = self
            .source
            .as_ref()
            .with_context(|| format!("{} has no source", self.name))?;
        let dir = paths.work.join("src").join(source.stem()?);
        let stamp = dir.join(".beize-unpacked");
        let fingerprint = self
            .sources()
            .map(Source::pin)
            .collect::<Vec<_>>()
            .join("\n");
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
    fn rejects_unknown_fields() {
        assert!(Recipe::parse("name = \"a\"\npatches = []\n").is_err());
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
                let keyring = root.join("keys").join(format!("{}.gpg", source.keyring));
                assert!(keyring.exists(), "{name}: missing {}", keyring.display());
            }
        }
    }
}
