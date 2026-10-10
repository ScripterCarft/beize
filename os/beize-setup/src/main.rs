//! `beize-setup` – Beize's first-boot setup on the text console, until the
//! graphical onboarding (M5) replaces it.
//!
//! `beize-setup owner` creates the owner's account: asks for a login name
//! and a password (twice), adds the user to /etc/passwd, /etc/group (own
//! group, and wheel: administrator) and /etc/shadow (yescrypt), gives root
//! the same password hash – the emergency shell asks for root's password –
//! and creates the home directory. It also asks for the time zone
//! (/etc/localtime). Runs once; afterwards /var/lib/beize/owner exists.

mod accounts;
mod crypt;
mod mute;
mod timezone;
mod transaction;

use std::ffi::c_int;
use std::fs;
use std::os::unix::fs::{PermissionsExt, chown, symlink};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use transaction::write;

const ETC: &str = "/etc";
/// Written last: the setup is done and never runs again.
const DONE: &str = "/var/lib/beize/owner";
/// The account databases as they were before the setup (see `transaction`).
const SAVED: &str = "/var/lib/beize/setup-rollback";

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
    // A setup cut off while writing (power cut, reset): back to before it.
    let (etc, saved) = (Path::new(ETC), Path::new(SAVED));
    if transaction::roll_back(etc, saved)? {
        eprintln!("beize-setup: undid an account setup that did not finish");
    }
    let passwd = read(etc.join("passwd"))?;
    let group = read(etc.join("group"))?;
    let shadow = read(etc.join("shadow"))?;

    // First mute, then clear: nothing the boot prints afterwards lands in
    // the middle of the questions. Unmuted when `_muted` goes out of scope.
    let _muted = mute::console();
    keyboard_signals();
    print!("\x1b[H\x1b[2J");
    println!();
    println!("  Welcome to Beize. Create your account – you sign in with it, and its");
    println!("  password also opens the emergency console. Ctrl+C starts over.");
    println!();
    let Owner {
        name,
        full_name,
        hash,
        zone,
    } = loop {
        match questions(&passwd, &group)? {
            Some(owner) => break owner,
            None => {
                println!();
                println!("  Starting over.");
                println!();
            }
        }
    };

    let id = accounts::free_id(&passwd, &group).ok_or("no free user ID")?;
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        / 86_400;
    // Nothing is written before all answers are in; from here on, all of it
    // or none (a cut-off setup is rolled back on the next boot).
    transaction::save(etc, saved)?;
    write(
        &etc.join("shadow"),
        &accounts::add_shadow(&shadow, &name, &hash, days)?,
        0o000,
    )?;
    write(
        &etc.join("group"),
        &accounts::add_group(&group, &name, id)?,
        0o644,
    )?;
    write(
        &etc.join("passwd"),
        &accounts::add_user(&passwd, &name, id, &full_name),
        0o644,
    )?;

    let home = Path::new("/home").join(&name);
    fs::create_dir_all(&home).map_err(|e| format!("creating {}: {e}", home.display()))?;
    chown(&home, Some(id), Some(id)).map_err(|e| format!("owning {}: {e}", home.display()))?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("protecting {}: {e}", home.display()))?;
    set_time_zone(&zone)?;

    write(Path::new(DONE), &format!("{name}\n"), 0o644)?;
    // After DONE: a copy left by a cut here is the state before the setup,
    // which never runs again – harmless.
    transaction::discard(saved)?;
    println!();
    println!("  Account \"{name}\" created. Sign in below.");
    println!();
    Ok(())
}

/// What the owner answered.
struct Owner {
    name: String,
    full_name: String,
    /// The password, hashed as soon as it is confirmed.
    hash: String,
    zone: String,
}

