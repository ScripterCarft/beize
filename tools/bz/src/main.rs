//! `bz` – the Beize build tool.
//!
//! Turns the recipes and configuration in this repository into a bootable
//! disk image and runs it in QEMU.

mod boottimes;
mod buildroot;
mod cargo;
mod cgroup;
mod cve;
mod debuginfo;
mod elf;
mod files;
mod firmware;
mod hwcheck;
mod image;
mod initrd;
mod install;
mod installer;
mod jobserver;
mod kernel;
mod layout;
mod microcode;
mod outdated;
mod packages;
mod paths;
mod privileges;
mod recipe;
mod release;
mod sbom;
mod schedule;
mod secureboot;
mod security;
mod select;
mod source;
mod stamp;
mod tree;
mod usr;
mod util;
mod variant;
mod vm;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::paths::Paths;

#[derive(Parser)]
#[command(name = "bz", version, about = "Beize build tool")]
struct Cli {
    /// Show every command and all output (by default only failures, and
    /// boot consoles go to build/work/logs/boot)
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Fail instead of updating files bz keeps in the repository, such as
    /// kernel/beize.config (CI)
    #[arg(long, global = true)]
    locked: bool,
    /// The variant to build or test, a directory of images/ (ADR 0025):
    /// `desktop`, or `base` without the user interface
    #[arg(long, global = true, default_value = variant::DEFAULT)]
    variant: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch, configure and build the kernel
    Kernel,
    /// Build recipes and their dependencies (e.g. `bz package glibc`)
    Package {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Build the packages of the image and write the /usr image
    Usr,
    /// Build the initrd that goes into the Unified Kernel Image
    Initrd,
    /// Assemble and sign the Unified Kernel Image and write the disk image
    Image,
    /// Build kernel, /usr image, initrd and disk image
    Build,
    /// Boot the image in QEMU with an interactive serial console (state is kept between runs)
    Run,
    /// Boot a VM headless, run commands on its root shell and print their
    /// output, e.g. `bz debug 'journalctl -b -p warning'`
    Debug {
        /// `run` (the VM of `bz run`) or a machine `bz smoke` left in
        /// build/work/variant-<variant>
        #[arg(long, default_value = "run")]
        machine: String,
        /// Typed when the boot asks for the recovery key
        #[arg(long)]
        recovery_key: Option<String>,
        /// Another TPM state directory beside the machine, e.g. `smoke-other-tpm`
        #[arg(long)]
        tpm: Option<String>,
        /// Commands for the root shell; `@wait N` pauses N seconds,
        /// `@screendump FILE` saves the screen as PNG
        commands: Vec<String>,
    },
    /// Install the image on a fresh test disk, boot it twice and verify everything
    Smoke {
        /// Only install and boot twice (all checks of a normal start), skip
        /// the scenarios that tamper with firmware, TPM and image
        #[arg(long)]
        quick: bool,
    },
    /// Show where a boot spent its time, from its journal (e.g. one copied
    /// from real hardware, see docs/hardware.md)
    BootTimes {
        /// A journal directory or file, or a boot's `.json` journal kept by
        /// `bz smoke` in build/work/logs/boot/
        journal: std::path::PathBuf,
        /// Which boot, as `journalctl --boot` takes it: 0 the last, -1 the
        /// one before, or a boot ID
        #[arg(long, default_value = "0", allow_hyphen_values = true)]
        boot: String,
        /// Show pauses between kernel messages from this many milliseconds
        #[arg(long, default_value_t = 100)]
        gap: u64,
    },
    /// Show which PCI devices of a real machine the built image has a driver
    /// for, from that machine's journal (see docs/hardware.md)
    HwCheck {
        /// A journal directory or file
        journal: std::path::PathBuf,
        /// Which boot, as `journalctl --boot` takes it
        #[arg(long, default_value = "0", allow_hyphen_values = true)]
        boot: String,
    },
    /// Show which package installed a file (e.g. `bz owner /usr/bin/bash`)
    Owner { path: String },
    /// Download every source and check its pin and its maintainers' signature
    Verify,
    /// Show which sources have newer upstream releases
    Outdated,
    /// Show the known vulnerabilities (NVD) of what the image contains; fails
    /// on an unassessed one of CVSS 7.0 or more (ADR 0022)
    Cve,
    /// The owner's production Secure Boot keys (owner only, in a terminal)
    Secureboot {
        #[command(subcommand)]
        command: SecurebootCommand,
    },
    /// Sign the boot chain with the owner's keys from 1Password and write the
    /// disk image for real hardware to build/out/<variant>/production (owner only)
    Sign,
    /// Write the release of this version for real hardware to
    /// build/out/<variant>/release/<version>, signed with the owner's keys from
    /// 1Password: what beize-update installs (owner only, ADR 0020)
    Release,
    /// Write the signed image for real hardware onto an empty disk, named by
    /// its device and its GPT disk GUID (owner only, see keys/secureboot/README.md)
    Install {
        /// The whole disk, e.g. /dev/sdc
        disk: std::path::PathBuf,
        /// The disk's GPT GUID, as Windows shows it (Get-Disk)
        #[arg(long)]
        guid: String,
        /// Replace the Beize installed on this disk (erases its data)
        #[arg(long)]
        reinstall: bool,
    },
}

#[derive(Subcommand)]
enum SecurebootCommand {
    /// Create the production keys: private halves into 1Password, public
    /// halves into keys/secureboot
    #[command(name = "new-keys")]
    Keys,
    /// Create the update signing key (ADR 0020) for keys made before it:
    /// private half into 1Password, public half into keys/secureboot
    #[command(name = "new-update-key")]
    UpdateKey,
    /// Create the kernel module signing key (ADR 0015) for keys made before
    /// it: private half into 1Password, certificate into keys/secureboot
    #[command(name = "new-module-key")]
    ModuleKey,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    util::set_verbose(cli.verbose);
    util::set_locked(cli.locked);
    let paths = Paths::new(&cli.variant)?;
    // Everything runs in the buildroot (./bz enters it), except writing a
    // real disk: that needs sudo and the machine's disks, and builds nothing.
    if !matches!(cli.command, Command::Install { .. }) {
        buildroot::check(&paths)?;
    }

