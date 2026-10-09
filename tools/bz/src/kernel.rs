use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::util;

/// Fixed build metadata so the same sources produce the same kernel.
const BUILD_USER: &str = "beize";
const BUILD_HOST: &str = "beize";
const BUILD_TIMESTAMP: &str = "Thu Jan  1 00:00:00 UTC 1970";

/// Configures and builds the kernel. Returns the path of the bzImage in `out/`.
pub fn build(paths: &Paths) -> Result<PathBuf> {
    let recipe = Recipe::load(paths, "linux")?;
    let src = recipe.unpack(paths)?;
    let obj = paths.work.join("linux-obj");
    fs::create_dir_all(&obj)?;
    // Every fragment in kernel/config, in name order: one image for the VM
    // tests and for real hardware.
    let mut fragments: Vec<PathBuf> = fs::read_dir(paths.root.join("kernel/config"))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    fragments.retain(|path| path.extension().is_some_and(|ext| ext == "config"));
    fragments.sort();

    util::step(&format!("Configuring linux {}", recipe.version));
    util::run(make(&src, &obj).args(["defconfig", "kvm_guest.config"]))?;
    util::run(
        Command::new(src.join("scripts/kconfig/merge_config.sh"))
            .current_dir(&src)
            .arg("-m")
            .arg("-O")
            .arg(&obj)
            .arg(obj.join(".config"))
            .args(&fragments),
    )?;
    util::run(make(&src, &obj).arg("olddefconfig"))?;
    for fragment in &fragments {
        verify_config(fragment, &obj.join(".config"))?;
    }

    util::step(&format!("Building linux {}", recipe.version));
    let jobs = util::job_count();
    util::run(make(&src, &obj).arg(format!("-j{jobs}")).arg("bzImage"))?;

    let release = util::output(make(&src, &obj).args(["-s", "kernelrelease"]))?;
    let image = paths.out.join("vmlinuz");
    fs::copy(obj.join("arch/x86/boot/bzImage"), &image)?;
    fs::write(paths.out.join("kernel.release"), &release)?;
    eprintln!("  kernel {release} -> {}", image.display());
    Ok(image)
}

fn make(src: &Path, obj: &Path) -> Command {
    let mut cmd = Command::new("make");
    cmd.current_dir(src)
        .arg(format!("O={}", obj.display()))
        .env("KBUILD_BUILD_USER", BUILD_USER)
        .env("KBUILD_BUILD_HOST", BUILD_HOST)
        .env("KBUILD_BUILD_TIMESTAMP", BUILD_TIMESTAMP);
    cmd
}

/// Fails if Kconfig dropped or changed any option requested by the fragment,
/// e.g. because a dependency is missing.
fn verify_config(fragment: &Path, config: &Path) -> Result<()> {
    let wanted = parse_config(
        &fs::read_to_string(fragment).with_context(|| format!("reading {}", fragment.display()))?,
    );
    let actual = parse_config(&fs::read_to_string(config)?);

    let mismatches: Vec<String> = wanted
        .iter()
        .filter(|(key, value)| actual.get(*key) != Some(*value))
        .map(|(key, value)| {
            let got = actual.get(key).map_or("unset", String::as_str);
            format!("{key}: wanted {value}, got {got}")
        })
        .collect();

    if !mismatches.is_empty() {
        bail!(
            "kernel config does not match {}:\n  {}",
            fragment.display(),
            mismatches.join("\n  ")
        );
    }
    Ok(())
}

/// Parses `CONFIG_X=value` and `# CONFIG_X is not set` lines.
/// A disabled option is represented by the value `n`.
fn parse_config(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("# ") {
                let key = rest.strip_suffix(" is not set")?;
                return key
                    .starts_with("CONFIG_")
                    .then(|| (key.to_owned(), "n".to_owned()));
            }
            let (key, value) = line.split_once('=')?;
            key.starts_with("CONFIG_")
                .then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_and_unset_options() {
        let config = parse_config(
            "# comment\nCONFIG_EFI=y\nCONFIG_LOCALVERSION=\"-beize\"\n# CONFIG_DEBUG is not set\n\n",
        );
        assert_eq!(config.len(), 3);
        assert_eq!(config["CONFIG_EFI"], "y");
        assert_eq!(config["CONFIG_LOCALVERSION"], "\"-beize\"");
        assert_eq!(config["CONFIG_DEBUG"], "n");
    }

    #[test]
    fn ignores_plain_comments() {
        assert!(parse_config("# Boot: the kernel is a UKI\n").is_empty());
    }
}
