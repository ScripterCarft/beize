//! The GNU make jobserver all package builds share: make 4.4, ninja 1.13,
//! cargo and rustc take part. It holds one token per job that may run, for
//! every build at once. bz takes a token for each build it starts – the job
//! every make or ninja runs without asking – and the build's tools take more
//! for more jobs. The pool is sized by CPUs and memory
//! ([`schedule::job_count`]), so builds side by side never run more jobs
//! than the machine has room for.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, PoisonError};

use anyhow::{Context, Result};

use crate::paths::Paths;
use crate::{schedule, util};

/// The pool of this bz run, sized by [`schedule::job_count`] when first
/// needed: package builds and the kernel take from the same one.
pub fn shared(paths: &Paths) -> Result<&'static Jobserver> {
    static SHARED: OnceLock<Jobserver> = OnceLock::new();
    static CREATING: Mutex<()> = Mutex::new(());
    let _creating = CREATING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(server) = SHARED.get() {
        return Ok(server);
    }
    let server = Jobserver::new(&paths.work.join("jobserver"), schedule::job_count())?;
    Ok(SHARED.get_or_init(|| server))
}

pub struct Jobserver {
    fifo: File,
    path: PathBuf,
    size: usize,
}

impl Jobserver {
    /// A pool of `size` tokens in a named pipe at `path`: make 4.4's
    /// `--jobserver-auth=fifo:`, the form ninja understands too.
    pub fn new(path: &Path, size: usize) -> Result<Self> {
        if path.symlink_metadata().is_ok() {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
        util::run(Command::new("mkfifo").args(["-m", "600"]).arg(path))?;
        // Open for reading and writing: never blocks, never sees its end.
        let fifo = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        (&fifo).write_all(&vec![b'+'; size])?;
        Ok(Self {
            fifo,
            path: path.to_owned(),
            size,
        })
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// MAKEFLAGS for a build: how many jobs at most and where the tokens are.
    pub fn makeflags(&self) -> String {
        format!(
            "-j{} --jobserver-auth=fifo:{}",
            self.size,
            self.path.display()
        )
    }

    /// Waits for a token; it goes back to the pool when dropped.
    pub fn acquire(&self) -> Result<Token<'_>> {
        let mut byte = [0];
        (&self.fifo)
            .read_exact(&mut byte)
            .context("waiting for a jobserver token")?;
        Ok(Token {
            server: self,
            byte: byte[0],
        })
    }
}

impl Drop for Jobserver {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// One job's place in the pool.
pub struct Token<'a> {
    server: &'a Jobserver,
    byte: u8,
}

impl Drop for Token<'_> {
    fn drop(&mut self) {
        // A token that cannot go back only makes the pool smaller.
        let _ = (&self.server.fifo).write_all(&[self.byte]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_go_back_to_the_pool() {
        let path = std::env::temp_dir().join(format!("bz-jobserver-{}", std::process::id()));
        let server = Jobserver::new(&path, 2).unwrap();
        assert_eq!(
            server.makeflags(),
            format!("-j2 --jobserver-auth=fifo:{}", path.display())
        );
        let first = server.acquire().unwrap();
        let second = server.acquire().unwrap();
        drop(first);
        let third = server.acquire().unwrap();
        drop((second, third));
        // Both tokens are back: a make started now could take them.
        let mut pool = Vec::new();
        for _ in 0..2 {
            pool.push(server.acquire().unwrap());
        }
        drop(pool);
        drop(server);
        assert!(!path.exists());
    }
}
