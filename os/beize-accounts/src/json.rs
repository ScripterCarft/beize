//! The JSON homed takes: user records and secrets
//! (https://systemd.io/USER_RECORD/). Only what Beize sets, built by hand –
//! no JSON library for a handful of strings.

use crate::NewAccount;
use crate::secret::Secret;

/// `text` as a JSON string, quotes included.
pub fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn list(items: &[&str]) -> String {
    let items: Vec<String> = items.iter().map(|item| string(item)).collect();
    format!("[{}]", items.join(","))
}

/// The secret section: the plain passwords (and recovery keys) that prove
/// who acts, or set a new password.
pub fn secret(passwords: &[&str]) -> Secret {
    Secret::new(format!("{{\"password\":{}}}", list(passwords)))
}

/// The user record for a new account, its secrets included: homed checks
/// that every hashed password and recovery key has its plain text there.
/// Stored in a directory on the encrypted root partition (ADR 0019).
pub fn new_account(account: &NewAccount, password_hash: &str, recovery_key_hash: &str) -> Secret {
    let mut fields = vec![
        format!("\"userName\":{}", string(account.name)),
        "\"storage\":\"directory\"".to_owned(),
        "\"shell\":\"/usr/bin/bash\"".to_owned(),
        "\"recoveryKeyType\":[\"modhex64\"]".to_owned(),
    ];
    if !account.real_name.is_empty() {
        fields.push(format!("\"realName\":{}", string(account.real_name)));
    }
    if account.administrator {
        fields.push("\"memberOf\":[\"wheel\"]".to_owned());
    }
    fields.push(format!(
        "\"privileged\":{{\"hashedPassword\":{},\"recoveryKey\":[{{\"type\":\"modhex64\",\
         \"hashedPassword\":{}}}]}}",
        list(&[password_hash]),
        string(recovery_key_hash)
    ));
    fields.push(format!(
        "\"secret\":{}",
        secret(&[account.password, account.recovery_key]).expose()
    ));
    Secret::new(format!("{{{}}}", fields.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_strings() {
        assert_eq!(string("joe"), "\"joe\"");
        assert_eq!(string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\u000ad\"");
        assert_eq!(string("Jörg"), "\"Jörg\"");
    }

    #[test]
    fn builds_the_record_of_a_new_account() {
        let account = NewAccount {
            name: "joe",
            real_name: "Joe \"J\"",
            password: "pass word",
            recovery_key: "aaaa-bbbb",
            administrator: true,
        };
        let record = new_account(&account, "$y$p", "$y$k");
        assert_eq!(
            record.expose(),
            "{\"userName\":\"joe\",\"storage\":\"directory\",\"shell\":\"/usr/bin/bash\",\
             \"recoveryKeyType\":[\"modhex64\"],\"realName\":\"Joe \\\"J\\\"\",\
             \"memberOf\":[\"wheel\"],\"privileged\":{\"hashedPassword\":[\"$y$p\"],\
             \"recoveryKey\":[{\"type\":\"modhex64\",\"hashedPassword\":\"$y$k\"}]},\
             \"secret\":{\"password\":[\"pass word\",\"aaaa-bbbb\"]}}"
        );
        let plain = NewAccount {
            real_name: "",
            administrator: false,
            ..account
        };
        let record = new_account(&plain, "$y$p", "$y$k");
        assert!(!record.expose().contains("realName"));
        assert!(!record.expose().contains("memberOf"));
    }
}
