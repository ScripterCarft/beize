//! `bz` – the Beize build tool.
//!
//! Turns the recipes and configuration in this repository into a bootable
//! disk image and runs it in QEMU.

mod image;
mod initrd;
mod install;
mod installer;
mod kernel;
mod layout;
mod packages;
mod paths;
mod qemu;
mod recipe;
mod secureboot;
mod source;
mod tree;
mod usr;
mod util;

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
    /// Install the image on a fresh test disk, boot it twice and verify everything
    Smoke,
    /// Show which package installed a file (e.g. `bz owner /usr/bin/bash`)
    Owner { path: String },
    /// Download every source and check its pin and its maintainers' signature
    Verify,
    /// The owner's production Secure Boot keys (owner only, in a terminal)
    Secureboot {
        #[command(subcommand)]
        command: SecurebootCommand,
    },
    /// Sign the boot chain with the owner's keys from 1Password and write the
    /// disk image for real hardware to build/out/production (owner only)
    Sign,
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
    NewKeys,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    util::set_verbose(cli.verbose);
    let paths = Paths::new()?;

    match cli.command {
        Command::Kernel => kernel::build(&paths).map(drop),
        Command::Package { names } => packages::build(&paths, &names).map(drop),
        Command::Usr => usr::build(&paths).map(drop),
        Command::Initrd => initrd::build(&paths).map(drop),
        Command::Image => image::build(&paths).map(drop),
        Command::Build => build_all(&paths),
        Command::Run => qemu::run(&paths),
        Command::Smoke => qemu::smoke(&paths),
        Command::Owner { path } => {
            println!("{}", install::owner(&paths, &path)?);
            Ok(())
        }
        Command::Verify => verify_sources(&paths),
        Command::Secureboot {
            command: SecurebootCommand::NewKeys,
        } => secureboot::new_keys(
            &secureboot::Store::one_password()?,
            &secureboot::production_public_dir(&paths),
            &secureboot::microsoft_dir(&paths),
            secureboot::Profile::WithWindows,
        ),
        Command::Install {
            disk,
            guid,
            reinstall,
        } => installer::install(&paths, &disk, &guid, reinstall),
        Command::Sign => {
            let store = secureboot::Store::one_password()?;
            let disk = image::sign(&paths, &store)?;
            eprintln!(
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
    eprintln!("\n  ok: {signed} sources pinned and signed by their maintainers");
    if !unsigned.is_empty() {
        eprintln!(
            "  {} sources pinned but UNSIGNED (upstream publishes no signatures):",
            unsigned.len()
        );
        for entry in &unsigned {
            eprintln!("    - {entry}");
        }
    }
    Ok(())
}

fn build_all(paths: &Paths) -> Result<()> {
    kernel::build(paths)?;
    usr::build(paths)?;
    initrd::build(paths)?;
    image::build(paths)?;
    Ok(())
}
