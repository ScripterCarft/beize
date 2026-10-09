//! `beize-setup` – Beize's first-boot setup on the text console, until the
//! graphical onboarding (M5) replaces it.
//!
//! `beize-setup owner` creates the owner's account: asks for a login name
//! and a password (twice), adds the user to /etc/passwd, /etc/group (own
//! group, and wheel: administrator) and /etc/shadow (yescrypt), gives root
//! the same password hash – the emergency shell asks for root's password –
//! and creates the home directory. Runs once; afterwards
//! /var/lib/beize/owner exists.

mod accounts;
mod crypt;
mod mute;

use std::fs;
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PASSWD: &str = "/etc/passwd";
const GROUP: &str = "/etc/group";
const SHADOW: &str = "/etc/shadow";
const DONE: &str = "/var/lib/beize/owner";

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("owner") => owner(),
        _ => Err("usage: beize-setup owner".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("beize-setup: {error}");
            ExitCode::FAILURE
        }
    }
}

fn owner() -> Result<(), String> {
    if Path::new(DONE).exists() {
        return Ok(());
    }
    let passwd = read(PASSWD)?;
    let group = read(GROUP)?;
    let shadow = read(SHADOW)?;

    // First mute, then clear: nothing the boot prints afterwards lands in
    // the middle of the questions. Unmuted when `_muted` goes out of scope.
    let _muted = mute::console();
    print!("\x1b[H\x1b[2J");
    println!();
    println!("  Welcome to Beize. Create your account – you sign in with it, and its");
    println!("  password also opens the emergency console.");
    println!();
    let name = loop {
        let name = ask("Your login name (e.g. joe):", true)?;
        match accounts::validate_name(&name) {
            Ok(()) if accounts::exists(&passwd, &name) || accounts::exists(&group, &name) => {
                println!("  \"{name}\" exists already.");
            }
            Ok(()) => break name,
            Err(reason) => println!("  {reason}"),
        }
    };
    let full_name = ask("Your full name (optional):", true)?;
    let password = loop {
        let first = ask("Password:", false)?;
        if first.chars().count() < 8 {
            println!("  At least 8 characters, please.");
            continue;
        }
        if ask("Password again:", false)? == first {
            break first;
        }
        println!("  The passwords differ.");
    };
    let hash = crypt::hash(&password)?;
    drop(password);

    let id = accounts::free_id(&passwd, &group).ok_or("no free user ID")?;
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        / 86_400;
    // shadow first: if anything after it fails, root's password is set and
    // the emergency console works; passwd last makes the account usable.
    write(
        SHADOW,
        &accounts::add_shadow(&shadow, &name, &hash, days)?,
        0o000,
    )?;
    write(GROUP, &accounts::add_group(&group, &name, id)?, 0o644)?;
    write(
        PASSWD,
        &accounts::add_user(&passwd, &name, id, &full_name),
        0o644,
    )?;

    let home = Path::new("/home").join(&name);
    fs::create_dir_all(&home).map_err(|e| format!("creating {}: {e}", home.display()))?;
    chown(&home, Some(id), Some(id)).map_err(|e| format!("owning {}: {e}", home.display()))?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("protecting {}: {e}", home.display()))?;

    write(DONE, &format!("{name}\n"), 0o644)?;
    println!();
    println!("  Account \"{name}\" created. Sign in below.");
    println!();
    Ok(())
}

/// Asks on the console through systemd-ask-password (which hides passwords).
/// It asks on the terminal only if its stdin is one – otherwise it waits for
/// a password agent – so the service's console is passed on.
fn ask(question: &str, echo: bool) -> Result<String, String> {
    let output = Command::new("systemd-ask-password")
        .arg(if echo { "--echo=yes" } else { "--echo=no" })
        .args(["--timeout=0", "--id=beize-setup"])
        .arg(question)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("starting systemd-ask-password: {e}"))?;
    if !output.status.success() {
        return Err("no answer on the console".into());
    }
    let mut answer = String::from_utf8(output.stdout).map_err(|_| "the answer is not UTF-8")?;
    while answer.ends_with('\n') {
        answer.pop();
    }
    Ok(answer)
}

fn read(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))
}

/// Replaces `path` atomically (temporary file in the same directory, then
/// rename), so a power cut never leaves a half-written account database.
fn write(path: &str, content: &str, mode: u32) -> Result<(), String> {
    let path = Path::new(path);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let temporary = path.with_extension("beize-setup");
    fs::write(&temporary, content).map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("protecting {}: {e}", temporary.display()))?;
    fs::File::open(&temporary)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("syncing {}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("replacing {}: {e}", path.display()))
}
