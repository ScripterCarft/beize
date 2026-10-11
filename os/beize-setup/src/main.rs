//! `beize-setup` – Beize's first-boot setup on the text console, until the
//! graphical onboarding (M5) replaces it.
//!
//! `beize-setup owner` first asks for the keyboard layout and switches to it
//! at once (ADR 0017), so that everything after it – the password above all –
//! is typed with the owner's own keyboard. Then it asks for a login name and
//! a password (twice) and has Beize's account service create the owner's
//! account (ADR 0019): an administrator (wheel: run0 with their own password,
//! ADR 0018). It locks root and asks for the time zone (/etc/localtime). Runs
//! once; afterwards /var/lib/beize/owner exists.

mod accounts;
mod keyboard;
mod mute;
mod timezone;
mod transaction;

use std::ffi::c_int;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use beize_accounts::{Client, NewAccount, Secret};
use transaction::write;

const SHADOW: &str = "/etc/shadow";
/// Written last: the setup is done and never runs again.
const DONE: &str = "/var/lib/beize/owner";
/// The account being created (see `transaction`).
const PENDING: &str = "/var/lib/beize/setup-pending";

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
    // The setup runs as root: the account service asks it for no password.
    let mut accounts = Client::connect()?;
    // A setup cut off after creating the account (power cut, reset): back
    // to before it.
    let pending = Path::new(PENDING);
    if let Some(name) = transaction::unfinished(pending)? {
        if beize_accounts::exists(&name) {
            accounts.remove(&name)?;
        }
        transaction::finish(pending)?;
        eprintln!("beize-setup: removed the account \"{name}\" of a setup that did not finish");
    }
    let shadow = read(Path::new(SHADOW))?;

    // First mute, then clear: nothing the boot prints afterwards lands in
    // the middle of the questions. Unmuted when `_muted` goes out of scope.
    let _muted = mute::console();
    keyboard_signals();
    print!("\x1b[H\x1b[2J");
    println!();
    println!("  Welcome to Beize. Create your account – you sign in with it and confirm");
    println!("  changes to the system with its password. Ctrl+C starts over.");
    println!();
    let Owner {
        name,
        full_name,
        password,
        zone,
    } = loop {
        match questions()? {
            Some(owner) => break owner,
            None => {
                println!();
                println!("  Starting over.");
                println!();
            }
        }
    };

    // Nothing is written before all answers are in; from here on, all of it
    // or none (a cut-off setup is undone on the next boot).
    write(Path::new(SHADOW), &accounts::lock_root(&shadow)?, 0o000)?;
    transaction::begin(pending, &name)?;
    accounts.create(
        &NewAccount {
            name: &name,
            real_name: &full_name,
            password: password.expose(),
            administrator: true,
        },
        None,
    )?;
    set_time_zone(&zone)?;
    write(Path::new(DONE), &format!("{name}\n"), 0o644)?;
    transaction::finish(pending)?;
    println!();
    println!("  Account \"{name}\" created. Sign in below.");
    println!();
    Ok(())
}

/// What the owner answered.
struct Owner {
    name: String,
    full_name: String,
    password: Secret,
    zone: String,
}

/// The setup's questions; `None` when the owner cancelled one (Ctrl+C,
/// Ctrl+D) to start over.
fn questions() -> Result<Option<Owner>, String> {
    if !choose_keyboard()? {
        return Ok(None);
    }
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
        match beize_accounts::validate_name(&name) {
            Ok(()) if beize_accounts::exists(&name) => {
                println!("  \"{name}\" exists already.");
            }
            Ok(()) => break name,
            Err(reason) => println!("  {reason}"),
        }
    };
    let full_name = answer!("Your full name (optional):", true);
    let password = loop {
        let first = Secret::new(answer!("Password:", false));
        if let Err(reason) = beize_accounts::validate_password(first.expose()) {
            println!("  {reason}");
            continue;
        }
        if Secret::new(answer!("Password again:", false)).expose() == first.expose() {
            break first;
        }
        println!("  The passwords differ.");
    };
    let Some(zone) = ask_time_zone()? else {
        return Ok(None);
    };
    Ok(Some(Owner {
        name,
        full_name,
        password,
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

/// Asks for the keyboard layout and switches to it; `false` if cancelled.
/// Offered is what the console can load (see `keyboard`); the firmware's
/// layout, if it reports one, is the suggestion. Unlike the other answers it
/// takes effect at once: the questions after it are typed with it. A setup
/// cut off later leaves only the keyboard set – asked again next time.
fn choose_keyboard() -> Result<bool, String> {
    let model_map = read(keyboard::MODEL_MAP)?;
    let xkb_names = read(keyboard::XKB_NAMES)?;
    let keymaps = keyboard::keymap_names(Path::new(keyboard::KEYMAPS));
    let layouts = keyboard::offered(&model_map, &xkb_names, |name| keymaps.contains(name));
    let suggested = keyboard::firmware_tag().and_then(|tag| keyboard::for_tag(&tag, &layouts));
    let first = match suggested {
        Some(layout) => format!(
            "Your keyboard layout – Enter for {} ({}), or a language, country or code:",
            layout.description,
            layout.code()
        ),
        None => "Your keyboard layout – a language, country or code (e.g. German, de):".to_owned(),
    };
    let mut question = first.clone();
    loop {
        let Some(answer) = ask(&question, true)? else {
            return Ok(false);
        };
        let layout = match (answer.trim().is_empty(), suggested) {
            (true, Some(layout)) => layout.clone(),
            (true, None) => continue,
            (false, _) => match keyboard::find(&answer, &layouts) {
                keyboard::Found::Layout(layout) => layout,
                keyboard::Found::Several(layouts) => {
                    println!("  Several layouts fit:");
                    for layout in layouts {
                        println!("    {:<22} {}", layout.code(), layout.description);
                    }
                    question = "Which one?".to_owned();
                    continue;
                }
                keyboard::Found::None => {
                    println!("  Not found. A language, a country or a code like de or fr, please.");
                    continue;
                }
            },
        };
        match set_keyboard(&layout) {
            Ok(()) => {
                println!("  Keyboard: {} ({}).", layout.description, layout.code());
                return Ok(true);
            }
            Err(error) => {
                println!("  The layout could not be set ({error}). Another one, please.");
                question = first.clone();
            }
        }
    }
}

/// Sets the layout through localed – both halves of the row, as chosen, not
/// converted – and waits until the console uses it: localed only asks for
/// systemd-vconsole-setup to be restarted, `systemctl restart` returns once
/// the keymap is loaded (and fails if it is not).
fn set_keyboard(layout: &keyboard::Layout) -> Result<(), String> {
    run(Command::new("localectl").args(["set-keymap", "--no-convert", &layout.keymap]))?;
    run(Command::new("localectl").args([
        "set-x11-keymap",
        "--no-convert",
        &layout.layout,
        &layout.model,
        &layout.variant,
        &layout.options,
    ]))?;
    run(Command::new("systemctl").args(["restart", "systemd-vconsole-setup.service"]))
}

/// Runs a command, its output to the journal only (the console is the
/// setup's).
fn run(command: &mut Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("starting {:?}: {e}", command.get_program()))?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    eprintln!("beize-setup: {:?}: {}", command.get_program(), error.trim());
    Err(format!("{:?} failed", command.get_program()))
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
