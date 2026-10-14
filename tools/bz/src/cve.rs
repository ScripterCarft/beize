//! `bz cve`: the known vulnerabilities of what the image contains (ADR 0022),
//! as Yocto's cve-check finds them – from the NVD, by each recipe's CPE
//! names and version. Assessed ones (`[sbom.cve-ignore]`, with the reason)
//! are left out. Fails when one of CVSS 7.0 or more is left: the weekly CI
//! run then turns red.
//!
//! Rust projects' crates are not in the NVD: RustSec's advisories for the
//! Cargo.lock of each such source (`[sbom] cargo-lock`) as Beize builds it
//! (`[cargo.update]`), with cargo-audit as their upstreams run it. Every advisory of a vulnerability fails until it
//! is assessed; unmaintained or unsound crates are shown. Beize's own crates
//! are cargo-deny's (deny.toml).
//!
//! Without an API key the NVD allows 5 requests in 30 seconds; with one in
//! `NVD_API_KEY`, 50.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cargo;
use crate::outdated::compare_versions;
use crate::paths::Paths;
use crate::sbom;
use crate::util;

const API: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0";
/// From this CVSS base score on, an open vulnerability fails the check
/// (CVSS's "high" and "critical").
const FAIL_FROM: f64 = 7.0;

/// One vulnerability of one package.
#[derive(Debug, Clone, PartialEq)]
struct Finding {
    id: String,
    score: Option<f64>,
    summary: String,
}

pub fn show(paths: &Paths) -> Result<()> {
    let key = std::env::var("NVD_API_KEY").ok().filter(|k| !k.is_empty());
    let pause = Duration::from_millis(if key.is_some() { 700 } else { 6500 });
    let mut open = 0;
    let mut serious = Vec::new();
    let mut stale = Vec::new();
    for recipe in sbom::image_recipes(paths)? {
        if sbom::builds_only(&recipe) {
            continue;
        }
        let mut findings: BTreeMap<String, Finding> = BTreeMap::new();
        for cpe in recipe.sbom.cpe.iter().flatten() {
            let name = sbom::cpe_of_version(cpe, &recipe.version);
            let answer = query(&name, key.as_deref())
                .with_context(|| format!("asking the NVD about {name}"))?;
            for finding in parse(&answer, cpe, &recipe.version)? {
                findings.entry(finding.id.clone()).or_insert(finding);
            }
            thread::sleep(pause);
        }
        for cve in recipe.sbom.cve_ignore.keys() {
            if cve.starts_with("CVE-") && !findings.contains_key(cve) {
                stale.push(format!("{}: {cve}", recipe.name));
            }
        }
        for finding in findings.values() {
            if recipe.sbom.cve_ignore.contains_key(&finding.id) {
                continue;
            }
            open += 1;
            let score = finding
                .score
                .map_or_else(|| "  ? ".to_owned(), |s| format!("{s:4.1}"));
            let line = format!(
                "  {} {}: {} ({score}) {}",
                recipe.name, recipe.version, finding.id, finding.summary
            );
            println!("{line}");
            if finding.score.is_some_and(|s| s >= FAIL_FROM) {
                serious.push(line);
            }
        }
    }
    let nvd_serious = serious.len();
    let mut crates_open = 0;
    for recipe in sbom::image_recipes(paths)? {
        if let Some(lock) = &recipe.sbom.cargo_lock {
            let src = cargo::source(paths, &recipe, &recipe.unpack(paths)?)?;
            let report = rustsec(paths, &src.join(lock))
                .with_context(|| format!("checking the crates of {}", recipe.name))?;
            for (id, line) in report.vulnerabilities {
                if recipe.sbom.cve_ignore.contains_key(&id) {
                    continue;
                }
                let line = format!("  {} {}: {line}", recipe.name, recipe.version);
                println!("{line}");
                crates_open += 1;
                serious.push(line);
            }
            for line in report.warnings {
                println!("  {} {}: {line}", recipe.name, recipe.version);
            }
            for id in recipe.sbom.cve_ignore.keys() {
                if id.starts_with("RUSTSEC-") && !report.reported.contains(id) {
                    stale.push(format!("{}: {id}", recipe.name));
                }
            }
        }
    }
    for entry in &stale {
        println!("  assessed, but no longer reported (remove from [sbom.cve-ignore]): {entry}");
    }
    println!(
        "NVD: {open} open vulnerabilities, {nvd_serious} of CVSS {FAIL_FROM} or more; \
         RustSec: {crates_open} in crates"
    );
    if !serious.is_empty() {
        bail!(
            "{} open vulnerabilities to act on: update the package, or assess them in its \
             recipe's [sbom.cve-ignore] with the reason",
            serious.len()
        );
    }
    Ok(())
}

