use std::path::{Path, PathBuf};

use walkdir::WalkDir;

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
        let size = subtree_size(&child_path);
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

fn subtree_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
