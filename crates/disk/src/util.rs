use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use walkdir::WalkDir;

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Newest mtime anywhere under `path`, the directory itself included.
/// `None` when nothing readable is there.
pub fn newest_mtime(path: &Path) -> Option<SystemTime> {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .max()
}

/// Whole days since anything under `path` last changed.
pub fn idle_days(path: &Path, now: SystemTime) -> Option<u64> {
    let last = newest_mtime(path)?;
    Some(
        now.duration_since(last)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs()
            / 86_400,
    )
}

/// On-disk size of `path` in bytes via `du -sk`. Returns 0 if missing or `du` fails.
pub fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let output = match Command::new("du").args(["-sk"]).arg(path).output() {
        Ok(out) if out.status.success() => out,
        _ => return 0,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn format_size_picks_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2 MB");
        assert_eq!(
            format_size(3 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "3.5 GB"
        );
    }

    #[test]
    fn dir_size_returns_zero_for_missing_path() {
        assert_eq!(dir_size(Path::new("/this/path/does/not/exist/xyz")), 0);
    }

    #[test]
    fn dir_size_measures_known_files() {
        let dir = tempdir().unwrap();
        // Write 8 KB across two files so `du -sk` rounds to >= 8.
        let big = vec![0u8; 4096];
        fs::write(dir.path().join("a"), &big).unwrap();
        fs::write(dir.path().join("b"), &big).unwrap();
        let size = dir_size(dir.path());
        assert!(size >= 8 * 1024, "expected ≥ 8 KB, got {size}");
    }
}