/// What RustSec says about the crates of one Cargo.lock.
#[derive(Debug, Default, PartialEq)]
struct RustsecReport {
    /// Vulnerabilities: advisory ID and a line to show.
    vulnerabilities: Vec<(String, String)>,
    /// Unmaintained, unsound or yanked crates, to show.
    warnings: Vec<String>,
    /// Every advisory ID it named.
    reported: std::collections::BTreeSet<String>,
}

/// Runs cargo-audit on `lock`, with RustSec's advisory database fetched
/// into `build/work/rustsec`.
fn rustsec(paths: &Paths, lock: &Path) -> Result<RustsecReport> {
    let out = Command::new("cargo")
        .args(["audit", "--json", "--file"])
        .arg(lock)
        .arg("--db")
        .arg(paths.work.join("rustsec/advisory-db"))
        .output()
        .context("starting cargo audit")?;
    // It exits non-zero when it finds a vulnerability; the JSON says what.
    serde_json::from_slice(&out.stdout)
        .map_err(anyhow::Error::from)
        .and_then(|json: Value| parse_rustsec(&json))
        .with_context(|| {
            format!(
                "cargo audit gave no report: {}",
                String::from_utf8_lossy(&out.stderr).trim_end()
            )
        })
}

/// Reads cargo-audit's `--json` report.
fn parse_rustsec(json: &Value) -> Result<RustsecReport> {
    let mut report = RustsecReport::default();
    let line = |entry: &Value| -> Option<(String, String)> {
        let advisory = &entry["advisory"];
        let id = advisory["id"].as_str()?.to_owned();
        let package = &entry["package"];
        Some((
            id.clone(),
            format!(
                "{} {} {id} {}",
                package["name"].as_str()?,
                package["version"].as_str()?,
                advisory["title"].as_str().unwrap_or_default()
            ),
        ))
    };
    for entry in json["vulnerabilities"]["list"]
        .as_array()
        .context("no vulnerabilities in cargo audit's report")?
    {
        let (id, text) = line(entry).context("an unreadable vulnerability")?;
        report.reported.insert(id.clone());
        report.vulnerabilities.push((id, text));
    }
    for (kind, entries) in json["warnings"].as_object().into_iter().flatten() {
        for entry in entries.as_array().into_iter().flatten() {
            match line(entry) {
                Some((id, text)) => {
                    report.reported.insert(id);
                    report.warnings.push(format!("{text} ({kind})"));
                }
                // Yanked crates come without an advisory.
                None => report.warnings.push(format!(
                    "{} {} ({kind})",
                    entry["package"]["name"].as_str().unwrap_or("?"),
                    entry["package"]["version"].as_str().unwrap_or("?")
                )),
            }
        }
    }
    Ok(report)
}

/// The NVD's answer for the CPE name `cpe` (with version), retrying when it
/// limits the rate.
fn query(cpe: &str, key: Option<&str>) -> Result<Value> {
    let url = format!("{API}?virtualMatchString={cpe}&resultsPerPage=2000");
    let mut tries = 0;
    let text = loop {
        let mut curl = Command::new("curl");
        curl.args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--max-time", "120"]);
        if let Some(key) = key {
            curl.arg("--header").arg(format!("apiKey: {key}"));
        }
        match util::output(curl.arg(&url)) {
            Ok(text) => break text,
            // 403/429: over the rate limit; the NVD asks to wait 30 s.
            Err(_) if tries < 3 => {
                tries += 1;
                thread::sleep(Duration::from_secs(30));
            }
            Err(error) => return Err(error),
        }
    };
    serde_json::from_str(&text).context("parsing the NVD's answer")
}

