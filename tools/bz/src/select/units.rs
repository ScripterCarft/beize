//! What the initrd selection reads from unit files: settings as systemd
//! applies them, the programs of `Exec*=` lines, templates and the units a
//! socket, path or timer starts.

/// The template of an instance name: `foo@bar.service` → `foo@.service`.
pub(super) fn template(name: &str) -> Option<String> {
    let (prefix, rest) = name.split_once('@')?;
    let (instance, suffix) = rest.rsplit_once('.')?;
    (!instance.is_empty()).then(|| format!("{prefix}@.{suffix}"))
}

/// Units a socket, path or timer unit starts.
pub(super) fn triggered(name: &str, settings: &[(String, String)]) -> Vec<String> {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return Vec::new();
    };
    let setting = |key: &str| {
        settings
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    match kind {
        "socket" => {
            if let Some(service) = setting("Service") {
                vec![service]
            } else if setting("Accept").is_some_and(|v| v == "yes" || v == "true") {
                vec![format!("{stem}@.service")]
            } else {
                vec![format!("{stem}.service")]
            }
        }
        "path" | "timer" => vec![setting("Unit").unwrap_or_else(|| format!("{stem}.service"))],
        _ => Vec::new(),
    }
}

/// The `key=value` lines of a unit file, continuation lines joined.
pub(super) fn unit_settings(text: &str) -> Vec<(String, String)> {
    let mut settings = Vec::new();
    let mut line = String::new();
    for raw in text.lines() {
        let raw = raw.trim();
        if line.is_empty() && (raw.starts_with('#') || raw.starts_with(';')) {
            continue;
        }
        if let Some(part) = raw.strip_suffix('\\') {
            line.push_str(part);
            line.push(' ');
            continue;
        }
        line.push_str(raw);
        if let Some((key, value)) = line.split_once('=') {
            settings.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        line.clear();
    }
    settings
}

/// Settings of a unit file and its drop-ins, in order, as systemd applies
/// them: an empty value (`ExecStart=`) drops what was set before.
pub(super) fn effective(settings: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut kept: Vec<(String, String)> = Vec::new();
    for (key, value) in settings {
        if value.is_empty() {
            kept.retain(|(k, _)| *k != key);
        } else {
            kept.push((key, value));
        }
    }
    kept
}

/// The program of an `Exec*=` value and whether a failure is ignored (`-`).
pub(super) fn exec_program(value: &str) -> Option<(bool, String)> {
    let mut optional = false;
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        match c {
            '-' => optional = true,
            '@' | ':' | '+' | '!' | '|' => {}
            _ => break,
        }
        rest = &rest[1..];
    }
    let program = rest.split_whitespace().next()?;
    Some((optional, program.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_unit_settings() {
        let text = "[Unit]\n# Requires=commented.service\nRequires=a.service \\\n  b.service\n\
                    [Service]\nExecStart=-/usr/bin/foo --bar\nExecStart=\n";
        assert_eq!(
            unit_settings(text),
            [
                ("Requires".to_owned(), "a.service  b.service".to_owned()),
                ("ExecStart".to_owned(), "-/usr/bin/foo --bar".to_owned()),
                ("ExecStart".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn an_empty_value_resets_a_setting() {
        let unit = unit_settings(
            "ExecStartPre=-plymouth\nExecStart=-/usr/lib/systemd/systemd-sulogin-shell\n",
        );
        let dropin = unit_settings(
            "ExecStartPre=\nExecStart=\nExecStart=/usr/lib/beize/beize-tpm emergency\n",
        );
        assert_eq!(
            effective([unit, dropin].concat()),
            [(
                "ExecStart".to_owned(),
                "/usr/lib/beize/beize-tpm emergency".to_owned()
            )]
        );
    }

    #[test]
    fn finds_the_program_of_exec_lines() {
        assert_eq!(
            exec_program("-/usr/lib/systemd/systemd-sulogin-shell emergency"),
            Some((true, "/usr/lib/systemd/systemd-sulogin-shell".to_owned()))
        );
        assert_eq!(
            exec_program("!!@/usr/bin/foo foo"),
            Some((false, "/usr/bin/foo".to_owned()))
        );
        assert_eq!(
            exec_program("udevadm info --cleanup-db"),
            Some((false, "udevadm".to_owned()))
        );
        assert_eq!(exec_program(""), None);
    }

    #[test]
    fn knows_templates_and_triggered_units() {
        assert_eq!(
            template("systemd-cryptsetup@root.service").as_deref(),
            Some("systemd-cryptsetup@.service")
        );
        assert_eq!(template("systemd-pcrextend@.service"), None);
        assert_eq!(template("initrd.target"), None);
        let accept = vec![("Accept".to_owned(), "yes".to_owned())];
        assert_eq!(
            triggered("systemd-pcrextend.socket", &accept),
            ["systemd-pcrextend@.service"]
        );
        assert_eq!(
            triggered("systemd-journald.socket", &[]),
            ["systemd-journald.service"]
        );
        let service = vec![("Service".to_owned(), "x.service".to_owned())];
        assert_eq!(triggered("y.socket", &service), ["x.service"]);
        assert_eq!(
            triggered("systemd-ask-password-console.path", &[]),
            ["systemd-ask-password-console.service"]
        );
        assert!(triggered("initrd.target", &[]).is_empty());
    }
}
