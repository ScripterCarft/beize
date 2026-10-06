//! Secure Boot: signing EFI binaries and preparing firmware for tests.
//!
//! Development builds are signed with throwaway test keys generated under
//! `build/work/secureboot-test/`, so every build exercises the complete
//! Secure Boot chain in QEMU. Production keys never enter the build tree;
//! they live in the user's 1Password vault (ADR 0006).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::paths::Paths;
use crate::util;

/// Owner GUID recorded with Beize's certificates in the firmware databases.
pub const OWNER_GUID: &str = "6265697a-6500-4000-8000-000000000100";
/// Pristine OVMF variable store (setup mode, no keys enrolled).
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

/// A Secure Boot key hierarchy: PK (platform owner) signs KEK, KEK
/// authorises db updates, db holds the certificate that signs boot binaries.
pub struct Keys {
    dir: PathBuf,
}

impl Keys {
    /// Throwaway keys for development builds and QEMU tests; created on demand.
    pub fn test(paths: &Paths) -> Result<Self> {
        let dir = paths.work.join("secureboot-test");
        fs::create_dir_all(&dir)?;
        for (name, purpose) in [
            ("PK", "platform key"),
            ("KEK", "key exchange key"),
            ("db", "signing key"),
        ] {
            if !dir.join(format!("{name}.crt")).exists() {
                generate(&dir, name, &format!("Beize TEST ONLY {purpose}"))?;
            }
        }
        Ok(Self { dir })
    }

    fn cert(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.crt"))
    }

    /// Signs an EFI binary with the db key and checks the signature.
    pub fn sign(&self, input: &Path, output: &Path) -> Result<()> {
        let cert = self.cert("db");
        util::output(
            Command::new("sbsign")
                .arg("--key")
                .arg(self.dir.join("db.key"))
                .arg("--cert")
                .arg(&cert)
                .arg("--output")
                .arg(output)
                .arg(input),
        )
        .with_context(|| format!("signing {}", input.display()))?;
        util::output(
            Command::new("sbverify")
                .arg("--cert")
                .arg(&cert)
                .arg(output),
        )
        .with_context(|| format!("verifying signature of {}", output.display()))?;
        eprintln!("  signed {}", output.display());
        Ok(())
    }

    /// Writes an OVMF variable store with these keys enrolled and Secure Boot
    /// enabled. Only these keys are trusted (no Microsoft certificates).
    pub fn firmware_vars(&self, output: &Path) -> Result<()> {
        ensure!(
            Path::new(OVMF_VARS).exists(),
            "{OVMF_VARS} is missing; install ovmf"
        );
        util::output(
            Command::new("virt-fw-vars")
                .arg("--input")
                .arg(OVMF_VARS)
                .arg("--output")
                .arg(output)
                .arg("--set-pk")
                .arg(OWNER_GUID)
                .arg(self.cert("PK"))
                .arg("--add-kek")
                .arg(OWNER_GUID)
                .arg(self.cert("KEK"))
                .arg("--add-db")
                .arg(OWNER_GUID)
                .arg(self.cert("db"))
                .arg("--no-microsoft")
                .arg("--secure-boot"),
        )
        .context("enrolling keys into the firmware variables")?;
        Ok(())
    }
}

/// Creates a self-signed RSA-2048 certificate and key. RSA-2048 is what every
/// UEFI implementation must accept; larger keys fail on some mainboards.
fn generate(dir: &Path, name: &str, common_name: &str) -> Result<()> {
    let key = dir.join(format!("{name}.key"));
    util::output(
        Command::new("openssl")
            .args([
                "req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256",
            ])
            .args(["-days", "3650", "-subj", &format!("/CN={common_name}/")])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(dir.join(format!("{name}.crt"))),
    )
    .with_context(|| format!("generating {name}"))?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
