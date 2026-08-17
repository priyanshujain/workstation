use std::path::{Path, PathBuf};

use crate::util::dir_size;

pub struct ScanResult {
    pub path: PathBuf,
    pub total_size: u64,
    pub children: Vec<ChildEntry>,
}

pub struct ChildEntry {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
}

/// Scan `dir` one level deep, computing the recursive size of each immediate child.
/// Children sorted by size descending. Symlinks not followed. Permission errors
/// silently skipped (matches existing `dir_size` behavior).
pub fn scan_dir(dir: &Path) -> ScanResult {
    let path = dir.to_path_buf();
    if !dir.exists() || !dir.is_dir() {
        return ScanResult {
            path,
            total_size: 0,
            children: Vec::new(),
        };
    }

    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => {
            return ScanResult {
                path,
                total_size: 0,
                children: Vec::new(),
            };
        }
    };

    let mut children: Vec<ChildEntry> = Vec::new();
    for entry in read.flatten() {
        let child_path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_symlink() {
            continue;
        }

        let is_dir = file_type.is_dir();
        let size = dir_size(&child_path);
        children.push(ChildEntry {
            name,
            path: child_path,
            size,
            is_dir,
        });
    }

    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    let total_size = children.iter().map(|c| c.size).sum();

    ScanResult {
        path,
        total_size,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_dir_returns_empty_for_missing_path() {
        let result = scan_dir(Path::new("/this/path/does/not/exist/xyz"));
        assert_eq!(result.total_size, 0);
        assert!(result.children.is_empty());
    }

    #[test]
    fn scan_dir_lists_immediate_children_only() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create top-level file
        fs::write(root.join("top.txt"), vec![0u8; 100]).unwrap();

        // Create nested directory with a file deep inside
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        let deep = nested.join("deep");
        fs::create_dir(&deep).unwrap();
        fs::write(deep.join("buried.bin"), vec![0u8; 500]).unwrap();

        let result = scan_dir(root);

        // Only 2 immediate children: the file and the nested directory
        assert_eq!(result.children.len(), 2);

        // 'deep' itself must not appear at the top level
        let names: Vec<&str> = result.children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"top.txt"));
        assert!(names.contains(&"nested"));
        assert!(!names.contains(&"deep"));
        assert!(!names.contains(&"buried.bin"));

        // But the buried bytes count against 'nested'
        let nested_entry = result.children.iter().find(|c| c.name == "nested").unwrap();
        assert!(
            nested_entry.size >= 500,
            "expected ≥ 500 bytes, got {}",
            nested_entry.size
        );
        assert!(nested_entry.is_dir);
    }

    #[test]
    fn scan_dir_sorts_children_by_size_descending() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Sizes a block apart, so the ordering survives block rounding.
        fs::write(root.join("small.bin"), vec![0u8; 8 * 1024]).unwrap();
        fs::write(root.join("big.bin"), vec![0u8; 512 * 1024]).unwrap();
        fs::write(root.join("medium.bin"), vec![0u8; 64 * 1024]).unwrap();

        let result = scan_dir(root);

        assert_eq!(result.children.len(), 3);
        assert_eq!(result.children[0].name, "big.bin");
        assert_eq!(result.children[1].name, "medium.bin");
        assert_eq!(result.children[2].name, "small.bin");
    }

    #[test]
    #[cfg(unix)]
    fn scan_dir_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path();

        // Target dir outside the scanned tree with substantial content
        let target_parent = tempdir().unwrap();
        let target = target_parent.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("big.bin"), vec![0u8; 10_000]).unwrap();

        // Symlink inside scanned root pointing to that dir
        symlink(&target, root.join("link")).unwrap();

        // Plus a real small file for sanity
        fs::write(root.join("real.txt"), vec![0u8; 50]).unwrap();

        let result = scan_dir(root);

        // Symlink should be skipped entirely
        let names: Vec<&str> = result.children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            !names.contains(&"link"),
            "symlink should not appear: {names:?}"
        );
        assert!(names.contains(&"real.txt"));

        // Total must not include the 10,000-byte symlink target
        assert!(
            result.total_size < 10_000,
            "symlink target was followed: total={}",
            result.total_size
        );
    }
}
