//! The image's bill of materials (ADR 0022), as SPDX 2.3 JSON: what Beize
//! contains – every package with version, source, pin, license and CPE, and
//! the Rust crates of Beize's own services – what it depends on, and the
//! buildroot that built it (ADR 0021). Written with every image and every
//! release; dated by SOURCE_DATE_EPOCH, so the same build gives the same file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::packages::{self, BUILD_EPOCH};
use crate::paths::Paths;
use crate::recipe::{Recipe, Stage};
use crate::say;
use crate::source::Origin;
use crate::tree;
use crate::util;

/// The SBOM's file name, next to the disk image and in each release.
pub const FILE: &str = "sbom.spdx.json";
/// What the buildroot records about itself (ci/buildroot/create.sh).
const BUILDINFO: &str = "/usr/share/beize-buildroot/buildinfo";
/// Recipes that go into the image besides the packages: the kernel and
/// the firmware and microcode it loads.
const KERNEL_RECIPES: [&str; 3] = ["linux", "linux-firmware", "intel-microcode"];
/// Licenses without an SPDX identifier that recipes name.
const OWN_LICENSES: [(&str, &str, &str); 3] = [
    (
        "LicenseRef-Public-Domain",
        "Public domain",
        "Dedicated to the public domain by its authors.",
    ),
    (
        "LicenseRef-Firmware",
        "Firmware licenses",
        "Redistributable binary firmware under the licenses in linux-firmware's WHENCE.",
    ),
    (
        "LicenseRef-Intel-Microcode",
        "Intel microcode license",
        "Intel's license for redistributing its processor microcode, in license.txt of the release.",
    ),
];

/// Writes the SBOM of the image of `version` to `dest`.
pub fn write(paths: &Paths, version: &str, dest: &Path) -> Result<()> {
    let definition = tree::image_definition(paths)?;
    let mut recipes = packages::resolve(paths, &definition.packages)?;
    for name in KERNEL_RECIPES {
        recipes.push(Recipe::load(paths, name)?);
    }
    let crates = rust_crates(paths, &recipes)?;
    let buildroot = buildinfo()?;
    let document = document(version, &recipes, &crates, &buildroot)?;
    let mut text = serde_json::to_string_pretty(&document)?;
    text.push('\n');
    fs::write(dest, text).with_context(|| format!("writing {}", dest.display()))?;
    say!(
        "  sbom -> {} ({} packages, {} crates)",
        dest.display(),
        recipes.len(),
        crates.len()
    );
    Ok(())
}

/// A Rust crate in Beize's own services.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Crate {
    name: String,
    version: String,
    checksum: Option<String>,
    /// The recipes whose programs link it.
    used_by: BTreeSet<String>,
}

fn document(
    version: &str,
    recipes: &[Recipe],
    crates: &[Crate],
    buildroot: &[(String, String, String)],
) -> Result<Value> {
    let image = "SPDXRef-Image";
    let mut packages = vec![json!({
        "SPDXID": image,
        "name": "Beize",
        "versionInfo": version,
        "primaryPackagePurpose": "OPERATING-SYSTEM",
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": false,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
    })];
    let mut relationships = vec![relationship("SPDXRef-DOCUMENT", "DESCRIBES", image)];
    let names: BTreeSet<&str> = recipes.iter().map(|r| r.name.as_str()).collect();
    for recipe in recipes {
        let id = package_id(&recipe.name);
        packages.push(recipe_package(recipe, &id)?);
        let builds_only = recipe.stage == Some(Stage::Toolchain)
            || (recipe.stage == Some(Stage::Target) && recipe.files.image.is_empty());
        relationships.push(if builds_only {
            relationship(&id, "BUILD_TOOL_OF", image)
        } else {
            relationship(image, "CONTAINS", &id)
        });
        for dep in &recipe.deps {
            if names.contains(dep.as_str()) {
                relationships.push(relationship(&id, "DEPENDS_ON", &package_id(dep)));
            }
        }
    }
    for krate in crates {
        let id = format!(
            "SPDXRef-Crate-{}",
            spdx_id(&format!("{}-{}", krate.name, krate.version))
        );
        let mut package = json!({
            "SPDXID": id,
            "name": krate.name,
            "versionInfo": krate.version,
            "downloadLocation": format!(
                "https://crates.io/api/v1/crates/{}/{}/download", krate.name, krate.version),
            "filesAnalyzed": false,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "externalRefs": [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": format!("pkg:cargo/{}@{}", krate.name, krate.version),
            }],
        });
        if let Some(checksum) = &krate.checksum {
            package["checksums"] = json!([{ "algorithm": "SHA256", "checksumValue": checksum }]);
        }
        packages.push(package);
        for user in &krate.used_by {
            relationships.push(relationship(&package_id(user), "STATIC_LINK", &id));
        }
    }
    for (name, version, arch) in buildroot {
        let id = format!("SPDXRef-Buildroot-{}", spdx_id(name));
        packages.push(json!({
            "SPDXID": id,
            "name": name,
            "versionInfo": version,
            "supplier": "Organization: Canonical Ltd.",
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "externalRefs": [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": format!("pkg:deb/ubuntu/{name}@{version}?arch={arch}"),
            }],
        }));
        relationships.push(relationship(&id, "BUILD_TOOL_OF", image));
    }
    let licenses: Vec<Value> = OWN_LICENSES
        .iter()
        .map(|(id, name, text)| json!({ "licenseId": id, "name": name, "extractedText": text }))
        .collect();
    let mut document = json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": format!("Beize {version}"),
        "documentNamespace": "",
        "creationInfo": {
            "created": iso8601(BUILD_EPOCH),
            "creators": ["Tool: bz (Beize build tool)"],
        },
        "packages": packages,
        "relationships": relationships,
        "hasExtractedLicensingInfos": licenses,
    });
    // Unique for its content, as SPDX asks, and the same for the same build.
    let digest = util::hex(&Sha256::digest(serde_json::to_vec(&document)?));
    document["documentNamespace"] = json!(format!(
        "https://github.com/ScripterCarft/beize/spdx/{version}-{digest}"
    ));
    Ok(document)
}

