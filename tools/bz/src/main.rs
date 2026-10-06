//! `bz` – the Beize build tool.
//!
//! Turns the recipes and configuration in this repository into a bootable
//! disk image and runs it in QEMU.

mod image;
mod install;
mod kernel;
mod packages;
mod paths;
mod qemu;
mod recipe;
mod rootfs;
mod source;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::paths::Paths;

#[derive(Parser)]
#[command(name = "bz", version, about = "Beize build tool")]
struct Cli {
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
    /// Build the packages of the base image and assemble the root filesystem
    Rootfs,
    /// Assemble the Unified Kernel Image and the GPT disk image
    Image,
    /// Build kernel, root filesystem and disk image
    Build,
    /// Boot the disk image in QEMU with an interactive serial console
    Run,
    /// Boot the disk image headless and verify that the system comes up
    Smoke,
    /// Show which package installed a file (e.g. `bz owner /usr/bin/bash`)
    Owner { path: String },
    /// Download every source and check its pin and its maintainers' signature
    Verify,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;

    match cli.command {
        Command::Kernel => kernel::build(&paths).map(drop),
        Command::Package { names } => packages::build(&paths, &names).map(drop),
        Command::Rootfs => rootfs::build(&paths).map(drop),
        Command::Image => image::build(&paths).map(drop),
        Command::Build => build_all(&paths),
        Command::Run => qemu::run(&paths),
        Command::Smoke => qemu::smoke(&paths),
        Command::Owner { path } => {
            println!("{}", install::owner(&paths, &path)?);
            Ok(())
        }
        Command::Verify => verify_sources(&paths),
    }
}

fn verify_sources(paths: &Paths) -> Result<()> {
    let mut count = 0;
    for name in recipe::all_names(paths)? {
        let recipe = recipe::Recipe::load(paths, &name)?;
        if recipe.source_from.is_some() {
            continue; // verified with the recipe it borrows from
        }
        for source in recipe.sources() {
            util::step(&format!("Verifying {name} ({})", source.stem()?));
            source.fetch(paths, true)?;
            count += 1;
        }
    }
    eprintln!("\n  ok: {count} sources pinned and signed by their maintainers");
    Ok(())
}

fn build_all(paths: &Paths) -> Result<()> {
    kernel::build(paths)?;
    rootfs::build(paths)?;
    image::build(paths)?;
    Ok(())
}
