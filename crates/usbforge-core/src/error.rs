//! Error type shared across the core and platform crates.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A device could not be enumerated, opened, or queried.
    #[error("device error: {0}")]
    Device(String),

    /// The requested operation is not implemented on the current platform yet.
    #[error("not supported on this platform: {0}")]
    Unsupported(String),

    /// The selected image / source could not be parsed or is invalid.
    #[error("invalid image: {0}")]
    Image(String),

    /// A guard tripped: refusing a dangerous or nonsensical operation.
    #[error("refused: {0}")]
    Refused(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn device(msg: impl Into<String>) -> Self {
        Error::Device(msg.into())
    }
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
