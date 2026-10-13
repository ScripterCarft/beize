//! `beize-accountsd` – Beize's account service (ADR 0019), the one authority
//! over people's accounts, like opendirectoryd on macOS. One process per
//! connection (beize-accounts.socket, Accept=yes): systemd passes the
//! connection as stdin; the kernel says who is at the other end
//! (SO_PEERCRED), so no client can claim to be someone else.
//!
//! Who may do what – every change proven, never just asked for:
//! - change your own password: with the current one;
//! - create an account, reset another's password: an administrator (wheel)
//!   with their own password, as on macOS;
//! - root (the setup, the recovery – reachable only with an administrator's
//!   password or the recovery key): all of it, without a password;
//! - remove an account: root only.
//!
//! A wrong password costs 2 s, as at the login (pam_faildelay): the service
//! is no faster way to guess.

mod store;

use std::ffi::{CStr, c_char, c_int};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use beize_accounts::json::{self, Json, Object};
use beize_accounts::protocol::{self, error};
use beize_accounts::{crypt, validate_name, validate_password};

use store::Store;

const USERDB: &str = "/etc/userdb";
const HOMES: &str = "/home";
const ADMIN_GROUP: &str = "wheel";
/// After a wrong password.
#[cfg(not(test))]
const FAIL_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const FAIL_DELAY: Duration = Duration::ZERO;