    match cli.command {
        Command::Kernel => build_toolchain(&paths).and_then(|()| kernel::build(&paths).map(drop)),
        Command::Package { names } => packages::build(&paths, &names).map(drop),
        Command::Usr => usr::build(&paths).map(drop),
        Command::Initrd => initrd::build(&paths).map(drop),
        Command::Image => image::build(&paths).map(drop),
        Command::Build => build_all(&paths),
        Command::Run => vm::run(&paths),
        Command::Debug {
            machine,
            recovery_key,
            tpm,
            commands,
        } => vm::debug(
            &paths,
            &machine,
            recovery_key.as_deref(),
            tpm.as_deref(),
            &commands,
        ),
        Command::Smoke { quick } => vm::smoke(&paths, quick),
        Command::BootTimes { journal, boot, gap } => boottimes::show(&journal, &boot, gap),
        Command::HwCheck { journal, boot } => hwcheck::show(&paths, &journal, &boot),
        Command::Outdated => outdated::show(&paths),
        Command::Cve => cve::show(&paths),
        Command::Owner { path } => {
            println!("{}", install::owner(&paths, &path)?);
            Ok(())
        }
        Command::Verify => verify_sources(&paths),
        Command::Secureboot {
            command: SecurebootCommand::Keys,
        } => secureboot::new_keys(
            &secureboot::Store::one_password()?,
            &secureboot::production_public_dir(&paths),
            &secureboot::microsoft_dir(&paths),
            secureboot::Profile::WithWindows,
        ),
        Command::Secureboot {
            command: SecurebootCommand::UpdateKey,
        } => secureboot::new_update_key(
            &secureboot::Store::one_password()?,
            &secureboot::production_public_dir(&paths),
        ),
        Command::Secureboot {
            command: SecurebootCommand::ModuleKey,
        } => secureboot::new_module_key(
            &secureboot::Store::one_password()?,
            &secureboot::production_public_dir(&paths),
        ),
        Command::Release => {
            let store = secureboot::Store::one_password()?;
            let dir = release::release(&paths, &store)?;
            say!(
                "
  ok: {} is signed with the production keys",
                dir.display()
            );
            Ok(())
        }
        Command::Install {
            disk,
            guid,
            reinstall,
        } => installer::install(&paths, &disk, &guid, reinstall),
        Command::Sign => {
            let store = secureboot::Store::one_password()?;
            let disk = image::sign(&paths, &store)?;
            say!(
                "
  ok: {} is signed with the production keys",
                disk.display()
            );
            Ok(())
        }
    }
}

fn verify_sources(paths: &Paths) -> Result<()> {
    let mut signed = 0;
    let mut unsigned = Vec::new();
    for name in recipe::all_names(paths)? {
        let recipe = recipe::Recipe::load(paths, &name)?;
        if recipe.source_from.is_some() {
            continue; // verified with the recipe it borrows from
        }
        for source in recipe.sources() {
            util::step(&format!("Verifying {name} ({})", source.stem()?));
            source.fetch(paths, true)?;
            match &source.trust {
                source::Trust::Signed { .. } => signed += 1,
                source::Trust::PinnedOnly { reason } => unsigned.push(format!("{name}: {reason}")),
            }
        }
    }
    say!("\n  ok: {signed} sources pinned and signed by their maintainers");
    if !unsigned.is_empty() {
        say!(
            "  {} sources pinned but UNSIGNED (upstream publishes no signatures):",
            unsigned.len()
        );
        for entry in &unsigned {
            say!("    - {entry}");
        }
    }
    Ok(())
}

/// The toolchain, then the kernel and the packages of the image side by
/// side, then the image's parts from them.
/// The toolchain the kernel is compiled with ([`kernel::TOOLCHAIN`]).
fn build_toolchain(paths: &Paths) -> Result<()> {
    packages::build(paths, &kernel::TOOLCHAIN.map(str::to_owned)).map(drop)
}

fn build_all(paths: &Paths) -> Result<()> {
    let image_packages = paths.variant.packages();
    // The kernel needs the toolchain, and so does every package but three
    // small data packages: little could run beside building it.
    build_toolchain(paths)?;
    std::thread::scope(|scope| {
        // What the kernel build says comes in one piece when it is done.
        let kernel = scope.spawn(|| util::capture(|| kernel::build(paths)));
        let packages = packages::build(paths, &image_packages);
        // A panic in the kernel build goes on as the panic it is.
        let (kernel, said) = kernel
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        for line in said {
            say!("{line}");
        }
        packages.and(kernel)
    })?;
    usr::build(paths)?;
    initrd::build(paths)?;
    image::build(paths)?;
    Ok(())
}
