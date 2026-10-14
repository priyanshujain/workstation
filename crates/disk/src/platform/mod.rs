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
pub fn cleanup_targets(closed: &[PathBuf]) -> Vec<Target> {
    macos::cleanup_targets(closed)
}

#[cfg(not(target_os = "macos"))]
pub fn cleanup_targets(_closed: &[PathBuf]) -> Vec<Target> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn closed_paths() -> Vec<PathBuf> {
    macos::closed_paths()
}

#[cfg(not(target_os = "macos"))]
pub fn closed_paths() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn has_full_disk_access() -> bool {
    macos::has_full_disk_access()
}

#[cfg(not(target_os = "macos"))]
pub fn has_full_disk_access() -> bool {
    true
}
