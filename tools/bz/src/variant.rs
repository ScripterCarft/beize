//! The variants of Beize (ADR 0025): one build, several images. `base` is a
//! complete system without a user interface; `desktop` extends it. Each is
//! a directory in `images/` (see `images/desktop/variant.toml`); a variant
//! that extends another adds to it and changes nothing of it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::tree;

/// What `bz` builds unless told otherwise: Beize for a computer.
pub const DEFAULT: &str = "desktop";

/// The presets of a variant's system units and of every user's units (ADR 0013).
const SYSTEM_PRESET: &str = "system.preset";
const USER_PRESET: &str = "user.preset";
/// What runs in an ordinary boot (ADR 0013).
const RUNNING_UNITS: &str = "running-units.txt";
/// How exposed the services are (`security.rs`).
const SECURITY: &str = "security.toml";
/// What the image weighs (`sizes.rs`).
const SIZES: &str = "sizes.toml";

/// `images/<variant>/variant.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    name: String,
    extends: Option<String>,
    packages: Vec<String>,
}

/// `images/<variant>/security.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Security {
    #[serde(default)]
    exposure: BTreeMap<String, f64>,
    /// Services of every user's service manager.
    #[serde(default, rename = "user-exposure")]
    user_exposure: BTreeMap<String, f64>,
}

/// `images/<variant>/sizes.toml`: in KiB, the variant's /usr image and the
/// programs the layer adds.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SizesFile {
    usr: u64,
    #[serde(default)]
    programs: BTreeMap<String, u64>,
}

/// What the variant's image weighs, in KiB: its /usr image, and Beize's
/// programs in it by their path.
pub struct Sizes {
    pub usr: u64,
    pub programs: BTreeMap<String, u64>,
}

/// The measured exposure of the services, by unit.
#[derive(Default)]
pub struct Exposure {
    pub system: BTreeMap<String, f64>,
    pub user: BTreeMap<String, f64>,
}

/// One directory of `images/`: a variant without what it extends.
#[derive(Clone, Debug)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub packages: Vec<String>,
}

/// A variant with everything it extends.
#[derive(Clone, Debug)]
pub struct Variant {
    /// `VARIANT_ID` in os-release, and the directory in `images/`.
    pub id: String,
    /// `VARIANT` in os-release.
    pub name: String,
    /// What it consists of, the base first and this variant last.
    pub layers: Vec<Layer>,
}

