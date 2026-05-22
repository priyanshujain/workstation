use std::path::PathBuf;

use crate::platform;
use crate::util::dir_size;

pub struct Category {
    pub name: String,
    pub paths: Vec<CategoryPath>,
    pub total_size: u64,
}

pub struct CategoryPath {
    pub label: String,
    pub path: PathBuf,
    pub size: u64,
}

/// Scan disk usage by category. Returns categories sorted by size (largest first).
pub fn scan_categories() -> Vec<Category> {
    let mut categories: Vec<Category> = platform::audit_categories()
        .into_iter()
        .map(|(name, paths)| build_category(name, paths))
        .collect();
    categories.sort_by_key(|c| std::cmp::Reverse(c.total_size));
    categories.retain(|c| c.total_size > 0);
    categories
}

fn build_category(name: &str, paths: Vec<(&str, PathBuf)>) -> Category {
    let mut cat_paths = Vec::new();
    let mut total_size = 0u64;
    for (label, path) in paths {
        let size = dir_size(&path);
        if size > 0 {
            total_size += size;
            cat_paths.push(CategoryPath {
                label: label.to_string(),
                path,
                size,
            });
        }
    }
    Category {
        name: name.to_string(),
        paths: cat_paths,
        total_size,
    }
}
