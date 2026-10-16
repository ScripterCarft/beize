//! Where the owner's private keys are kept: their 1Password vault, reached
//! only from an interactive terminal (ADR 0010), and on the build machine
//! only in a private directory in memory.

use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail, ensure};

use crate::say;
use crate::util;

/// The 1Password vault that holds the keys.
const VAULT: &str = "Beize";

/// Where the owner's private keys are kept.
pub enum Store {
    /// The owner's 1Password vault, through the 1Password CLI; `account`
    /// selects one of several signed-in accounts (`BZ_OP_ACCOUNT`).
    OnePassword { op: String, account: Option<String> },
    /// A plain directory; only for testing the owner's workflow.
    Directory(PathBuf),
}

impl Store {
    /// The owner's 1Password vault. Refuses to run without a terminal: only
    /// the owner, typing the command, may reach the production keys – never
    /// CI or other automation (ADR 0010).
    pub fn one_password() -> Result<Self> {
        ensure!(
            std::io::stdin().is_terminal(),
            "this command uses the production keys and only runs in an interactive \
             terminal, started by the owner"
        );
        // Under WSL only the Windows CLI talks to the 1Password app.
        let op = match env::var("BZ_OP") {
            Ok(op) => op,
            Err(_) => ["op.exe", "op"]
                .into_iter()
                .find(|op| on_path(op))
                .context(
                    "the 1Password CLI was not found: install it on Windows (op.exe, \
                     reachable from WSL) or set BZ_OP",
                )?
                .to_owned(),
        };
        let account = choose_account(&op)?;
        Ok(Self::OnePassword { op, account })
    }

    /// The 1Password CLI, with the account if one is set.
    fn op(op: &str, account: &Option<String>) -> Command {
        let mut cmd = Command::new(op);
        if let Some(account) = account {
            cmd.args(["--account", account]);
        }
        cmd
    }

    pub(super) fn put(&self, title: &str, file_name: &str, secret: &[u8]) -> Result<()> {
        match self {
            Self::OnePassword { op, account } => {
                say!("  storing \"{title}\" in the 1Password vault \"{VAULT}\"");
                let mut cmd = Self::op(op, account);
                cmd.args(["document", "create", "-", "--vault", VAULT])
                    .args(["--title", title, "--file-name", file_name]);
                secret_command(&mut cmd, Some(secret)).map(drop)
            }
            Self::Directory(dir) => {
                fs::create_dir_all(dir)?;
                write_private(&dir.join(title), secret)
            }
        }
    }

    pub(super) fn get(&self, title: &str) -> Result<Vec<u8>> {
        match self {
            Self::OnePassword { op, account } => {
                say!("  reading \"{title}\" from the 1Password vault \"{VAULT}\"");
                let mut cmd = Self::op(op, account);
                cmd.args(["document", "get", title, "--vault", VAULT]);
                secret_command(&mut cmd, None)
            }
            Self::Directory(dir) => fs::read(dir.join(title))
                .with_context(|| format!("reading {}", dir.join(title).display())),
        }
    }
}

/// Where the owner's choice of 1Password account is remembered (outside the
/// repository: it is about this computer's 1Password, not about Beize).
fn account_file() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(config.join("beize/1password-account"))
}

/// A 1Password account as `op account list --format json` describes it.
#[derive(Debug, PartialEq)]
struct Account {
    id: String,
    email: String,
    url: String,
}

fn parse_accounts(json: &str) -> Result<Vec<Account>> {
    let list: serde_json::Value = serde_json::from_str(json)?;
    let field = |a: &serde_json::Value, key: &str| a[key].as_str().unwrap_or_default().to_owned();
    Ok(list
        .as_array()
        .context("op account list did not return a list")?
        .iter()
        .map(|a| Account {
            id: field(a, "account_uuid"),
            email: field(a, "email"),
            url: field(a, "url"),
        })
        .filter(|a| !a.id.is_empty())
        .collect())
}

