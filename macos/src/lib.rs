//! ws-macos: macOS-specific resources for declarative workstation configuration
//!
//! This crate provides macOS-specific resource implementations:
//! - `packages`: Homebrew formula and cask management
//! - `settings`: macOS defaults (future)
//! - `services`: launchd agents (future)

pub mod packages;

pub use packages::brew::{BrewCask, BrewFormula};
