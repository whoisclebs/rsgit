//! Error and result types shared across the application.

use std::fmt::{Display, Formatter};

/// Application-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur while bootstrapping or serving rsgit.
#[derive(Debug)]
pub enum Error {
    /// Wrapper around standard I/O errors.
    Io(std::io::Error),
    /// Invalid runtime configuration.
    Config(String),
    /// A Git subprocess failed, timed out, or produced unsafe output.
    Git(String),
    /// A spawned helper thread failed to join.
    Thread(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Git(msg) => write!(f, "git error: {msg}"),
            Self::Thread(msg) => write!(f, "thread error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