/// The vulnerabilities in `answer` that make `prefix` at `version` itself
/// vulnerable. The NVD's search also returns CVEs that only name it as the
/// platform of the vulnerable software (an Apache module on OpenSSL, a KDE
/// program on Linux), and ones for another architecture; Yocto's cve-check
/// counts only the vulnerable entries, too.
fn parse(answer: &Value, prefix: &str, version: &str) -> Result<Vec<Finding>> {
    let total = answer["totalResults"].as_u64().context("no totalResults")?;
    let items = answer["vulnerabilities"]
        .as_array()
        .context("no vulnerabilities")?;
    if total as usize > items.len() {
        bail!("the NVD has {total} results, more than one page");
    }
    Ok(items
        .iter()
        .filter_map(|item| {
            let cve = &item["cve"];
            if cve["vulnStatus"] == "Rejected" || !makes_vulnerable(cve, prefix, version) {
                return None;
            }
            let summary = cve["descriptions"]
                .as_array()?
                .iter()
                .find(|d| d["lang"] == "en")
                .and_then(|d| d["value"].as_str())
                .unwrap_or("");
            Some(Finding {
                id: cve["id"].as_str()?.to_owned(),
                score: score(&cve["metrics"]),
                summary: shorten(summary, 100),
            })
        })
        .collect())
}

