//! Secure Boot: Beize's keys, where they live, and the files that enroll them
//! into the firmware (ADR 0010).
//!
//! Beize owns the platform: its own PK and KEK, and a db key that signs
//! systemd-boot and the UKI. Which other systems the firmware trusts is a
//! [`Profile`]; with Windows, Microsoft's KEK and db certificates are enrolled
//! alongside Beize's. systemd-boot enrolls everything by itself once the
//! firmware is in setup mode, from signed files on the ESP – in every boot
//! test and on real hardware alike.
//!
//! - Test keys are generated on demand under `build/work/secureboot-test/`.
//! - The owner's production keys: private halves only in 1Password, public
//!   halves and the signed enrollment files in `keys/secureboot/`. On the
//!   build machine private keys exist only in memory (`/dev/shm`). PK and KEK
//!   are needed only by `bz secureboot new-keys`; `bz sign` uses db and PCR.

mod store;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

pub use self::store::Store;
use self::store::{MemoryDir, write_private};
use crate::packages::BUILD_EPOCH;
use crate::paths::Paths;
use crate::say;
use crate::stamp::{Fingerprint, Stamp};
use crate::tree;
use crate::{usr, util};

/// Owner GUID recorded with Beize's certificates in the firmware databases.
pub const OWNER_GUID: &str = "6265697a-6500-4000-8000-000000000100";
/// Owner GUID Microsoft uses for its certificates.
const MICROSOFT_GUID: &str = "77fa9abd-0359-4d32-bd60-28f4e78f784b";
/// Pristine OVMF variable store: no keys, setup mode.
pub const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

/// The signed variable updates systemd-boot enrolls, in `/loader/keys/auto/`.
pub const ENROLL_FILES: [&str; 4] = ["PK.auth", "KEK.auth", "db.auth", "dbx.auth"];
/// What the firmware may hold (signature lists, unsigned), next to the
/// enrollment files. beize-tpm checks the measured state against it before
/// it re-binds the root partition (ADR 0010); built into the signed UKI.
pub const PROFILE_FILES: [&str; 4] = ["PK.esl", "KEK.esl", "db.esl", "beize-db.esl"];
/// Attributes of the Secure Boot variables, as systemd-boot sets them; the
/// signature covers them.
const VARIABLE_ATTRS: &str =
    "NON_VOLATILE,BOOTSERVICE_ACCESS,RUNTIME_ACCESS,TIME_BASED_AUTHENTICATED_WRITE_ACCESS";

/// A committed object: file name and pinned SHA-256.
type Pinned = (&'static str, &'static str);

/// Microsoft's objects, from github.com/microsoft/secureboot_objects at
/// commit 106458910883 (see `keys/microsoft/README.md`): file and SHA-256.
const MICROSOFT_KEK: [Pinned; 2] = [
    (
        "kek-ca-2011.der",
        "a1117f516a32cefcba3f2d1ace10a87972fd6bbe8fe0d0b996e09e65d802a503",
    ),
    (
        "kek-2k-ca-2023.der",
        "3cd3f0309edae228767a976dd40d9f4affc4fbd5218f2e8cc3c9dd97e8ac6f9d",
    ),
];
const MICROSOFT_DB: [Pinned; 5] = [
    (
        "windows-production-pca-2011.der",
        "e8e95f0733a55e8bad7be0a1413ee23c51fcea64b3c8fa6a786935fddcc71961",
    ),
    (
        "windows-uefi-ca-2023.der",
        "076f1fea90ac29155ebf77c17682f75f1fdd1be196da302dc8461e350a9ae330",
    ),
    (
        "uefi-ca-2011.der",
        "48e99b991f57fc52f76149599bff0a58c47154229b9f8d603ac40d3500248507",
    ),
    (
        "uefi-ca-2023.der",
        "f6124e34125bee3fe6d79a574eaa7b91c0e7bd9d929c1a321178efd611dad901",
    ),
    (
        "option-rom-uefi-ca-2023.der",
        "e5be3e64c6e66a281457ecdece0d6d0787577aad2a3a0144262c10c14ba8d8f1",
    ),
];
/// Microsoft's revocation list (dbx), already signed with Microsoft's KEK.
const MICROSOFT_DBX: Pinned = (
    "dbx-update-amd64.bin",
    "2089e3125e611376cb44326b5765674255443b2484f88de97251939d18055f68",
);

