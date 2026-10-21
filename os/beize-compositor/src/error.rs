//! One error type: what failed, and why, for the journal.

use std::fmt::{self, Display};

#[derive(Debug)]
pub struct Error(String);

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn error(message: impl Display) -> Error {
    Error(message.to_string())
}

/// Names what was being done when an error happened.
pub trait Context<T> {
    fn context(self, what: impl Display) -> Result<T>;
}

impl<T, E: Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: impl Display) -> Result<T> {
        self.map_err(|e| Error(format!("{what}: {e}")))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, what: impl Display) -> Result<T> {
        self.ok_or_else(|| Error(what.to_string()))
    }
}