/// The setup's questions; `None` when the owner cancelled one (Ctrl+C,
/// Ctrl+D) to start over.
fn questions(passwd: &str, group: &str) -> Result<Option<Owner>, String> {
    macro_rules! answer {
        ($question:expr, $echo:expr) => {
            match ask($question, $echo)? {
                Some(answer) => answer,
                None => return Ok(None),
            }
        };
    }
    let name = loop {
        let name = answer!("Your login name (e.g. joe):", true);
        match accounts::validate_name(&name) {
            Ok(()) if accounts::exists(passwd, &name) || accounts::exists(group, &name) => {
                println!("  \"{name}\" exists already.");
            }
            Ok(()) => break name,
            Err(reason) => println!("  {reason}"),
        }
    };
    let full_name = answer!("Your full name (optional):", true);
    let password = loop {
        let first = answer!("Password:", false);
        if first.chars().count() < 8 {
            println!("  At least 8 characters, please.");
            continue;
        }
        if answer!("Password again:", false) == first {
            break first;
        }
        println!("  The passwords differ.");
    };
    let hash = crypt::hash(&password)?;
    drop(password);
    let Some(zone) = ask_time_zone()? else {
        return Ok(None);
    };
    Ok(Some(Owner {
        name,
        full_name,
        hash,
        zone,
    }))
}

const SIGINT: c_int = 2;
const SIGQUIT: c_int = 3;
const SIGTSTP: c_int = 20;
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
}

/// The setup owns the console, and its keys must not end it: the boot would
/// go on to the login without an account. Ctrl+Z (stop) and Ctrl+\ (quit)
/// do nothing; Ctrl+C ends only the current question (see [`ask`]), and the
/// setup starts over.
fn keyboard_signals() {
    for signum in [SIGINT, SIGQUIT, SIGTSTP] {
        // SAFETY: setting a disposition to SIG_IGN installs no handler.
        unsafe { signal(signum, SIG_IGN) };
    }
}

/// Asks on the console through systemd-ask-password (which hides passwords).
/// It asks on the terminal only if its stdin is one – otherwise it waits for
/// a password agent – so the service's console is passed on. `None` if the
/// owner cancelled: Ctrl+C ends systemd-ask-password (its SIGINT is reset to
/// the default for that), Ctrl+D cancels it.
fn ask(question: &str, echo: bool) -> Result<Option<String>, String> {
    let mut command = Command::new("systemd-ask-password");
    command
        .arg(if echo { "--echo=yes" } else { "--echo=no" })
        .args(["--timeout=0", "--id=beize-setup"])
        .arg(question)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit());
    // SAFETY: runs in the child between fork and exec; signal() is
    // async-signal-safe and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            signal(SIGINT, SIG_DFL);
            Ok(())
        });
    }
    let output = command
        .output()
        .map_err(|e| format!("starting systemd-ask-password: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let mut answer = String::from_utf8(output.stdout).map_err(|_| "the answer is not UTF-8")?;
    while answer.ends_with('\n') {
        answer.pop();
    }
    Ok(Some(answer))
}

/// Asks until the answer names exactly one time zone; `None` if cancelled.
fn ask_time_zone() -> Result<Option<String>, String> {
    let zones = read(format!("{}/zone1970.tab", timezone::ZONEINFO))?;
    let countries = read(format!("{}/iso3166.tab", timezone::ZONEINFO))?;
    let mut question = "Your time zone – a city or country (e.g. Berlin):";
    loop {
        let Some(answer) = ask(question, true)? else {
            return Ok(None);
        };
        match timezone::find(&answer, &zones, &countries) {
            timezone::Found::Zone(zone) => return Ok(Some(zone)),
            timezone::Found::Several(zones) => {
                println!("  Several time zones fit:");
                for zone in zones {
                    println!("    {zone}");
                }
                question = "Which one?";
            }
            timezone::Found::None => {
                println!("  Not found. A larger city nearby or the country, please.");
            }
        }
    }
}

/// Points /etc/localtime at the zone, as systemd expects (a relative link
/// into /usr/share/zoneinfo); replaced atomically.
fn set_time_zone(zone: &str) -> Result<(), String> {
    const LOCALTIME: &str = "/etc/localtime";
    let target = format!("..{}/{zone}", timezone::ZONEINFO);
    let temporary = format!("{LOCALTIME}.beize-setup");
    let _ = fs::remove_file(&temporary);
    symlink(&target, &temporary).map_err(|e| format!("linking {temporary}: {e}"))?;
    fs::rename(&temporary, LOCALTIME).map_err(|e| format!("replacing {LOCALTIME}: {e}"))
}

fn read(path: impl AsRef<Path>) -> Result<String, String> {
    transaction::read(path.as_ref())
}
