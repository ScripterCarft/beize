//! The programs a udev rules file runs, for the initrd selection.

/// Programs a udev rules file runs (`PROGRAM`, `IMPORT{program}`, `RUN`),
/// not builtins and not programs named by a variable at run time
/// (`$env{REMOVE_CMD}`).
pub(super) fn rule_programs(text: &str) -> Vec<String> {
    let mut programs = Vec::new();
    let mut line = String::new();
    for raw in text.lines() {
        if let Some(part) = raw.strip_suffix('\\') {
            line.push_str(part);
            continue;
        }
        line.push_str(raw);
        if !line.trim_start().starts_with('#') {
            for (key, value) in rule_pairs(&line) {
                let runs = matches!(
                    key.as_str(),
                    "PROGRAM" | "IMPORT{program}" | "RUN" | "RUN{program}"
                );
                let program = value.split_whitespace().next();
                if let Some(program) = program.filter(|p| runs && !p.starts_with(['$', '%'])) {
                    programs.push(program.to_owned());
                }
            }
        }
        line.clear();
    }
    programs.sort();
    programs.dedup();
    programs
}

/// The `KEY op "value"` pairs of one udev rule line.
fn rule_pairs(line: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == ',').is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| !"=!+-:".contains(*c) && !c.is_whitespace()) {
            key.push(c);
        }
        if key.is_empty() {
            break;
        }
        while chars
            .next_if(|c| "=!+-:".contains(*c) || c.is_whitespace())
            .is_some()
        {}
        if chars.next() != Some('"') {
            break;
        }
        let mut value = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => value.extend(chars.next()),
                '"' => break,
                _ => value.push(c),
            }
        }
        pairs.push((key, value));
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_programs_of_udev_rules() {
        let rules = r#"
# PROGRAM="commented"
KERNEL=="sd*", IMPORT{program}="ata_id --export $devnode", ENV{X}="1"
SUBSYSTEM=="block", IMPORT{builtin}="blkid", RUN{builtin}+="kmod load"
ACTION=="add", RUN+="/usr/bin/foo \"quoted\"", \
  PROGRAM=="scsi_id -g"
ENV{MODALIAS}=="?*", RUN{program}+="bar"
ENV{REMOVE_CMD}!="", RUN+="$env{REMOVE_CMD}"
"#;
        assert_eq!(
            rule_programs(rules),
            ["/usr/bin/foo", "ata_id", "bar", "scsi_id"]
        );
    }
}
