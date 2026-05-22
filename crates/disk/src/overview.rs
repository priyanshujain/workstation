use std::path::Path;
use std::process::Command;

pub struct DiskOverview {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

impl DiskOverview {
    pub fn usage_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used as f64 / self.total as f64) * 100.0
    }
}

/// Get disk overview via `df`. On macOS, prefers `/System/Volumes/Data` (the APFS
/// data volume) for accurate usage; elsewhere falls back to `/`.
pub fn disk_overview() -> Option<DiskOverview> {
    let path = df_path();
    let output = Command::new("df").args(["-k", path]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().nth(1)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let total = parts[1].parse::<u64>().ok()? * 1024;
    let used = parts[2].parse::<u64>().ok()? * 1024;
    let free = parts[3].parse::<u64>().ok()? * 1024;
    Some(DiskOverview { total, used, free })
}

#[cfg(target_os = "macos")]
fn df_path() -> &'static str {
    if Path::new("/System/Volumes/Data").exists() {
        "/System/Volumes/Data"
    } else {
        "/"
    }
}

#[cfg(not(target_os = "macos"))]
fn df_path() -> &'static str {
    let _ = Path::new("/");
    "/"
}
