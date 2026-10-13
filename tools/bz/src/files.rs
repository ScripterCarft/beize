//! What of each package goes into Beize, as Fedora's `%files` decides it
//! (ADR 0005). A package's files are of three kinds:
//!
//! - for building only by their kind: headers, static libraries, pkg-config
//!   and CMake files, manuals, linker names ([`tree::is_build_only`]) – what
//!   Fedora's -devel packages hold;
//! - for the image: matched by a pattern of the recipe's `[files] image`;
//! - for building only, named by the recipe with its reason (`[files] build`);
//!   such an entry also takes files out of an image pattern, as Fedora's
//!   `%exclude`.
//!
//! Every other file fails the build, as Fedora's "Installed (but unpackaged)
//! file(s) found": nothing a package starts to install reaches the image
//! unseen. So does a pattern that matches nothing (an image pattern counts
//! only files it ships).
//!
//! Patterns are paths relative to the root; `*` matches within a path
//! component, `**` any number of components.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::install;
use crate::paths::Paths;
use crate::recipe::Recipe;
use crate::tree;

/// The files of `recipes` that go into the image, after checking every
/// package's claims.
pub fn image_files(paths: &Paths, recipes: &[Recipe]) -> Result<BTreeSet<PathBuf>> {
    let mut image = BTreeSet::new();
    let mut problems = String::new();
    for recipe in tree::target_packages(recipes) {
        let installed = install::manifest(paths, &recipe.name)?;
        let mut claimable = Vec::new();
        for file in installed {
            if !tree::is_build_only(&paths.sysroot, &file)? {
                claimable.push(file);
            }
        }
        match sort(recipe, &claimable) {
            Ok(files) => image.extend(files),
            Err(found) => problems.push_str(&found),
        }
    }
    if !problems.is_empty() {
        bail!(
            "the [files] of these recipes do not match what they install (ADR 0005):\n{}",
            problems.trim_end()
        );
    }
    Ok(image)
}

/// The image files among `files` of `recipe`, or a description of every
/// file it does not claim exactly once and every pattern without a match.
fn sort(recipe: &Recipe, files: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let image = &recipe.files.image;
    let build: Vec<&str> = recipe.files.build.iter().map(|b| b.path.as_str()).collect();
    let mut used = BTreeSet::new();
    let mut found = String::new();
    let mut shipped = Vec::new();
    for entry in &recipe.files.build {
        if entry.why.trim().is_empty() {
            let _ = writeln!(
                found,
                "  {}: {} needs a reason (why)",
                recipe.name, entry.path
            );
        }
    }
    for file in files {
        let Some(path) = file.to_str() else {
            let _ = writeln!(
                found,
                "  {}: /{} is no UTF-8 path",
                recipe.name,
                file.display()
            );
            continue;
        };
        // A build entry takes its files out of the image patterns, as
        // Fedora's %exclude does.
        let by_build: Vec<&str> = matching(build.iter().copied(), path);
        if !by_build.is_empty() {
            used.extend(by_build);
            continue;
        }
        let by_image: Vec<&str> = matching(image.iter().map(String::as_str), path);
        if by_image.is_empty() {
            let _ = writeln!(
                found,
                "  {}: /{path} is in neither image nor build",
                recipe.name
            );
        } else {
            used.extend(by_image);
            shipped.push(file.clone());
        }
    }
    for pattern in image
        .iter()
        .map(String::as_str)
        .chain(build.iter().copied())
    {
        if !used.contains(pattern) {
            let _ = writeln!(
                found,
                "  {}: {pattern} matches no file it installs",
                recipe.name
            );
        }
    }
    if found.is_empty() {
        Ok(shipped)
    } else {
        Err(found)
    }
}

fn matching<'a>(patterns: impl Iterator<Item = &'a str>, path: &str) -> Vec<&'a str> {
    patterns.filter(|pattern| matches(pattern, path)).collect()
}

/// Whether `path` matches `pattern` (see the module documentation).
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    components_match(&pattern, &path)
}