/// Which systems the firmware trusts besides Beize.
#[derive(Clone, Copy, Debug)]
pub enum Profile {
    /// Beize and Windows: Microsoft's KEK (so Windows keeps installing
    /// Secure Boot updates) and db (Windows boot manager, option ROMs of
    /// graphics cards and other devices) are enrolled next to Beize's.
    WithWindows,
}

const PK_KEY: &str = "PK.key";
const PK_CERT: &str = "PK.crt";
const KEK_KEY: &str = "KEK.key";
const KEK_CERT: &str = "KEK.crt";
const DB_KEY: &str = "db.key";
const DB_CERT: &str = "db.crt";
const PCR_KEY: &str = "pcr.key";
const PCR_PUBLIC: &str = "pcr.pub.pem";
/// Signs the list of an update (ADR 0020); its public half goes into `/usr`.
const UPDATE_KEY: &str = "update.key";
pub const UPDATE_PUBLIC: &str = "update.pub.pem";
/// Signs the kernel's modules (ADR 0015); the kernel of the same key set
/// trusts its certificate and no other.
const MODULE_KEY: &str = "module.key";
const MODULE_CERT: &str = "module.crt";

/// The owner's private keys in their key store: title and file name.
const PK_SECRET: (&str, &str) = ("Beize Secure Boot platform key (PK)", PK_KEY);
const KEK_SECRET: (&str, &str) = ("Beize Secure Boot key exchange key (KEK)", KEK_KEY);
const DB_SECRET: (&str, &str) = ("Beize Secure Boot signing key (db)", DB_KEY);
const PCR_SECRET: (&str, &str) = ("Beize PCR policy signing key", PCR_KEY);
const UPDATE_SECRET: (&str, &str) = ("Beize update signing key", UPDATE_KEY);
const MODULE_SECRET: (&str, &str) = ("Beize kernel module signing key", MODULE_KEY);

/// Where the public halves of the production keys and the enrollment files
/// are committed.
pub fn production_public_dir(paths: &Paths) -> PathBuf {
    paths.root.join("keys/secureboot")
}

/// Microsoft's certificates and revocation list, as committed.
pub fn microsoft_dir(paths: &Paths) -> PathBuf {
    paths.root.join("keys/microsoft")
}

/// The throwaway test keys (see [`Keys::test`]).
pub fn test_keys_dir(paths: &Paths) -> PathBuf {
    paths.work.join("secureboot-test")
}

