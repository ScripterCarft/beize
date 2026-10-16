//! Where Beize keeps people's accounts: systemd user records in /etc/userdb
//! (https://systemd.io/USER_RECORD/), which nss-systemd hands to every
//! program and to PAM like any account. Per account:
//!
//! - `<name>.user` – the record (world-readable), and `<uid>.user` linking to
//!   it: systemd looks records up by name and by ID;
//! - `<name>.user-privileged` – the password hash, root only;
//! - `<name>.group` – the person's own group, `<gid>.group` linking to it;
//! - `<name>:wheel.membership` – an administrator (ADR 0018).
//!
//! The service is the only writer; accounts without a `.user` file here
//! (root, system users in /etc/passwd) are not Beize's to change.

use std::ffi::{c_int, c_void};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, chown, symlink};
use std::path::{Path, PathBuf};

use beize_accounts::{Secret, is_name, json};

/// IDs for people: above system users, below systemd's own ranges.
const FIRST_ID: u32 = 1000;
const LAST_ID: u32 = 59999;
/// The administrators' group (polkit's and run0's, ADR 0018).
const ADMIN_GROUP: &str = "wheel";

const LOCK_EX: c_int = 2;

unsafe extern "C" {
    fn flock(fd: c_int, operation: c_int) -> c_int;
    fn getpwuid(uid: u32) -> *const crate::Passwd;
    fn getgrgid(gid: u32) -> *mut c_void;
}

pub struct Store {
    dir: PathBuf,
    homes: PathBuf,
}

/// Held while the accounts change: one change at a time.
pub struct Lock(#[allow(dead_code)] File);

impl Store {
    pub fn new(dir: impl Into<PathBuf>, homes: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            homes: homes.into(),
        }
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    pub fn lock(&self) -> Result<Lock, String> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&self.dir)
            .map_err(|e| format!("creating {}: {e}", self.dir.display()))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.path(".beize-accounts.lock"))
            .map_err(|e| format!("opening the lock: {e}"))?;
        // SAFETY: a valid open file descriptor.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
            return Err("cannot lock the accounts".into());
        }
        Ok(Lock(file))
    }

    /// Whether `name` is an account Beize keeps.
    pub fn has(&self, name: &str) -> bool {
        is_name(name) && self.path(&format!("{name}.user")).is_file()
    }

    /// The account's user ID.
    pub fn uid(&self, name: &str) -> Option<u32> {
        if !is_name(name) {
            return None;
        }
        let text = fs::read_to_string(self.path(&format!("{name}.user"))).ok()?;
        let record = json::Json::parse(&text, false).ok()?;
        record
            .object()
            .unsigned("uid")
            .and_then(|uid| uid.try_into().ok())
    }

    /// The account's password hash.
    pub fn hash(&self, name: &str) -> Option<Secret> {
        if !is_name(name) {
            return None;
        }
        let text =
            Secret::new(fs::read_to_string(self.path(&format!("{name}.user-privileged"))).ok()?);
        let record = json::Json::parse(text.expose(), true).ok()?;
        let hash = record
            .object()
            .object("privileged")?
            .first_str("hashedPassword")?;
        Some(Secret::new(hash.to_owned()))
    }

    /// Creates the account with its own group and an empty home; returns
    /// its user ID (= group ID).
    pub fn create(
        &self,
        _lock: &Lock,
        name: &str,
        real_name: &str,
        hash: &str,
        administrator: bool,
    ) -> Result<u32, String> {
        checked(name)?;
        let id = self.free_id().ok_or("no free user ID")?;
        let home = self.homes.join(name);
        if home.exists() {
            return Err(format!("{} exists already", home.display()));
        }
        // The record last: until it exists, the account does not.
        self.write_privileged(name, hash)?;
        write(
            &self.path(&format!("{name}.group")),
            &format!(
                "{{\"groupName\":{},\"gid\":{id},\"disposition\":\"regular\"}}\n",
                json::string(name)
            ),
            0o644,
        )?;
        let mut record = format!(
            "{{\"userName\":{},\"uid\":{id},\"gid\":{id},\"homeDirectory\":{},\
             \"shell\":\"/usr/bin/bash\",\"disposition\":\"regular\"",
            json::string(name),
            json::string(&home.to_string_lossy())
        );
        if !real_name.is_empty() {
            record.push_str(&format!(",\"realName\":{}", json::string(real_name)));
        }
        record.push_str("}\n");
        write(&self.path(&format!("{name}.user")), &record, 0o644)?;
        for (link, target) in [
            (format!("{id}.user"), format!("{name}.user")),
            (
                format!("{id}.user-privileged"),
                format!("{name}.user-privileged"),
            ),
            (format!("{id}.group"), format!("{name}.group")),
        ] {
            let link = self.path(&link);
            let _ = fs::remove_file(&link);
            symlink(&target, &link).map_err(|e| format!("linking {}: {e}", link.display()))?;
        }
        if administrator {
            write(
                &self.path(&format!("{name}:{ADMIN_GROUP}.membership")),
                "",
                0o644,
            )?;
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .map_err(|e| format!("creating {}: {e}", home.display()))?;
        chown(&home, Some(id), Some(id)).map_err(|e| format!("owning {}: {e}", home.display()))?;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("protecting {}: {e}", home.display()))?;
        sync_dir(&self.dir)?;
        Ok(id)
    }

    /// A new password hash.
    pub fn set_hash(&self, _lock: &Lock, name: &str, hash: &str) -> Result<(), String> {
        checked(name)?;
        self.write_privileged(name, hash)?;
        sync_dir(&self.dir)
    }

    /// Removes the account, its group and its home with everything in it.
    pub fn remove(&self, _lock: &Lock, name: &str) -> Result<(), String> {
        checked(name)?;
        let id = self.uid(name);
        // The record first: from then on the account is gone.
        let mut files = vec![
            format!("{name}.user"),
            format!("{name}.user-privileged"),
            format!("{name}.group"),
            format!("{name}:{ADMIN_GROUP}.membership"),
        ];
        if let Some(id) = id {
            files.extend([
                format!("{id}.user"),
                format!("{id}.user-privileged"),
                format!("{id}.group"),
            ]);
        }
        for file in files {
            match fs::remove_file(self.path(&file)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("removing {file}: {e}"));
                }
                _ => {}
            }
        }
        sync_dir(&self.dir)?;
        let home = self.homes.join(name);
        match fs::remove_dir_all(&home) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("removing {}: {e}", home.display()))
            }
            _ => Ok(()),
        }
    }

    fn write_privileged(&self, name: &str, hash: &str) -> Result<(), String> {
        let content = Secret::new(format!(
            "{{\"privileged\":{{\"hashedPassword\":[{}]}}}}\n",
            json::string(hash)
        ));
        write(
            &self.path(&format!("{name}.user-privileged")),
            content.expose(),
            0o600,
        )
    }

    /// The lowest ID from [`FIRST_ID`] that no user or group has, so the
    /// person's user and group share it.
    fn free_id(&self) -> Option<u32> {
        (FIRST_ID..=LAST_ID).find(|&id| {
            // SAFETY: only the null-ness of the static results is looked at.
            let taken = unsafe { !getpwuid(id).is_null() || !getgrgid(id).is_null() };
            !taken
                && !self.path(&format!("{id}.user")).exists()
                && !self.path(&format!("{id}.group")).exists()
        })
    }
}

