use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub mod macos;

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
