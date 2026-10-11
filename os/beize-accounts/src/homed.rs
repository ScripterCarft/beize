//! The module for systemd-homed: its D-Bus API (org.freedesktop.home1). homed
//! checks with polkit itself whether the caller may act – a person may
//! always change their own password – so this module needs no privileges of
//! its own.
//!
//! homed never overwrites a password without proof, not even for root: the
//! current password or a recovery key stored in the account. That is why
//! every account gets the computer's recovery key (ADR 0019).

use crate::bus::{Bus, CallError};
use crate::{Accounts, NewAccount, crypt, json};

const DESTINATION: &str = "org.freedesktop.home1";
const PATH: &str = "/org/freedesktop/home1";
const MANAGER: &str = "org.freedesktop.home1.Manager";

pub struct Homed {
    bus: Bus,
}

impl Homed {
    pub fn connect() -> Result<Self, String> {
        Ok(Self {
            bus: Bus::system()?,
        })
    }

    fn call(&self, member: &str, args: &[&str], sensitive: bool) -> Result<(), CallError> {
        self.bus
            .call(DESTINATION, PATH, MANAGER, member, args, sensitive)
    }
}

/// homed's error as a sentence for the person; `proof` names what was given
/// as proof (for a wrong one).
fn explain(error: CallError, name: &str, proof: &str) -> String {
    match error.name.rsplit('.').next().unwrap_or_default() {
        "BadPassword" | "BadRecoveryKey" | "BadPasswordAndNoToken" => {
            format!("That is not the {proof}.")
        }
        "NoSuchHome" => format!("There is no account \"{name}\"."),
        "UserNameExists" | "HomeExists" => format!("\"{name}\" exists already."),
        "AccessDenied" | "InteractiveAuthorizationRequired" => {
            "You are not allowed to change this account.".to_owned()
        }
        _ => format!("The account service failed: {}", error.message),
    }
}

impl Accounts for Homed {
    fn create(&self, account: &NewAccount) -> Result<(), String> {
        let password_hash = crypt::hash(account.password)?;
        let key_hash = crypt::hash(account.recovery_key)?;
        let record = json::new_account(account, &password_hash, &key_hash);
        self.call("CreateHome", &[record.expose()], true)
            .map_err(|e| explain(e, account.name, "password"))
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.call("RemoveHome", &[name], false)
            .map_err(|e| explain(e, name, "password"))
    }

    fn change_password(&self, name: &str, current: &str, new: &str) -> Result<(), String> {
        let (new, current) = (json::secret(&[new]), json::secret(&[current]));
        self.call(
            "ChangePasswordHome",
            &[name, new.expose(), current.expose()],
            true,
        )
        .map_err(|e| explain(e, name, "current password"))
    }

    fn reset_password(&self, name: &str, recovery_key: &str, new: &str) -> Result<(), String> {
        let (new, key) = (json::secret(&[new]), json::secret(&[recovery_key]));
        self.call(
            "ChangePasswordHome",
            &[name, new.expose(), key.expose()],
            true,
        )
        .map_err(|e| explain(e, name, "recovery key of this account"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(name: &str) -> CallError {
        CallError {
            name: name.to_owned(),
            message: "details".to_owned(),
        }
    }

    #[test]
    fn explains_homeds_errors() {
        assert_eq!(
            explain(
                error("org.freedesktop.home1.BadPassword"),
                "joe",
                "current password"
            ),
            "That is not the current password."
        );
        assert_eq!(
            explain(error("org.freedesktop.home1.NoSuchHome"), "joe", "x"),
            "There is no account \"joe\"."
        );
        assert_eq!(
            explain(error("org.freedesktop.Other"), "joe", "x"),
            "The account service failed: details"
        );
    }
}
