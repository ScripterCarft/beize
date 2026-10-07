//! `bz` – the Beize build tool.
//!
//! Turns the recipes and configuration in this repository into a bootable
//! disk image and runs it in QEMU.

mod image;
mod initrd;
mod install;
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
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
    usr::build(paths)?;
    initrd::build(paths)?;
    image::build(paths)?;
    Ok(())
}