/// Whether one of the CVE's configurations names `prefix` (part, vendor,
/// product) at `version`, on x86-64, as vulnerable.
fn makes_vulnerable(cve: &Value, prefix: &str, version: &str) -> bool {
    let matches = cve["configurations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|configuration| configuration["nodes"].as_array())
        .flatten()
        .filter_map(|node| node["cpeMatch"].as_array())
        .flatten();
    let wanted: Vec<&str> = prefix.split(':').collect();
    matches.into_iter().any(|m| {
        let Some(criteria) = m["criteria"].as_str() else {
            return false;
        };
        let fields: Vec<&str> = criteria.split(':').collect();
        let any = |field: &str| field == "*" || field == "-";
        m["vulnerable"] == true
            && fields.len() == 13
            && fields[..5] == wanted[..]
            && (any(fields[5]) || compare_versions(version, fields[5]) == Ordering::Equal)
            && (any(fields[11]) || ["x86_64", "x64", "amd64"].contains(&fields[11]))
            && within(m, version)
    })
}

/// Whether `version` lies in the range a CPE match gives.
fn within(m: &Value, version: &str) -> bool {
    let bounds: [(&str, &[Ordering]); 4] = [
        (
            "versionStartIncluding",
            &[Ordering::Greater, Ordering::Equal],
        ),
        ("versionStartExcluding", &[Ordering::Greater]),
        ("versionEndIncluding", &[Ordering::Less, Ordering::Equal]),
        ("versionEndExcluding", &[Ordering::Less]),
    ];
    bounds.iter().all(|(key, accept)| {
        m[*key]
            .as_str()
            .is_none_or(|bound| accept.contains(&compare_versions(version, bound)))
    })
}

/// The newest CVSS version's base score the NVD gives.
fn score(metrics: &Value) -> Option<f64> {
    [
        "cvssMetricV40",
        "cvssMetricV31",
        "cvssMetricV30",
        "cvssMetricV2",
    ]
    .iter()
    .find_map(|version| metrics[version][0]["cvssData"]["baseScore"].as_f64())
}

fn shorten(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_cargo_audits_report() {
        // Shapes as cargo-audit 0.22 writes them (ntpd-rs 1.9.0, 2026-10-14).
        let report = parse_rustsec(&json!({
            "vulnerabilities": { "found": true, "count": 1, "list": [{
                "advisory": { "id": "RUSTSEC-2026-0349", "title": "ServerHello downgrade" },
                "package": { "name": "rustls", "version": "0.23.40" }
            }]},
            "warnings": {
                "unsound": [{
                    "kind": "unsound",
                    "advisory": { "id": "RUSTSEC-2026-0190", "title": "Unsoundness" },
                    "package": { "name": "anyhow", "version": "1.0.102" }
                }],
                "yanked": [{
                    "kind": "yanked",
                    "advisory": null,
                    "package": { "name": "chacha20", "version": "0.10.0" }
                }]
            }
        }))
        .unwrap();
        assert_eq!(
            report.vulnerabilities,
            [(
                "RUSTSEC-2026-0349".to_owned(),
                "rustls 0.23.40 RUSTSEC-2026-0349 ServerHello downgrade".to_owned()
            )]
        );
        assert_eq!(
            report.warnings,
            [
                "anyhow 1.0.102 RUSTSEC-2026-0190 Unsoundness (unsound)",
                "chacha20 0.10.0 (yanked)"
            ]
        );
        assert!(report.reported.contains("RUSTSEC-2026-0190"));
        assert!(parse_rustsec(&json!({ "warnings": {} })).is_err());
    }

    const SSL: &str = "cpe:2.3:a:openssl:openssl";

    fn configured(vulnerable: bool, criteria: &str, range: Value) -> Value {
        let mut m = json!({ "vulnerable": vulnerable, "criteria": criteria });
        for (key, value) in range.as_object().unwrap() {
            m[key] = value.clone();
        }
        json!([{ "nodes": [{ "operator": "OR", "cpeMatch": [m] }] }])
    }

    fn cve(id: &str, status: &str, configurations: Value) -> Value {
        json!({ "cve": { "id": id, "vulnStatus": status, "configurations": configurations,
            "descriptions": [{ "lang": "es", "value": "x" },
                             { "lang": "en", "value": "A  heap\noverflow." }],
            "metrics": { "cvssMetricV31": [{ "cvssData": { "baseScore": 7.5 } }],
                         "cvssMetricV2": [{ "cvssData": { "baseScore": 5.0 } }] } } })
    }

    #[test]
    fn counts_only_what_makes_this_version_vulnerable() {
        let any = format!("{SSL}:*:*:*:*:*:*:*:*");
        let items = vec![
            // Vulnerable up to 4.0.4: counts.
            cve(
                "CVE-1",
                "Analyzed",
                configured(true, &any, json!({ "versionEndExcluding": "4.0.4" })),
            ),
            // Fixed in 4.0.3 (as glibc 2.44's NEWS shows for some): does not.
            cve(
                "CVE-2",
                "Analyzed",
                configured(true, &any, json!({ "versionEndExcluding": "4.0.3" })),
            ),
            // OpenSSL only as the platform of an Apache module: does not.
            cve(
                "CVE-3",
                "Modified",
                configured(false, &any, json!({ "versionStartIncluding": "1.1.1" })),
            ),
            // Another architecture (gcc's AArch64 stack protector): does not.
            cve(
                "CVE-4",
                "Analyzed",
                configured(true, &format!("{SSL}:*:*:*:*:*:*:arm64:*"), json!({})),
            ),
            // Exactly this version, awaiting a score: counts.
            cve(
                "CVE-5",
                "Awaiting Analysis",
                configured(true, &format!("{SSL}:4.0.3:*:*:*:*:*:*:*"), json!({})),
            ),
            // Withdrawn: does not.
            cve("CVE-6", "Rejected", configured(true, &any, json!({}))),
        ];
        let answer = json!({ "totalResults": items.len(), "vulnerabilities": items });
        let found = parse(&answer, SSL, "4.0.3").unwrap();
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["CVE-1", "CVE-5"]);
        assert_eq!(found[0].score, Some(7.5));
        assert_eq!(found[0].summary, "A heap overflow.");
        let more = json!({ "totalResults": 5, "vulnerabilities": [] });
        assert!(parse(&more, SSL, "4.0.3").is_err());
        assert_eq!(shorten("abcdef", 3), "abc…");
    }
}