fn recipe_package(recipe: &Recipe, id: &str) -> Result<Value> {
    let mut package = json!({
        "SPDXID": id,
        "name": recipe.name,
        "versionInfo": recipe.version,
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": false,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": recipe.sbom.license.as_deref().unwrap_or("NOASSERTION"),
        "copyrightText": "NOASSERTION",
    });
    if let Some(source) = &recipe.source {
        match &source.origin {
            Origin::Archive { url, sha256, .. } => {
                package["downloadLocation"] = json!(url);
                package["checksums"] = json!([{ "algorithm": "SHA256", "checksumValue": sha256 }]);
            }
            Origin::Git {
                url, tag, commit, ..
            } => {
                package["downloadLocation"] = json!(format!("git+{url}@{commit}"));
                package["sourceInfo"] = json!(format!("git tag {tag}, commit {commit}"));
            }
        }
    }
    let refs: Vec<Value> = recipe
        .sbom
        .cpe
        .iter()
        .flatten()
        .map(|cpe| {
            json!({
                "referenceCategory": "SECURITY",
                "referenceType": "cpe23Type",
                "referenceLocator": cpe_of_version(cpe, &recipe.version),
            })
        })
        .collect();
    if !refs.is_empty() {
        package["externalRefs"] = json!(refs);
    }
    // Vulnerabilities assessed as not affecting Beize, with the reason.
    let assessed: Vec<Value> = recipe
        .sbom
        .cve_ignore
        .iter()
        .map(|(cve, why)| {
            json!({
                "annotationType": "REVIEW",
                "annotator": "Tool: bz (Beize build tool)",
                "annotationDate": iso8601(BUILD_EPOCH),
                "comment": format!("{cve}: not affected: {why}"),
            })
        })
        .collect();
    if !assessed.is_empty() {
        package["annotations"] = json!(assessed);
    }
    Ok(package)
}

/// `cpe:2.3:a:gnu:bash` and `5.3` → the full CPE 2.3 name of that version.
pub fn cpe_of_version(prefix: &str, version: &str) -> String {
    format!("{prefix}:{version}:*:*:*:*:*:*:*")
}

fn relationship(from: &str, kind: &str, to: &str) -> Value {
    json!({ "spdxElementId": from, "relationshipType": kind, "relatedSpdxElement": to })
}

fn package_id(name: &str) -> String {
    format!("SPDXRef-Package-{}", spdx_id(name))
}

/// SPDX identifiers allow letters, digits, `.` and `-`.
fn spdx_id(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The crates linked into the programs of recipes built from this
/// repository's workspace, for Linux on x86-64, with their checksums from
/// Cargo.lock.
fn rust_crates(paths: &Paths, recipes: &[Recipe]) -> Result<Vec<Crate>> {
    let own: BTreeSet<&str> = recipes
        .iter()
        .filter(|r| !r.repo.is_empty())
        .map(|r| r.name.as_str())
        .collect();
    let out = Command::new("cargo")
        .args([
            "metadata",
            "--format-version=1",
            "--offline",
            "--locked",
            "--filter-platform=x86_64-unknown-linux-gnu",
        ])
        .current_dir(&paths.root)
        .output()
        .context("starting cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        );
    }
    let metadata: Value = serde_json::from_slice(&out.stdout)?;
    let ids: BTreeMap<&str, (&str, &str, bool)> = metadata["packages"]
        .as_array()
        .context("cargo metadata without packages")?
        .iter()
        .filter_map(|p| {
            Some((
                p["id"].as_str()?,
                (
                    p["name"].as_str()?,
                    p["version"].as_str()?,
                    p["source"].is_string(),
                ),
            ))
        })
        .collect();
    let deps: BTreeMap<&str, Vec<&str>> = metadata["resolve"]["nodes"]
        .as_array()
        .context("cargo metadata without resolve")?
        .iter()
        .filter_map(|n| {
            let normal = n["deps"].as_array()?.iter().filter(|d| {
                d["dep_kinds"].as_array().is_some_and(|kinds| {
                    kinds
                        .iter()
                        .any(|k| k["kind"].is_null() || k["kind"] == "build")
                })
            });
            Some((
                n["id"].as_str()?,
                normal.filter_map(|d| d["pkg"].as_str()).collect(),
            ))
        })
        .collect();
    let checksums = lock_checksums(&paths.root.join("Cargo.lock"))?;
    let mut crates: BTreeMap<(String, String), Crate> = BTreeMap::new();
    for (root, (name, ..)) in &ids {
        if !own.contains(name) {
            continue;
        }
        let mut pending = vec![*root];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some((crate_name, version, from_registry)) = ids.get(id)
                && *from_registry
            {
                let key = ((*crate_name).to_owned(), (*version).to_owned());
                crates
                    .entry(key.clone())
                    .or_insert_with(|| Crate {
                        name: key.0.clone(),
                        version: key.1.clone(),
                        checksum: checksums.get(&key).cloned(),
                        used_by: BTreeSet::new(),
                    })
                    .used_by
                    .insert((*name).to_owned());
            }
            pending.extend(deps.get(id).into_iter().flatten().copied());
        }
    }
    Ok(crates.into_values().collect())
}

