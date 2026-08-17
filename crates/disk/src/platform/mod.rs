use std::path::PathBuf;

use crate::cleanup::Target;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub fn audit_categories() -> Vec<(&'static str, Vec<(&'static str, PathBuf)>)> {
    macos::audit_categories()
}

#[cfg(not(target_os = "macos"))]
pub fn audit_categories() -> Vec<(&'static str, Vec<(&'static str, PathBuf)>)> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn cleanable_paths() -> Vec<(&'static str, &'static str, PathBuf)> {
    macos::cleanable_paths()
}

#[cfg(not(target_os = "macos"))]
pub fn cleanable_paths() -> Vec<(&'static str, &'static str, PathBuf)> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn cleanup_targets() -> Vec<Target> {
    macos::cleanup_targets()
}

#[cfg(not(target_os = "macos"))]
pub fn cleanup_targets() -> Vec<Target> {
    Vec::new()
}