impl Variant {
    /// Loads variant `id` from `images/` below the repository `root`.
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        let mut layers = Vec::new();
        let mut name = None;
        let mut next = Some(id.to_owned());
        while let Some(id) = next {
            ensure!(
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{id:?} is not a variant name: lowercase letters, digits and `-`"
            );
            ensure!(
                !layers.iter().any(|l: &Layer| l.id == id),
                "variant {id} extends itself"
            );
            let dir = root.join("images").join(&id);
            let file = dir.join("variant.toml");
            ensure!(
                file.is_file(),
                "there is no variant {id} ({} is missing)",
                file.display()
            );
            let definition: Definition = tree::read_toml(&file)?;
            name.get_or_insert(definition.name.clone());
            next = definition.extends;
            layers.push(Layer {
                id,
                name: definition.name,
                dir,
                packages: definition.packages,
            });
        }
        layers.reverse();
        let variant = Self {
            id: id.to_owned(),
            name: name.context("a variant without a name")?,
            layers,
        };
        variant.check()?;
        Ok(variant)
    }

    /// The variant this one extends, as its own: what runs in the start
    /// without this variant's additions (ADR 0025). `None` for the base.
    pub fn extended(&self) -> Option<Self> {
        let layers = self.layers[..self.layers.len() - 1].to_vec();
        let top = layers.last()?;
        Some(Self {
            id: top.id.clone(),
            name: top.name.clone(),
            layers,
        })
    }

    /// Every package the variant names, the base's first.
    pub fn packages(&self) -> Vec<String> {
        self.layers
            .iter()
            .flat_map(|l| l.packages.iter().cloned())
            .collect()
    }

    /// The presets of all layers, the base's first: systemd takes the first
    /// line that matches a unit, so no later layer switches a unit of an
    /// earlier one.
    pub fn system_preset(&self) -> Result<String> {
        self.joined(SYSTEM_PRESET)
    }

    /// The presets of the user units, as [`Self::system_preset`].
    pub fn user_preset(&self) -> Result<String> {
        self.joined(USER_PRESET)
    }

    /// What runs in an ordinary boot, all layers.
    pub fn running_units(&self) -> Result<String> {
        self.joined(RUNNING_UNITS)
    }

    /// The measured exposure of the services, all layers.
    pub fn exposure(&self) -> Result<Exposure> {
        let mut all = Exposure::default();
        for file in self.files(SECURITY) {
            let layer: Security = tree::read_toml(&file)?;
            all.system.extend(layer.exposure);
            all.user.extend(layer.user_exposure);
        }
        Ok(all)
    }

    /// What the variant weighs: its own /usr image (each layer states its
    /// variant's), the programs of all layers.
    pub fn sizes(&self) -> Result<Sizes> {
        let mut sizes = Sizes {
            usr: 0,
            programs: BTreeMap::new(),
        };
        for layer in &self.layers {
            let file = layer.dir.join(SIZES);
            ensure!(
                file.is_file(),
                "{} is missing: each variant states what it weighs",
                file.display()
            );
            let layer: SizesFile = tree::read_toml(&file)?;
            sizes.usr = layer.usr;
            sizes.programs.extend(layer.programs);
        }
        Ok(sizes)
    }

    /// The files a build of the variant depends on, for fingerprints.
    pub fn definition_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for layer in &self.layers {
            files.push(layer.dir.join("variant.toml"));
            for name in [SYSTEM_PRESET, USER_PRESET, RUNNING_UNITS, SECURITY, SIZES] {
                let file = layer.dir.join(name);
                if file.is_file() {
                    files.push(file);
                }
            }
        }
        files
    }

    /// The layers' files called `name`, the base's first; a layer may leave
    /// a file out when it adds nothing to it.
    fn files(&self, name: &str) -> Vec<PathBuf> {
        self.layers
            .iter()
            .map(|l| l.dir.join(name))
            .filter(|f| f.is_file())
            .collect()
    }

    fn joined(&self, name: &str) -> Result<String> {
        let mut text = String::new();
        for file in self.files(name) {
            text += &std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
        }
        Ok(text)
    }

    /// A later layer only adds: no package, preset unit, running unit or
    /// exposure entry an earlier layer has already.
    fn check(&self) -> Result<()> {
        let base = &self.layers[0];
        for name in [SYSTEM_PRESET, USER_PRESET, RUNNING_UNITS, SECURITY, SIZES] {
            ensure!(
                base.dir.join(name).is_file(),
                "variant {} extends nothing, so it needs its own {name}",
                base.id
            );
        }
        let mut problems = Vec::new();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        let mut add = |what: &str, item: String, layer: &str| {
            let key = format!("{what} {item}");
            match seen.get(&key) {
                Some(first) => problems.push(format!("{layer}: {key} is in {first} already")),
                None => {
                    seen.insert(key, layer.to_owned());
                }
            }
        };
        for layer in &self.layers {
            let read = |name: &str| -> Result<String> {
                let file = layer.dir.join(name);
                Ok(if file.is_file() {
                    std::fs::read_to_string(&file)
                        .with_context(|| format!("reading {}", file.display()))?
                } else {
                    String::new()
                })
            };
            for package in &layer.packages {
                add("package", package.clone(), &layer.id);
            }
            for unit in preset_units(&read(SYSTEM_PRESET)?) {
                add("system preset of", unit.to_owned(), &layer.id);
            }
            for unit in preset_units(&read(USER_PRESET)?) {
                add("user preset of", unit.to_owned(), &layer.id);
            }
            for unit in unit_list(&read(RUNNING_UNITS)?) {
                add("running unit", unit.to_owned(), &layer.id);
            }
            let file = layer.dir.join(SECURITY);
            if file.is_file() {
                let security: Security = tree::read_toml(&file)?;
                for unit in security.exposure.into_keys() {
                    add("exposure of", unit, &layer.id);
                }
                for unit in security.user_exposure.into_keys() {
                    add("user exposure of", unit, &layer.id);
                }
            }
            let file = layer.dir.join(SIZES);
            if file.is_file() {
                let sizes: SizesFile = tree::read_toml(&file)?;
                for program in sizes.programs.into_keys() {
                    add("size of", program, &layer.id);
                }
            }
        }
        if !problems.is_empty() {
            bail!(
                "variant {} changes what it extends (ADR 0025):\n  {}",
                self.id,
                problems.join("\n  ")
            );
        }
        Ok(())
    }
}

