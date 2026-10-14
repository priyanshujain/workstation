//! Error types for ws-core

use thiserror::Error;

/// Result type alias using ws-core's Error
pub type Result<T> = std::result::Result<T, Error>;

/// Core error types for ws
#[derive(Error, Debug)]
pub enum Error {
    /// Command execution failed
    #[error("Command failed: {command}\n{stderr}")]
    CommandFailed { command: String, stderr: String },

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic error wrapper
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}