/// The test keys' directory, readable by its owner only: test keys are
/// throwaway, but private keys all the same.
fn create_test_keys_dir(paths: &Paths) -> Result<PathBuf> {
    let dir = test_keys_dir(paths);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// The certificate of the test keys' module key, created on demand: the
/// kernel step builds the kernel that trusts it before anything else needs
/// the test keys.
pub fn test_module_cert(paths: &Paths) -> Result<PathBuf> {
    let dir = create_test_keys_dir(paths)?;
    if !dir.join(MODULE_CERT).exists() {
        generate_module_key(&dir, "Beize TEST ONLY kernel module signing key")?;
    }
    Ok(dir.join(MODULE_CERT))
}

/// What signs a build: the db key for boot binaries, the PCR key for the
/// expected PCR 11 values of a UKI, and the enrollment files that make the
/// firmware trust them; the module key for the kernel's modules; the update
/// key's public half for `/usr`, and its private half for releases.
pub struct Keys {
    dir: PathBuf,
    enroll: PathBuf,
    /// Production keys live in memory and are wiped when this is dropped.
    _memory: Option<MemoryDir>,
}

impl Keys {
    /// Throwaway keys for development builds and QEMU tests; created on demand.
    pub fn test(paths: &Paths, profile: Profile) -> Result<Self> {
        let dir = create_test_keys_dir(paths)?;
        for (name, purpose) in [
            ("PK", "platform key"),
            ("KEK", "key exchange key"),
            ("db", "signing key"),
        ] {
            if !dir.join(format!("{name}.crt")).exists() {
                generate_cert_key(&dir, name, &format!("Beize TEST ONLY {purpose}"))?;
            }
        }
        if !dir.join(PCR_PUBLIC).exists() {
            generate_pcr_key(&dir)?;
        }
        if !dir.join(UPDATE_PUBLIC).exists() {
            generate_update_key(&dir)?;
        }
        test_module_cert(paths)?;
        // Signed once for these keys and lists, as the owner's are: sbvarsign
        // dates each signature by the clock, and every image would differ.
        let enroll = dir.join("enroll");
        let mut inputs = Fingerprint::new()?
            .text(&format!("{profile:?}"))
            .tree(&microsoft_dir(paths))?;
        for cert in [PK_CERT, KEK_CERT, DB_CERT] {
            inputs = inputs.file(&dir.join(cert))?;
        }
        let stamp = Stamp::new(paths, "test-enrollment", inputs, &[&enroll]);
        if !stamp.is_current() {
            stamp.clear()?;
            enrollment_files(&dir, &enroll, &microsoft_dir(paths), profile)?;
            stamp.write()?;
        }
        Ok(Self {
            dir,
            enroll,
            _memory: None,
        })
    }

    /// The owner's signing keys: db, PCR and module key from `store`,
    /// certificates and enrollment files from `public`. Fails unless they
    /// belong together.
    pub fn production(store: &Store, public: &Path) -> Result<Self> {
        for file in [DB_CERT, PCR_PUBLIC]
            .iter()
            .chain(&ENROLL_FILES)
            .chain(&PROFILE_FILES)
        {
            ensure!(
                public.join(file).exists(),
                "{} is missing; create the production keys with `bz secureboot new-keys`",
                public.join(file).display()
            );
        }
        ensure!(
            public.join(UPDATE_PUBLIC).exists(),
            "{} is missing; create the update key with `bz secureboot new-update-key`",
            public.join(UPDATE_PUBLIC).display()
        );
        ensure!(
            public.join(MODULE_CERT).exists(),
            "{} is missing; create the module key with `bz secureboot new-module-key`",
            public.join(MODULE_CERT).display()
        );
        let memory = MemoryDir::new()?;
        for (title, file) in [DB_SECRET, PCR_SECRET, MODULE_SECRET] {
            write_private(&memory.0.join(file), &store.get(title)?)?;
        }
        for file in [DB_CERT, PCR_PUBLIC, UPDATE_PUBLIC, MODULE_CERT] {
            fs::copy(public.join(file), memory.0.join(file))?;
        }
        ensure!(
            public_key_of(&memory.0.join(DB_KEY))? == cert_public_key(&memory.0.join(DB_CERT))?,
            "the signing key from the key store does not match {}",
            public.join(DB_CERT).display()
        );
        ensure!(
            public_key_of(&memory.0.join(MODULE_KEY))?
                == cert_public_key(&memory.0.join(MODULE_CERT))?,
            "the module key from the key store does not match {}",
            public.join(MODULE_CERT).display()
        );
        ensure!(
            public_key_of(&memory.0.join(PCR_KEY))?
                == fs::read_to_string(memory.0.join(PCR_PUBLIC))?.trim(),
            "the PCR key from the key store does not match {}",
            public.join(PCR_PUBLIC).display()
        );
        Ok(Self {
            dir: memory.0.clone(),
            enroll: public.to_owned(),
            _memory: Some(memory),
        })
    }

    /// Adds the private update key from `store`, for signing a release; fails
    /// unless it matches the public half these keys were loaded with.
    pub fn with_update_key(self, store: &Store) -> Result<Self> {
        let key = self.dir.join(UPDATE_KEY);
        write_private(&key, &store.get(UPDATE_SECRET.0)?)?;
        ensure!(
            public_key_of(&key)? == fs::read_to_string(self.update_public())?.trim(),
            "the update key from the key store does not match {UPDATE_PUBLIC}"
        );
        Ok(self)
    }

    /// The public update key: beize-updated accepts only updates signed by
    /// its private half (ADR 0020).
    pub fn update_public(&self) -> PathBuf {
        self.dir.join(UPDATE_PUBLIC)
    }

    /// The module key and its certificate, which the kernel of these keys
    /// has built in (ADR 0015).
    pub fn module_key(&self) -> (PathBuf, PathBuf) {
        (self.dir.join(MODULE_KEY), self.dir.join(MODULE_CERT))
    }

    /// Signs `file` with the update key into `signature` (a raw ML-DSA-65
    /// signature over the whole file) and checks the signature.
    pub fn sign_update(&self, file: &Path, signature: &Path) -> Result<()> {
        let key = self.dir.join(UPDATE_KEY);
        ensure!(
            key.exists(),
            "signing an update needs the private update key"
        );
        util::output(
            Command::new("openssl")
                .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
                .arg(&key)
                .arg("-in")
                .arg(file)
                .arg("-out")
                .arg(signature),
        )
        .with_context(|| format!("signing {}", file.display()))?;
        verify_update(&self.update_public(), file, signature)
    }

    /// Key pair that signs the expected PCR 11 values of a UKI (`.pcrsig`).
    /// The TPM accepts the signature as policy: secrets sealed to the public
    /// key unlock for every UKI signed with it, so they survive updates.
    pub fn pcr_key(&self) -> (PathBuf, PathBuf) {
        (self.dir.join(PCR_KEY), self.dir.join(PCR_PUBLIC))
    }

    /// Directory with the signed enrollment files ([`ENROLL_FILES`]).
    pub fn enroll_dir(&self) -> &Path {
        &self.enroll
    }

    /// Signs an EFI binary with the db key and checks the signature. The
    /// signer is systemd-sbsign from Beize's own systemd build: it dates the
    /// signature by SOURCE_DATE_EPOCH (sbsign takes the current time), so
    /// the same build gives the same signed binary (#5).
    pub fn sign(&self, paths: &Paths, input: &Path, output: &Path) -> Result<()> {
        let cert = self.dir.join(DB_CERT);
        util::output(
            usr::sysroot_command(paths, "usr/lib/systemd/systemd-sbsign")
                .env("SOURCE_DATE_EPOCH", BUILD_EPOCH.to_string())
                .arg("sign")
                .arg("--private-key")
                .arg(self.dir.join(DB_KEY))
                .arg("--certificate")
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
        say!("  signed {}", output.display());
        Ok(())
    }
}

/// Creates the production keys: generated in memory, private halves put into
/// `store` and read back for comparison; certificates and the signed
/// enrollment files for `profile` written to `public`.
pub fn new_keys(store: &Store, public: &Path, microsoft: &Path, profile: Profile) -> Result<()> {
    ensure!(
        !public.join(DB_CERT).exists(),
        "production keys exist already ({}); replacing them is a deliberate key \
         rotation, not something to do by accident",
        public.display()
    );
    let memory = MemoryDir::new()?;
    util::step("Generating the production keys (in memory only)");
    generate_cert_key(&memory.0, "PK", "Beize platform key")?;
    generate_cert_key(&memory.0, "KEK", "Beize key exchange key")?;
    generate_cert_key(&memory.0, "db", "Beize Secure Boot signing key")?;
    generate_pcr_key(&memory.0)?;
    let enroll = memory.0.join("enroll");
    enrollment_files(&memory.0, &enroll, microsoft, profile)?;

    util::step("Storing the private keys");
    store_secrets(
        store,
        &memory.0,
        &[PK_SECRET, KEK_SECRET, DB_SECRET, PCR_SECRET],
    )?;

    util::step("Writing the public keys and enrollment files");
    fs::create_dir_all(public)?;
    let mut written = Vec::new();
    for file in [PK_CERT, KEK_CERT, DB_CERT, PCR_PUBLIC] {
        fs::copy(memory.0.join(file), public.join(file))?;
        written.push(public.join(file));
    }
    for file in ENROLL_FILES.iter().chain(&PROFILE_FILES) {
        fs::copy(enroll.join(file), public.join(file))?;
        written.push(public.join(file));
    }
    drop(memory);
    if !public.join(UPDATE_PUBLIC).exists() {
        written.push(create_update_key(store, public)?);
    }
    if !public.join(MODULE_CERT).exists() {
        written.push(create_module_key(store, public)?);
    }
    tell_to_commit(&written);
    Ok(())
}

/// Creates the production module key (ADR 0015), for owners whose Secure
/// Boot keys predate it: the private half into `store`, the certificate to
/// `public`. Refuses to replace an existing one.
pub fn new_module_key(store: &Store, public: &Path) -> Result<()> {
    ensure!(
        !public.join(MODULE_CERT).exists(),
        "the module key exists already ({}); replacing it is a deliberate key \
         rotation, not something to do by accident",
        public.join(MODULE_CERT).display()
    );
    let written = create_module_key(store, public)?;
    tell_to_commit(&[written]);
    Ok(())
}

fn create_module_key(store: &Store, public: &Path) -> Result<PathBuf> {
    let memory = MemoryDir::new()?;
    util::step("Generating the module key (in memory only)");
    generate_module_key(&memory.0, "Beize kernel module signing key")?;
    store_secrets(store, &memory.0, &[MODULE_SECRET])?;
    fs::create_dir_all(public)?;
    fs::copy(memory.0.join(MODULE_CERT), public.join(MODULE_CERT))?;
    Ok(public.join(MODULE_CERT))
}

/// Creates the production update key (ADR 0020), for owners whose Secure
/// Boot keys predate it: the private half into `store`, the public half to
/// `public`. Refuses to replace an existing one.
pub fn new_update_key(store: &Store, public: &Path) -> Result<()> {
    ensure!(
        !public.join(UPDATE_PUBLIC).exists(),
        "the update key exists already ({}); replacing it is a deliberate key \
         rotation, not something to do by accident",
        public.join(UPDATE_PUBLIC).display()
    );
    let written = create_update_key(store, public)?;
    tell_to_commit(&[written]);
    Ok(())
}

fn create_update_key(store: &Store, public: &Path) -> Result<PathBuf> {
    let memory = MemoryDir::new()?;
    util::step("Generating the update key (in memory only)");
    generate_update_key(&memory.0)?;
    store_secrets(store, &memory.0, &[UPDATE_SECRET])?;
    fs::create_dir_all(public)?;
    fs::copy(memory.0.join(UPDATE_PUBLIC), public.join(UPDATE_PUBLIC))?;
    Ok(public.join(UPDATE_PUBLIC))
}

/// Puts private keys from `dir` into `store` and reads them back for comparison.
fn store_secrets(store: &Store, dir: &Path, secrets: &[(&str, &str)]) -> Result<()> {
    for &(title, file) in secrets {
        let secret = fs::read(dir.join(file))?;
        store.put(title, file, &secret)?;
        ensure!(
            store.get(title)? == secret,
            "\"{title}\" reads back differently from the key store; remove it there \
             and run this again"
        );
    }
    Ok(())
}

fn tell_to_commit(written: &[PathBuf]) {
    say!(
        "  {}\n  The private keys are in the key store only. Commit the files above \
         (see keys/secureboot/README.md).",
        written
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Checks a raw ML-DSA signature of `file` against the public update key.
pub fn verify_update(public: &Path, file: &Path, signature: &Path) -> Result<()> {
    util::output(
        Command::new("openssl")
            .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
            .arg(public)
            .arg("-in")
            .arg(file)
            .arg("-sigfile")
            .arg(signature),
    )
    .with_context(|| format!("verifying the signature of {}", file.display()))
    .map(drop)
}

/// Writes the signed variable updates systemd-boot enrolls into `out`: PK
/// (signed by itself), KEK (signed by PK), db (signed by KEK) and
/// Microsoft's dbx; plus the unsigned lists as the profile ([`PROFILE_FILES`]).
/// `keys` holds PK and KEK with private keys and db's certificate.
fn enrollment_files(keys: &Path, out: &Path, microsoft: &Path, profile: Profile) -> Result<()> {
    tree::recreate(out)?;
    let scratch = out.join("lists");
    fs::create_dir_all(&scratch)?;

    let (kek_extra, db_extra): (&[Pinned], &[Pinned]) = match profile {
        Profile::WithWindows => (&MICROSOFT_KEK, &MICROSOFT_DB),
    };
    let mut lists = Vec::new();
    for (var, own_cert, extra) in [
        ("PK", PK_CERT, &[][..]),
        ("KEK", KEK_CERT, kek_extra),
        ("db", DB_CERT, db_extra),
    ] {
        let der = scratch.join(format!("{var}.der"));
        util::output(
            Command::new("openssl")
                .args(["x509", "-outform", "DER", "-in"])
                .arg(keys.join(own_cert))
                .arg("-out")
                .arg(&der),
        )?;
        let mut list = signature_list(&der, OWNER_GUID, &scratch)?;
        if var == "db" {
            // Beize's own db certificate: beize-tpm requires it to have
            // authorised the boot chain.
            fs::write(out.join("beize-db.esl"), &list)?;
        }
        for (file, sha256) in extra {
            list.extend(signature_list(
                &microsoft_file(microsoft, file, sha256)?,
                MICROSOFT_GUID,
                &scratch,
            )?);
        }
        // Also kept as the profile beize-tpm checks the firmware against.
        let path = out.join(format!("{var}.esl"));
        fs::write(&path, list)?;
        lists.push((var, path));
    }
    for ((var, list), (signer_key, signer_cert)) in
        lists
            .iter()
            .zip([(PK_KEY, PK_CERT), (PK_KEY, PK_CERT), (KEK_KEY, KEK_CERT)])
    {
        util::output(
            Command::new("sbvarsign")
                .arg("--key")
                .arg(keys.join(signer_key))
                .arg("--cert")
                .arg(keys.join(signer_cert))
                .args(["--attr", VARIABLE_ATTRS])
                .arg("--output")
                .arg(out.join(format!("{var}.auth")))
                .arg(var)
                .arg(list),
        )
        .with_context(|| format!("signing the {var} update"))?;
    }
    let (dbx, sha256) = MICROSOFT_DBX;
    fs::copy(
        microsoft_file(microsoft, dbx, sha256)?,
        out.join("dbx.auth"),
    )?;
    fs::remove_dir_all(&scratch)?;
    Ok(())
}

/// An EFI signature list holding one X.509 certificate (DER).
fn signature_list(der: &Path, owner: &str, scratch: &Path) -> Result<Vec<u8>> {
    let out = scratch.join("one.esl");
    util::output(
        Command::new("sbsiglist")
            .args(["--owner", owner, "--type", "x509", "--output"])
            .arg(&out)
            .arg(der),
    )?;
    Ok(fs::read(&out)?)
}

/// A committed Microsoft object, checked against its pinned hash.
fn microsoft_file(dir: &Path, file: &str, sha256: &str) -> Result<PathBuf> {
    let path = dir.join(file);
    let actual = util::sha256_file(&path)?;
    ensure!(
        actual == sha256,
        "{} has SHA-256 {actual}, expected {sha256}",
        path.display()
    );
    Ok(path)
}

fn public_key_of(private_key: &Path) -> Result<String> {
    util::output(
        Command::new("openssl")
            .args(["pkey", "-pubout", "-in"])
            .arg(private_key),
    )
}

fn cert_public_key(cert: &Path) -> Result<String> {
    util::output(
        Command::new("openssl")
            .args(["x509", "-noout", "-pubkey", "-in"])
            .arg(cert),
    )
}

/// Creates the RSA-2048 key pair for signed PCR policies; RSA-2048 is what
/// every TPM 2.0 supports for TPM2_PolicyAuthorize.
fn generate_pcr_key(dir: &Path) -> Result<()> {
    let key = dir.join(PCR_KEY);
    util::output(
        Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .arg("-out")
            .arg(&key),
    )
    .context("generating the PCR signing key")?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    fs::write(dir.join(PCR_PUBLIC), public_key_of(&key)? + "\n")?;
    Ok(())
}

/// Creates the ML-DSA-65 key pair that signs updates: post-quantum like the
/// kernel's module signatures (ADR 0015), checked with OpenSSL on Beize.
fn generate_update_key(dir: &Path) -> Result<()> {
    let key = dir.join(UPDATE_KEY);
    util::output(
        Command::new("openssl")
            .args(["genpkey", "-algorithm", "ML-DSA-65", "-out"])
            .arg(&key),
    )
    .context("generating the update signing key")?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    fs::write(dir.join(UPDATE_PUBLIC), public_key_of(&key)? + "\n")?;
    Ok(())
}

/// Creates the ML-DSA-87 key that signs kernel modules and its self-signed
/// certificate, with the extensions kbuild gives the key it would generate
/// (`certs/default_x509.genkey`). The kernel ignores the expiry date.
fn generate_module_key(dir: &Path, common_name: &str) -> Result<()> {
    let key = dir.join(MODULE_KEY);
    util::output(
        Command::new("openssl")
            .args(["req", "-new", "-x509", "-newkey", "ml-dsa-87", "-nodes"])
            .args(["-days", "36500", "-subj", &format!("/CN={common_name}/")])
            .args(["-addext", "basicConstraints=critical,CA:FALSE"])
            .args(["-addext", "keyUsage=digitalSignature"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(dir.join(MODULE_CERT)),
    )
    .context("generating the module signing key")?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Creates `name.key` and its self-signed certificate `name.crt`. RSA-2048 is
/// what every UEFI implementation must accept; larger keys fail on some
/// mainboards. Firmware does not check certificate expiry.
fn generate_cert_key(dir: &Path, name: &str, common_name: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_microsoft_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../keys/microsoft")
    }

    #[test]
    fn microsoft_objects_match_their_pins() {
        let dir = repo_microsoft_dir();
        for (file, sha256) in MICROSOFT_KEK
            .iter()
            .chain(&MICROSOFT_DB)
            .chain([&MICROSOFT_DBX])
        {
            microsoft_file(&dir, file, sha256).unwrap();
        }
    }

    #[test]
    fn production_keys_must_match_their_public_halves() {
        let dir = std::env::temp_dir().join(format!("bz-keys-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::Directory(dir.join("store"));
        let public = dir.join("public");
        let microsoft = repo_microsoft_dir();

        new_keys(&store, &public, &microsoft, Profile::WithWindows).unwrap();
        let fresh_keys_load = Keys::production(&store, &public).is_ok();
        let second_run_refused =
            new_keys(&store, &public, &microsoft, Profile::WithWindows).is_err();
        let second_update_key_refused = new_update_key(&store, &public).is_err();
        let second_module_key_refused = new_module_key(&store, &public).is_err();
        let enroll_files = ENROLL_FILES.map(|file| public.join(file).exists());
        let profile_files = PROFILE_FILES.map(|file| public.join(file).exists());
        let module_cert = public.join(MODULE_CERT).exists();
        let private_halves_public = [PK_KEY, KEK_KEY, DB_KEY, PCR_KEY, UPDATE_KEY, MODULE_KEY]
            .iter()
            .any(|file| public.join(file).exists());

        // The update key signs, and its signature checks against /usr's key.
        let list = dir.join("SHA256SUMS");
        fs::write(&list, "list\n").unwrap();
        let signature = dir.join("SHA256SUMS.sig");
        let keys = Keys::production(&store, &public)
            .and_then(|keys| keys.with_update_key(&store))
            .unwrap();
        keys.sign_update(&list, &signature).unwrap();
        let signed = verify_update(&public.join(UPDATE_PUBLIC), &list, &signature).is_ok();
        fs::write(&list, "list!\n").unwrap();
        let changed_accepted =
            verify_update(&public.join(UPDATE_PUBLIC), &list, &signature).is_ok();
        drop(keys);

        // Keys that belong to another key set must be rejected.
        let other = dir.join("other");
        fs::create_dir_all(&other).unwrap();
        generate_update_key(&other).unwrap();
        fs::copy(
            other.join(UPDATE_KEY),
            dir.join("store").join(UPDATE_SECRET.0),
        )
        .unwrap();
        let update_mismatch = Keys::production(&store, &public)
            .and_then(|keys| keys.with_update_key(&store))
            .err()
            .map(|e| e.to_string());
        generate_module_key(&other, "other").unwrap();
        let module_secret = dir.join("store").join(MODULE_SECRET.0);
        let own_module_key = fs::read(&module_secret).unwrap();
        fs::copy(other.join(MODULE_KEY), &module_secret).unwrap();
        let module_mismatch = Keys::production(&store, &public)
            .err()
            .map(|e| e.to_string());
        fs::write(&module_secret, own_module_key).unwrap();
        generate_pcr_key(&other).unwrap();
        fs::copy(other.join(PCR_KEY), dir.join("store").join(PCR_SECRET.0)).unwrap();
        let mismatch = Keys::production(&store, &public)
            .err()
            .map(|e| e.to_string());
        fs::remove_dir_all(&dir).unwrap();

        assert!(fresh_keys_load);
        assert!(second_run_refused);
        assert!(second_update_key_refused);
        assert!(second_module_key_refused);
        assert!(module_cert, "new-keys made no module key");
        assert!(
            module_mismatch.is_some_and(|e| e.contains("module key")),
            "a foreign module key was accepted"
        );
        assert!(signed, "a signed list did not verify");
        assert!(!changed_accepted, "a changed list verified");
        assert!(
            update_mismatch.is_some_and(|e| e.contains("update key")),
            "a foreign update key was accepted"
        );
        assert_eq!(enroll_files, [true; 4]);
        assert_eq!(profile_files, [true; 4]);
        assert!(
            !private_halves_public,
            "a private key ended up in the public files"
        );
        assert!(
            mismatch.is_some_and(|e| e.contains("PCR key")),
            "a foreign PCR key was accepted"
        );
    }
}