/// The unit names of a list like `running-units.txt`, without comments.
pub fn unit_list(list: &str) -> Vec<&str> {
    list.lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(str::split_whitespace)
        .collect()
}

/// The units (or patterns) a preset file names.
fn preset_units(preset: &str) -> impl Iterator<Item = &str> {
    preset
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_whitespace().nth(1))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn desktop_extends_base() {
        let desktop = Variant::load(&repository(), DEFAULT).unwrap();
        assert_eq!(desktop.id, "desktop");
        let ids: Vec<_> = desktop.layers.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["base", "desktop"]);
        let base = Variant::load(&repository(), "base").unwrap();
        assert_eq!(base.layers.len(), 1);
        assert!(base.packages().contains(&"systemd".to_owned()));
        // Without its additions the desktop is the base.
        assert!(base.extended().is_none());
        let without = desktop.extended().unwrap();
        assert_eq!(
            (without.id.as_str(), without.name.as_str()),
            ("base", "Basis")
        );
        assert_eq!(without.packages(), base.packages());
        assert_eq!(
            without.running_units().unwrap(),
            base.running_units().unwrap()
        );
        // The desktop's lists start with the base's.
        assert!(
            desktop
                .running_units()
                .unwrap()
                .starts_with(&base.running_units().unwrap())
        );
        let (desktop, base) = (desktop.exposure().unwrap(), base.exposure().unwrap());
        assert!(desktop.system.len() >= base.system.len());
        assert!(desktop.user.contains_key("beize-compositor.service"));
    }

    #[test]
    fn a_variant_only_adds() {
        let root = std::env::temp_dir().join(format!("bz-variant-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join("images").join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "base/variant.toml",
            "name = \"Basis\"\npackages = [\"bash\"]\n",
        );
        write("base/system.preset", "# a comment\nenable a.service\n");
        write("base/user.preset", "enable u.service\n");
        write("base/running-units.txt", "a.service # why\n");
        write(
            "base/security.toml",
            "[exposure]\n\"a.service\" = 1.0\n[user-exposure]\n\"u.service\" = 1.0\n",
        );
        write(
            "base/sizes.toml",
            "usr = 100\n[programs]\n\"usr/bin/a\" = 10\n",
        );
        write(
            "top/variant.toml",
            "name = \"Top\"\nextends = \"base\"\npackages = [\"mesa\"]\n",
        );
        write("top/running-units.txt", "b.service\n");
        let top = Variant::load(&root, "top").unwrap();
        assert_eq!(top.name, "Top");
        assert_eq!(top.packages(), ["bash", "mesa"]);
        assert_eq!(
            unit_list(&top.running_units().unwrap()),
            ["a.service", "b.service"]
        );

        // Repeating or switching what the base has fails.
        for (file, text, problem) in [
            (
                "top/variant.toml",
                "name = \"Top\"\nextends = \"base\"\npackages = [\"bash\"]\n",
                "package bash",
            ),
            (
                "top/system.preset",
                "disable a.service\n",
                "system preset of a.service",
            ),
            (
                "top/user.preset",
                "disable u.service\n",
                "user preset of u.service",
            ),
            (
                "top/security.toml",
                "[exposure]\n\"a.service\" = 9.0\n",
                "exposure of a.service",
            ),
            (
                "top/security.toml",
                "[user-exposure]\n\"u.service\" = 9.0\n",
                "user exposure of u.service",
            ),
            (
                "top/sizes.toml",
                "usr = 200\n[programs]\n\"usr/bin/a\" = 20\n",
                "size of usr/bin/a",
            ),
        ] {
            write(file, text);
            let error = format!("{:#}", Variant::load(&root, "top").unwrap_err());
            assert!(error.contains(problem), "{error}");
            write(
                "top/variant.toml",
                "name = \"Top\"\nextends = \"base\"\npackages = []\n",
            );
            let _ = fs::remove_file(root.join("images").join("top/system.preset"));
            let _ = fs::remove_file(root.join("images").join("top/user.preset"));
            let _ = fs::remove_file(root.join("images").join("top/security.toml"));
            let _ = fs::remove_file(root.join("images").join("top/sizes.toml"));
        }
        write(
            "loop/variant.toml",
            "name = \"L\"\nextends = \"loop\"\npackages = []\n",
        );
        assert!(Variant::load(&root, "loop").is_err());
        assert!(Variant::load(&root, "Top").is_err());
        assert!(Variant::load(&root, "missing").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
