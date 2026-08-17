use std::collections::HashSet;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::SystemTime;

use walkdir::WalkDir;

/// Decimal units, so a size here reads the same as it does in Finder, About
/// This Mac, `df -H` and `diskutil`. Dividing by 1024 would be GiB under a GB
/// label, which puts every figure about 7 percent below what macOS says about
/// the same bytes.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    const TB: u64 = 1_000_000_000_000;

    // A tier begins where the tier below it would round up to four digits, so
    // nothing ever prints as "1000 KB" or "1000.0 GB".
    const MB_FLOOR: u64 = 999_500; // 999.5 KB
    const GB_FLOOR: u64 = 999_500_000; // 999.5 MB
    const TB_FLOOR: u64 = 999_950_000_000; // 999.95 GB

    if bytes >= TB_FLOOR {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB_FLOOR {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB_FLOOR {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
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

/// Bytes `meta` actually occupies, counting a hardlinked file only the first
/// time it is seen. `seen` must be shared across everything being summed
/// together or the same inode gets billed twice.
///
/// Block usage, not `len()`: a sparse or APFS-compressed file occupies far
/// less than its apparent length, and `df` counts blocks. Mixing the two
/// definitions is how a scan ends up disagreeing with the volume it scanned.
pub fn allocated(meta: &Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    meta.blocks() * 512
}

/// On-disk size of everything under `path`, in bytes. Symlinks are counted but
/// never followed. Unreadable entries are skipped.
pub fn dir_size(path: &Path) -> u64 {
    let mut seen = HashSet::new();
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| allocated(&m, &mut seen))
        .sum()
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
        assert_eq!(format_size(2_000), "2 KB");
        assert_eq!(format_size(2_048), "2 KB");
        assert_eq!(format_size(2_000_000), "2 MB");
        assert_eq!(format_size(3_500_000_000), "3.5 GB");
        assert_eq!(format_size(3_500_000_000_000), "3.5 TB");
    }

    #[test]
    fn format_size_changes_unit_at_each_thousand() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_000), "1 KB");
        assert_eq!(format_size(999_999), "1 MB");
        assert_eq!(format_size(1_000_000), "1 MB");
        assert_eq!(format_size(999_999_999), "1.0 GB");
        assert_eq!(format_size(1_000_000_000), "1.0 GB");
        assert_eq!(format_size(999_999_999_999), "1.0 TB");
        assert_eq!(format_size(1_000_000_000_000), "1.0 TB");
    }

    #[test]
    fn format_size_never_prints_four_digits_before_the_unit() {
        // The tier below is left the moment its own rounding would reach 1000.
        assert_eq!(format_size(999_499), "999 KB");
        assert_eq!(format_size(999_500), "1 MB");
        assert_eq!(format_size(999_499_999), "999 MB");
        assert_eq!(format_size(999_500_000), "1.0 GB");
        assert_eq!(format_size(999_949_999_999), "999.9 GB");
        assert_eq!(format_size(999_950_000_000), "1.0 TB");
    }

    #[test]
    fn format_size_agrees_with_what_macos_says_about_this_disk() {
        // `diskutil info /System/Volumes/Data` reports this machine's container
        // as "494.4 GB (494384795648 Bytes)", and Finder, About This Mac and
        // `df -H` all agree with it. Dividing by 1024 gives 460.4 GB, which is
        // the number that made the tool look broken. Do not restore it.
        assert_eq!(format_size(494_384_795_648), "494.4 GB");
        assert_eq!(format_size(63_222_611_968), "63.2 GB");
    }

    #[test]
    fn dir_size_returns_zero_for_missing_path() {
        assert_eq!(dir_size(Path::new("/this/path/does/not/exist/xyz")), 0);
    }

    #[test]
    fn dir_size_counts_a_hardlinked_file_once() {
        // Two names, one inode. `df` charges the volume for one, so this must
        // too, or every category holding a linked file overstates itself.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("original"), vec![0u8; 64 * 1024]).unwrap();
        fs::hard_link(dir.path().join("original"), dir.path().join("clone")).unwrap();

        let size = dir_size(dir.path());
        assert!(
            (64 * 1024..96 * 1024).contains(&size),
            "hardlink counted twice: {size}"
        );
    }

    #[test]
    fn dir_size_reports_blocks_not_apparent_length() {
        // A sparse file's length is a promise, not an allocation.
        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse.bin");
        let file = fs::File::create(&path).unwrap();
        file.set_len(512 * 1024 * 1024).unwrap();
        drop(file);

        let size = dir_size(dir.path());
        assert!(
            size < 1024 * 1024,
            "apparent length used instead of blocks: {size}"
        );
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