/// Every file of an account is named after it: only a name as
/// [`is_name`] has it stays inside the store (no `/`, no `..`).
fn checked(name: &str) -> Result<(), String> {
    if is_name(name) {
        Ok(())
    } else {
        Err(format!("{name:?} is not an account name"))
    }
}

/// Replaces `path` atomically and durably, with `mode` from the start.
fn write(path: &Path, content: &str, mode: u32) -> Result<(), String> {
    let temporary = path.with_extension("beize-new");
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("replacing {}: {e}", path.display()))
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("syncing {}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (Store, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("beize-accounts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        (Store::new(root.join("userdb"), root.join("home")), root)
    }

    #[test]
    fn keeps_an_account_as_a_systemd_user_record() {
        let (store, root) = store("create");
        fs::create_dir_all(root.join("home")).unwrap();
        let lock = store.lock().unwrap();
        // chown to the new ID needs root; the files are what matters here.
        let created = store.create(&lock, "joe", "Joe \"J\"", "$y$hash", true);
        if unsafe_is_root() {
            created.unwrap();
        }
        let dir = root.join("userdb");
        let record = fs::read_to_string(dir.join("joe.user")).unwrap();
        let record = json::Json::parse(&record, false).unwrap();
        let object = record.object();
        assert_eq!(object.str("userName"), Some("joe"));
        assert_eq!(object.str("realName"), Some("Joe \"J\""));
        let id = object.unsigned("uid").unwrap() as u32;
        assert!((FIRST_ID..=LAST_ID).contains(&id));
        assert_eq!(object.unsigned("gid"), Some(u64::from(id)));
        assert_eq!(store.uid("joe"), Some(id));
        assert_eq!(store.hash("joe").unwrap().expose(), "$y$hash");
        let mode = |f: &str| fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode("joe.user-privileged"),
            0o600,
            "the hash is root's only"
        );
        assert_eq!(mode("joe.user"), 0o644);
        assert_eq!(
            fs::read_link(dir.join(format!("{id}.user"))).unwrap(),
            Path::new("joe.user")
        );
        assert!(dir.join("joe:wheel.membership").exists());
        assert!(store.has("joe"));

        store.set_hash(&lock, "joe", "$y$new").unwrap();
        assert_eq!(store.hash("joe").unwrap().expose(), "$y$new");

        store.remove(&lock, "joe").unwrap();
        assert!(!store.has("joe"));
        assert!(fs::read_dir(&dir).unwrap().all(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".beize-accounts.lock")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    fn unsafe_is_root() -> bool {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        // SAFETY: no arguments, no side effects.
        unsafe { geteuid() == 0 }
    }
}