fn components_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| components_match(rest, &path[skip..])),
        Some((first, rest)) => match path.split_first() {
            Some((name, path_rest)) => {
                name_matches(first, name) && components_match(rest, path_rest)
            }
            None => false,
        },
    }
}

/// `*` in `pattern` matches any run of characters within one name.
fn name_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return name.is_empty();
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_match_within_a_name_double_stars_across() {
        assert!(matches("usr/bin/bash", "usr/bin/bash"));
        assert!(!matches("usr/bin/bash", "usr/bin/bashbug"));
        assert!(matches(
            "usr/lib/libglib-2.0.so.*",
            "usr/lib/libglib-2.0.so.0.9000.1"
        ));
        assert!(!matches(
            "usr/lib/libglib-2.0.so.*",
            "usr/lib/libglib-2.0.so"
        ));
        assert!(matches(
            "usr/bin/glib-compile-*",
            "usr/bin/glib-compile-schemas"
        ));
        assert!(!matches("usr/bin/*", "usr/bin/x/y"));
        assert!(matches("usr/lib/gio/**", "usr/lib/gio/modules/a.so"));
        assert!(matches("usr/lib/gio/**", "usr/lib/gio/a"));
        assert!(!matches("usr/lib/gio/**", "usr/lib/giomm/a"));
        assert!(matches("usr/**/*.conf", "usr/lib/a/b.conf"));
        assert!(matches("usr/**/*.conf", "usr/b.conf"));
        assert!(matches("usr/lib/*-*.so", "usr/lib/a-b-c.so"));
        assert!(!matches("usr/lib/*-*.so", "usr/lib/abc.so"));
    }

    fn with_files(files: &str) -> Recipe {
        Recipe::parse(&format!(
            "name = \"p\"\nversion = \"1\"\n\n[files]\n{files}"
        ))
        .unwrap()
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn ships_what_the_image_patterns_claim() {
        // A build entry takes its file out of an image pattern (%exclude).
        let recipe = with_files(
            "image = [\"usr/bin/*\"]\nbuild = [{ path = \"usr/bin/tool\", why = \"x\" }]\n",
        );
        let files = paths(&["usr/bin/a", "usr/bin/tool"]);
        assert_eq!(sort(&recipe, &files).unwrap(), paths(&["usr/bin/a"]));
        // An image pattern that only matches excluded files is stale.
        let recipe = with_files(
            "image = [\"usr/bin/a\", \"usr/bin/t*\"]\nbuild = [{ path = \"usr/bin/tool\", why = \"x\" }]\n",
        );
        let error = sort(&recipe, &files).unwrap_err();
        assert!(error.contains("usr/bin/t* matches no file"), "{error}");
        let recipe = with_files("build = [{ path = \"usr/bin/*\", why = \" \" }]\n");
        assert!(
            sort(&recipe, &files)
                .unwrap_err()
                .contains("needs a reason")
        );

        let recipe = with_files(
            "image = [\"usr/bin/a\"]\nbuild = [{ path = \"usr/bin/tool\", why = \"x\" }]\n",
        );
        assert_eq!(sort(&recipe, &files).unwrap(), paths(&["usr/bin/a"]));
    }

    #[test]
    fn unclaimed_files_and_unused_patterns_fail() {
        let recipe = with_files("image = [\"usr/bin/a\", \"usr/lib/gone/**\"]\n");
        let error = sort(&recipe, &paths(&["usr/bin/a", "usr/bin/new"])).unwrap_err();
        assert!(
            error.contains("/usr/bin/new is in neither image nor build"),
            "{error}"
        );
        assert!(error.contains("usr/lib/gone/** matches no file"), "{error}");
    }

    #[test]
    fn a_package_without_files_ships_nothing_and_claims_nothing() {
        let recipe = Recipe::parse("name = \"p\"\nversion = \"1\"\n").unwrap();
        assert_eq!(sort(&recipe, &[]).unwrap(), Vec::<PathBuf>::new());
        assert!(sort(&recipe, &paths(&["usr/bin/a"])).is_err());
    }
}
