//! Error types for ws-core

use crate::ResourceId;
use thiserror::Error;

/// Result type alias using ws-core's Error
pub type Result<T> = std::result::Result<T, Error>;

/// Core error types for ws
#[derive(Error, Debug)]
pub enum Error {
    /// Resource detection failed
    #[error("Failed to detect state of {resource}: {message}")]
    DetectionFailed {
        resource: ResourceId,
        message: String,
    },

    /// Resource application failed
    #[error("Failed to apply {resource}: {message}")]
    ApplyFailed {
        resource: ResourceId,
        message: String,
    },

    /// Missing dependency in graph
    #[error("Resource {resource} depends on {dependency}, which is not in the graph")]
    MissingDependency {
        resource: ResourceId,
        dependency: ResourceId,
    },

    /// Cyclic dependency detected
    #[error("Cyclic dependency detected involving {resource}")]
    CyclicDependency { resource: ResourceId },

    /// Profile not found
    #[error("Profile '{name}' not found. Available profiles: {available:?}")]
    ProfileNotFound {
        name: String,
        available: Vec<String>,
    },

    /// Scope not found
    #[error("Scope '{name}' not found. Available scopes: {available:?}")]
    ScopeNotFound {
        name: String,
        available: Vec<String>,
    },

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
