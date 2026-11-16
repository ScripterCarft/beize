//! `--polkit-helper COOKIE`: polkit's helper from an agent's side, for the
//! counter-check of ADR 0031. It connects to the helper's socket as an agent
//! does, names the user (`$USER`) and COOKIE, answers a password prompt with
//! the line on its standard input, and prints the helper's verdict:
//! `helper: SUCCESS` or `helper: FAILURE`. With a cookie polkitd never gave
//! out, even the right password must get FAILURE. Nothing it reads or writes
//! besides the verdict is printed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use super::Result;

const HELPER: &str = "/run/polkit/agent-helper.socket";

pub fn run(cookie: &str) -> Result<()> {
    let user = std::env::var("USER").map_err(|_| "USER is not set")?;
    let mut password = String::new();
    std::io::stdin()
        .read_line(&mut password)
        .map_err(|e| format!("stdin: {e}"))?;
    let password = password.trim_end_matches('\n');
    let mut stream = UnixStream::connect(HELPER).map_err(|e| format!("{HELPER}: {e}"))?;
    stream
        .write_all(format!("{user}\n{cookie}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            println!("helper: closed");
            return Ok(());
        }
        let line = line.trim_end();
        if line.starts_with("PAM_PROMPT_ECHO_OFF") {
            stream
                .write_all(format!("{password}\n").as_bytes())
                .map_err(|e| e.to_string())?;
        } else if line.starts_with("SUCCESS") || line.starts_with("FAILURE") {
            println!("helper: {line}");
            return Ok(());
        }
    }
}