/// `(name, version)` → SHA-256 of every crate in a Cargo.lock.
fn lock_checksums(lock: &Path) -> Result<BTreeMap<(String, String), String>> {
    #[derive(serde::Deserialize)]
    struct Lock {
        package: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        version: String,
        checksum: Option<String>,
    }
    let lock: Lock = tree::read_toml(lock)?;
    Ok(lock
        .package
        .into_iter()
        .filter_map(|e| Some(((e.name, e.version), e.checksum?)))
        .collect())
}

/// The buildroot's packages: `(name, version, architecture)`.
fn buildinfo() -> Result<Vec<(String, String, String)>> {
    if cfg!(test) {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(BUILDINFO).with_context(|| format!("reading {BUILDINFO}"))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let (name, version, arch) = (fields.next()?, fields.next()?, fields.next()?);
            (fields.next().is_none() && !version.starts_with('('))
                .then(|| (name.to_owned(), version.to_owned(), arch.to_owned()))
        })
        .collect())
}

/// `seconds` since 1970 in UTC as ISO 8601 (SPDX's `created`).
fn iso8601(seconds: u64) -> String {
    let days = seconds / 86_400;
    let rest = seconds % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_identifiers() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(BUILD_EPOCH), "2026-10-06T00:00:00Z");
        assert_eq!(iso8601(951_782_400 + 3661), "2000-02-29T01:01:01Z");
        assert_eq!(spdx_id("libstdc++_x"), "libstdc---x");
        assert_eq!(
            cpe_of_version("cpe:2.3:a:gnu:bash", "5.3"),
            "cpe:2.3:a:gnu:bash:5.3:*:*:*:*:*:*:*"
        );
        assert_eq!(
            cpe_of_version("cpe:2.3:a:gnu:bash", "5.3")
                .split(':')
                .count(),
            13
        );
    }

    #[test]
    fn the_image_contains_what_ships_and_names_its_licenses() {
        let paths = Paths::new().unwrap();
        let recipes: Vec<Recipe> = ["glibc", "gcc", "linux-headers"]
            .iter()
            .map(|n| Recipe::load(&paths, n).unwrap())
            .collect();
        let doc = document("0.0", &recipes, &[], &[]).unwrap();
        let rel = |from: &str, kind: &str, to: &str| {
            doc["relationships"].as_array().unwrap().iter().any(|r| {
                r["spdxElementId"] == from
                    && r["relationshipType"] == kind
                    && r["relatedSpdxElement"] == to
            })
        };
        assert!(rel("SPDXRef-Image", "CONTAINS", "SPDXRef-Package-glibc"));
        assert!(rel("SPDXRef-Package-gcc", "BUILD_TOOL_OF", "SPDXRef-Image"));
        assert!(rel(
            "SPDXRef-Package-linux-headers",
            "BUILD_TOOL_OF",
            "SPDXRef-Image"
        ));
        let glibc = doc["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "glibc")
            .unwrap();
        assert!(
            glibc["licenseDeclared"]
                .as_str()
                .unwrap()
                .starts_with("LGPL-2.1-or-later")
        );
        assert_eq!(glibc["externalRefs"][0]["referenceType"], "cpe23Type");
        // Every LicenseRef a recipe names is explained in the document.
        let explained: Vec<&str> = OWN_LICENSES.iter().map(|l| l.0).collect();
        for name in recipe_names(&paths) {
            let recipe = Recipe::load(&paths, &name).unwrap();
            for word in recipe
                .sbom
                .license
                .iter()
                .flat_map(|l| l.split([' ', '(', ')']))
            {
                if word.starts_with("LicenseRef-") {
                    assert!(explained.contains(&word), "{name}: {word} is not explained");
                }
            }
        }
    }

    fn recipe_names(paths: &Paths) -> Vec<String> {
        crate::recipe::all_names(paths).unwrap()
    }
}
