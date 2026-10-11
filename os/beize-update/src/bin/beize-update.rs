//! `beize-update` – installs an update on the console (ADR 0020), until the
//! settings do it (M5): `run0 beize-update install <source>`, where the
//! source is a directory or an http(s):// URL holding a release made by
//! `bz release`.

use std::process::ExitCode;

use beize_update::{INTERFACE, Refused, SOCKET, call, error};
use beize_varlink::json;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, source] = &args[..] else {
        return usage();
    };
    if command != "install" {
        return usage();
    }
    println!("Fetching and checking the update …");
    match call(
        SOCKET,
        INTERFACE,
        "Install",
        &format!("\"source\":{}", json::string(source)),
    ) {
        Ok(reply) => {
            let version = reply
                .object()
                .object("parameters")
                .and_then(|p| p.str("version"))
                .unwrap_or("?");
            println!("Beize {version} is installed. It starts with the next restart.");
            ExitCode::SUCCESS
        }
        Err(Refused(name, reason)) => {
            let text = match name {
                error::NOT_ALLOWED => {
                    "Only an administrator can install updates: run0 beize-update …"
                }
                error::NO_UPDATE => "There is no newer version.",
                error::UNTRUSTED => {
                    "The update is not from Beize or was changed; nothing was installed."
                }
                _ => "The update could not be installed.",
            };
            eprintln!("{text}\n({reason})");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: beize-update install <directory or http(s):// URL>");
    ExitCode::from(2)
}