/// The account whose vault "Beize" holds the keys. With several accounts
/// signed in op refuses to guess, so: `BZ_OP_ACCOUNT` if set (passed as
/// --account – environment variables do not reach Windows programs started
/// from WSL), else the remembered choice, else ask the owner.
fn choose_account(op: &str) -> Result<Option<String>> {
    if let Some(account) = env::var("BZ_OP_ACCOUNT").ok().filter(|a| !a.is_empty()) {
        return Ok(Some(account));
    }
    if let Some(saved) = account_file().and_then(|f| fs::read_to_string(f).ok()) {
        let saved = saved.trim();
        if !saved.is_empty() {
            return Ok(Some(saved.to_owned()));
        }
    }
    // Listing accounts unlocks nothing.
    let accounts = parse_accounts(&util::output(
        Command::new(op).args(["account", "list", "--format", "json"]),
    )?)?;
    if accounts.len() <= 1 {
        return Ok(None);
    }

    say!("\n  1Password: several accounts found");
    for (i, account) in accounts.iter().enumerate() {
        // whoami only reports; it does not ask to unlock.
        let unlocked = Command::new(op)
            .args(["whoami", "--account", &account.id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        say!(
            "    {}) {:<32} {:<32} {}",
            i + 1,
            account.email,
            account.url,
            if unlocked { "unlocked" } else { "locked" }
        );
    }
    let choice = loop {
        let answer = prompt(&format!(
            "  Which account holds the vault \"{VAULT}\"? [1-{}]: ",
            accounts.len()
        ))?;
        if let Some(account) = answer
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| accounts.get(i))
        {
            break account;
        }
    };
    let remember = prompt("  Remember this for next time? [Y/n]: ")?;
    if !remember.eq_ignore_ascii_case("n")
        && let Some(file) = account_file()
    {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&file, format!("{}\n", choice.id))?;
        say!(
            "  remembered in {} (delete it to choose again)",
            file.display()
        );
    }
    Ok(Some(choice.id.clone()))
}

fn prompt(question: &str) -> Result<String> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

/// A private directory in memory for secrets, removed when dropped.
pub(super) struct MemoryDir(pub(super) PathBuf);

impl MemoryDir {
    pub(super) fn new() -> Result<Self> {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let shm = Path::new("/dev/shm");
        // Directories of bz processes that are gone: left behind only if bz
        // was killed while it held keys.
        for entry in fs::read_dir(shm)? {
            let path = entry?.path();
            let pid = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("bz-keys-"))
                .and_then(|rest| rest.split('-').next());
            if pid.is_some_and(|pid| !Path::new("/proc").join(pid).exists()) {
                fs::remove_dir_all(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        let dir = shm.join(format!(
            "bz-keys-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for MemoryDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs a command that receives or returns a secret on stdin/stdout. Neither
/// is echoed or included in errors; only stderr is.
fn secret_command(cmd: &mut Command, stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    if let Some(secret) = stdin {
        let mut pipe = child.stdin.take().context("stdin")?;
        pipe.write_all(secret)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "{program} failed with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Writes a secret into a new file that only its owner can read from the
/// start, never through an existing file or symlink.
pub(super) fn write_private(path: &Path, secret: &[u8]) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(secret))
        .with_context(|| format!("writing {}", path.display()))
}

fn on_path(program: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_1password_account_list() {
        let json = r#"[
            {"url": "my.1password.eu", "email": "joe@example.org",
             "user_uuid": "U1", "account_uuid": "A1"},
            {"url": "marfisoft.1password.eu", "email": "joe@marfisoft.example",
             "user_uuid": "U2", "account_uuid": "A2"},
            {"url": "broken.example"}
        ]"#;
        let accounts = parse_accounts(json).unwrap();
        assert_eq!(
            accounts.len(),
            2,
            "entries without an account ID are skipped"
        );
        assert_eq!(accounts[1].id, "A2");
        assert_eq!(accounts[1].url, "marfisoft.1password.eu");
        assert!(parse_accounts("{}").is_err());
    }
}