fn main() -> ExitCode {
    // SAFETY: systemd passes the accepted connection as stdin
    // (StandardInput=socket); nothing else uses fd 0.
    let mut stream = unsafe { UnixStream::from_raw_fd(0) };
    let caller = match Caller::of(&stream) {
        Ok(caller) => caller,
        Err(e) => {
            eprintln!("beize-accountsd: who is calling? {e}");
            return ExitCode::FAILURE;
        }
    };
    let store = Store::new(USERDB, HOMES);
    loop {
        match protocol::read_message(&mut stream) {
            Ok(Some(request)) => {
                let reply = handle(&store, &caller, request.expose());
                if protocol::write_message(&mut stream, &reply).is_err() {
                    return ExitCode::FAILURE;
                }
            }
            Ok(None) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("beize-accountsd: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
}

/// Who is at the other end of the connection.
struct Caller {
    uid: u32,
    /// The login name, if the caller has an account.
    name: Option<String>,
    administrator: bool,
}

/// Why a request was refused: the error and, for some, the reason.
struct Refused(&'static str, String);

fn refused(error: &'static str) -> Refused {
    Refused(error, String::new())
}

fn handle(store: &Store, caller: &Caller, request: &str) -> String {
    let Ok(request) = Json::parse(request, true) else {
        return reply_error("org.varlink.service.InvalidParameter", "");
    };
    let object = request.object();
    let method = beize_varlink::method(object, protocol::INTERFACE);
    let Some(params) = object.object("parameters") else {
        return reply_error("org.varlink.service.InvalidParameter", "parameters");
    };
    let result = match method {
        Some("Create") => create(store, caller, params),
        Some("ChangePassword") => change_password(store, caller, params),
        Some("ResetPassword") => reset_password(store, caller, params),
        Some("Remove") => remove(store, caller, params),
        _ => return reply_error("org.varlink.service.MethodNotFound", ""),
    };
    match result {
        Ok(()) => "{\"parameters\":{}}".to_owned(),
        Err(Refused(name, reason)) => {
            reply_error(&format!("{}.{name}", protocol::INTERFACE), &reason)
        }
    }
}

fn reply_error(error: &str, reason: &str) -> String {
    format!(
        "{{\"error\":{},\"parameters\":{{\"reason\":{}}}}}",
        json::string(error),
        json::string(reason)
    )
}

fn string<'a>(params: Object<'a>, key: &str) -> Result<&'a str, Refused> {
    params
        .str(key)
        .ok_or_else(|| Refused(error::INVALID, format!("{key} is missing")))
}

fn create(store: &Store, caller: &Caller, params: Object) -> Result<(), Refused> {
    let name = string(params, "name")?;
    let password = string(params, "password")?;
    let real_name = params.str("realName").unwrap_or("");
    let administrator = params.bool("administrator").unwrap_or(false);
    authorize_administrator(store, caller, params.str("auth"))?;
    validate_name(name).map_err(|reason| Refused(error::INVALID, reason))?;
    validate_password(password).map_err(|reason| Refused(error::INVALID, reason))?;
    if beize_accounts::exists(name) || store.has(name) {
        return Err(refused(error::ACCOUNT_EXISTS));
    }
    let hash = crypt::hash(password).map_err(|e| Refused(error::FAILED, e))?;
    let lock = store.lock().map_err(|e| Refused(error::FAILED, e))?;
    let uid = store
        .create(&lock, name, real_name, &hash, administrator)
        .map_err(|e| Refused(error::FAILED, e))?;
    eprintln!(
        "beize-accountsd: created {name} (uid {uid}{}) for uid {}",
        if administrator { ", administrator" } else { "" },
        caller.uid
    );
    Ok(())
}

fn change_password(store: &Store, caller: &Caller, params: Object) -> Result<(), Refused> {
    let name = string(params, "name")?;
    let current = string(params, "current")?;
    let new = string(params, "new")?;
    if !store.has(name) {
        return Err(refused(error::NO_SUCH_ACCOUNT));
    }
    if caller.uid != 0 && store.uid(name) != Some(caller.uid) {
        return Err(refused(error::NOT_ALLOWED));
    }
    prove(store, name, current)?;
    set_password(store, name, new)?;
    eprintln!("beize-accountsd: {name} changed their password");
    Ok(())
}

fn reset_password(store: &Store, caller: &Caller, params: Object) -> Result<(), Refused> {
    let name = string(params, "name")?;
    let new = string(params, "new")?;
    authorize_administrator(store, caller, params.str("auth"))?;
    if !store.has(name) {
        return Err(refused(error::NO_SUCH_ACCOUNT));
    }
    set_password(store, name, new)?;
    eprintln!(
        "beize-accountsd: password of {name} reset by uid {}",
        caller.uid
    );
    Ok(())
}

fn remove(store: &Store, caller: &Caller, params: Object) -> Result<(), Refused> {
    let name = string(params, "name")?;
    if caller.uid != 0 {
        return Err(refused(error::NOT_ALLOWED));
    }
    if !store.has(name) {
        return Err(refused(error::NO_SUCH_ACCOUNT));
    }
    let lock = store.lock().map_err(|e| Refused(error::FAILED, e))?;
    store
        .remove(&lock, name)
        .map_err(|e| Refused(error::FAILED, e))?;
    eprintln!("beize-accountsd: removed {name}");
    Ok(())
}

fn set_password(store: &Store, name: &str, new: &str) -> Result<(), Refused> {
    validate_password(new).map_err(|reason| Refused(error::INVALID, reason))?;
    let hash = crypt::hash(new).map_err(|e| Refused(error::FAILED, e))?;
    let lock = store.lock().map_err(|e| Refused(error::FAILED, e))?;
    store
        .set_hash(&lock, name, &hash)
        .map_err(|e| Refused(error::FAILED, e))
}

/// Root may; an administrator with their own password may.
fn authorize_administrator(
    store: &Store,
    caller: &Caller,
    auth: Option<&str>,
) -> Result<(), Refused> {
    if caller.uid == 0 {
        return Ok(());
    }
    let (Some(name), true) = (&caller.name, caller.administrator) else {
        return Err(refused(error::NOT_ALLOWED));
    };
    prove(store, name, auth.unwrap_or(""))
}

/// `password` is `name`'s; otherwise the fail delay and BadPassword.
fn prove(store: &Store, name: &str, password: &str) -> Result<(), Refused> {
    let proven = store
        .hash(name)
        .is_some_and(|hash| crypt::verify(password, hash.expose()));
    if !proven {
        eprintln!("beize-accountsd: wrong password for {name}");
        thread::sleep(FAIL_DELAY);
        return Err(refused(error::BAD_PASSWORD));
    }
    Ok(())
}

// The caller, from the kernel and NSS.

#[repr(C)]
pub(crate) struct Passwd {
    name: *const c_char,
    passwd: *const c_char,
    uid: u32,
    gid: u32,
}

#[repr(C)]
struct Group {
    name: *const c_char,
    passwd: *const c_char,
    gid: u32,
}

unsafe extern "C" {
    fn getpwuid(uid: u32) -> *const Passwd;
    fn getgrnam(name: *const c_char) -> *const Group;
    fn getgrouplist(user: *const c_char, group: u32, groups: *mut u32, count: *mut c_int) -> c_int;
}

impl Caller {
    fn of(stream: &UnixStream) -> Result<Self, String> {
        let uid = beize_varlink::peer_uid(stream).map_err(|e| e.to_string())?;
        // SAFETY: getpwuid returns a static record or null; its name and
        // primary group are copied at once.
        let (name, gid) = unsafe {
            let record = getpwuid(uid);
            if record.is_null() || (*record).name.is_null() {
                (None, 0)
            } else {
                let name = CStr::from_ptr((*record).name)
                    .to_string_lossy()
                    .into_owned();
                (Some(name), (*record).gid)
            }
        };
        let administrator = name
            .as_deref()
            .is_some_and(|name| in_group(name, gid, ADMIN_GROUP));
        Ok(Self {
            uid,
            name,
            administrator,
        })
    }
}

/// Whether user `name` (primary group `gid`) is in group `group`.
fn in_group(name: &str, gid: u32, group: &str) -> bool {
    let (Ok(name), Ok(group)) = (std::ffi::CString::new(name), std::ffi::CString::new(group))
    else {
        return false;
    };
    // SAFETY: NUL-terminated name; the group ID is copied at once.
    let wanted = unsafe {
        let record = getgrnam(group.as_ptr());
        if record.is_null() {
            return false;
        }
        (*record).gid
    };
    let mut groups = vec![0u32; 256];
    let mut count = groups.len() as c_int;
    // SAFETY: a buffer of `count` group IDs; getgrouplist writes at most that
    // many and returns -1 if they do not fit.
    let r = unsafe { getgrouplist(name.as_ptr(), gid, groups.as_mut_ptr(), &mut count) };
    if r < 0 {
        return false;
    }
    groups.truncate(count.max(0) as usize);
    groups.contains(&wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a temporary directory with the accounts "joe" and
    /// "adm"; returns their user IDs. Creating chowns the home, which needs
    /// root: as a user the files are written all the same.
    fn store(test: &str) -> (Store, u32, u32, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("beize-accountsd-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        let store = Store::new(root.join("userdb"), root.join("home"));
        let lock = store.lock().unwrap();
        for (name, admin) in [("joe", false), ("adm", true)] {
            let hash = crypt::hash(&format!("{name}-password")).unwrap();
            let _ = store.create(&lock, name, "", &hash, admin);
        }
        drop(lock);
        let (joe, adm) = (store.uid("joe").unwrap(), store.uid("adm").unwrap());
        (store, joe, adm, root)
    }

    fn call(store: &Store, caller: &Caller, method: &str, params: &str) -> String {
        handle(
            store,
            caller,
            &format!("{{\"method\":\"io.beize.Accounts.{method}\",\"parameters\":{{{params}}}}}"),
        )
    }

    fn is_error(reply: &str, error: &str) -> bool {
        reply.contains(&format!("\"io.beize.Accounts.{error}\""))
    }

    const OK: &str = "{\"parameters\":{}}";

    #[test]
    fn only_proven_changes_go_through() {
        let (store, joe_uid, adm_uid, root) = store("rules");
        let joe = Caller {
            uid: joe_uid,
            name: Some("joe".into()),
            administrator: false,
        };
        let adm = Caller {
            uid: adm_uid,
            name: Some("adm".into()),
            administrator: true,
        };
        let root_caller = Caller {
            uid: 0,
            name: Some("root".into()),
            administrator: false,
        };

        // Your own password: only with the current one, never someone else's.
        let change = "\"name\":\"joe\",\"current\":\"wrong-password\",\"new\":\"joe-new-password\"";
        assert!(is_error(
            &call(&store, &joe, "ChangePassword", change),
            error::BAD_PASSWORD
        ));
        let change = "\"name\":\"adm\",\"current\":\"adm-password\",\"new\":\"stolen-password\"";
        assert!(is_error(
            &call(&store, &joe, "ChangePassword", change),
            error::NOT_ALLOWED
        ));
        let change = "\"name\":\"joe\",\"current\":\"joe-password\",\"new\":\"short\"";
        assert!(is_error(
            &call(&store, &joe, "ChangePassword", change),
            error::INVALID
        ));
        let change = "\"name\":\"joe\",\"current\":\"joe-password\",\"new\":\"joe-new-password\"";
        assert_eq!(call(&store, &joe, "ChangePassword", change), OK);
        assert!(crypt::verify(
            "joe-new-password",
            store.hash("joe").unwrap().expose()
        ));

        // Someone else's password: an administrator with their own password.
        let reset =
            "\"name\":\"adm\",\"new\":\"taken-over-password\",\"auth\":\"joe-new-password\"";
        assert!(is_error(
            &call(&store, &joe, "ResetPassword", reset),
            error::NOT_ALLOWED
        ));
        let reset = "\"name\":\"joe\",\"new\":\"reset-password\",\"auth\":\"wrong-password\"";
        assert!(is_error(
            &call(&store, &adm, "ResetPassword", reset),
            error::BAD_PASSWORD
        ));
        let reset = "\"name\":\"joe\",\"new\":\"reset-password\"";
        assert!(is_error(
            &call(&store, &adm, "ResetPassword", reset),
            error::BAD_PASSWORD
        ));
        let reset = "\"name\":\"joe\",\"new\":\"reset-password\",\"auth\":\"adm-password\"";
        assert_eq!(call(&store, &adm, "ResetPassword", reset), OK);
        assert!(crypt::verify(
            "reset-password",
            store.hash("joe").unwrap().expose()
        ));
        // root (setup, recovery) needs no password; accounts not Beize's stay untouched.
        let reset = "\"name\":\"joe\",\"new\":\"recovered-password\"";
        assert_eq!(call(&store, &root_caller, "ResetPassword", reset), OK);
        let reset = "\"name\":\"root\",\"new\":\"root-password\"";
        assert!(is_error(
            &call(&store, &root_caller, "ResetPassword", reset),
            error::NO_SUCH_ACCOUNT
        ));

        // Creating: administrators with their password; removing: root only.
        let create = "\"name\":\"eve\",\"realName\":\"\",\"password\":\"eve-password\",\"administrator\":true";
        assert!(is_error(
            &call(&store, &joe, "Create", create),
            error::NOT_ALLOWED
        ));
        assert!(is_error(
            &call(&store, &adm, "Create", create),
            error::BAD_PASSWORD
        ));
        let create = "\"name\":\"joe\",\"realName\":\"\",\"password\":\"joe-password\",\"auth\":\"adm-password\"";
        assert!(is_error(
            &call(&store, &adm, "Create", create),
            error::ACCOUNT_EXISTS
        ));
        assert!(is_error(
            &call(&store, &adm, "Remove", "\"name\":\"joe\""),
            error::NOT_ALLOWED
        ));

        // Nonsense is refused, not acted on.
        assert!(call(&store, &root_caller, "Delete", "").contains("MethodNotFound"));
        assert!(handle(&store, &root_caller, "not json").contains("InvalidParameter"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
