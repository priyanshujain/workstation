use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::platform;
use crate::util::dir_size;

pub struct Target {
    pub name: String,
    pub description: String,
    /// Estimated reclaimable bytes (0 means unknown/not scannable)
    pub size: u64,
    action: CleanAction,
}

pub enum CleanAction {
    /// Empty a directory's contents but keep the directory itself.
    RemoveContents(PathBuf),
    /// Delete a directory and everything inside it.
    RemoveDir(PathBuf),
    /// Delete a single file.
    RemoveFile(PathBuf),
    RunCommand(String, Vec<String>),
    RemoveByExtension(PathBuf, String),
}

impl Target {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        size: u64,
        action: CleanAction,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size,
            action,
        }
    }

    pub fn action(&self) -> &CleanAction {
        &self.action
    }

    pub fn clean(&self) -> Result<u64, String> {
        match &self.action {
            CleanAction::RemoveContents(path) => {
                let size = dir_size(path);
                if path.exists() {
                    for entry in fs::read_dir(path).map_err(|e| e.to_string())?.flatten() {
                        rm_rf(&entry.path())?;
                    }
                }
                Ok(size)
            }
            CleanAction::RemoveDir(path) => {
                let size = dir_size(path);
                if path.exists() {
                    rm_rf(path)?;
                }
                Ok(size)
            }
            CleanAction::RemoveFile(path) => {
                let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                if path.exists() {
                    rm_rf(path)?;
                }
                Ok(size)
            }
            CleanAction::RunCommand(cmd, args) => {
                let size_before = self.size;
                let output = Command::new(cmd)
                    .args(args)
                    .output()
                    .map_err(|e| format!("{cmd}: {e}"))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("{cmd} failed: {stderr}"));
                }
                Ok(size_before)
            }
            CleanAction::RemoveByExtension(dir, ext) => {
                let mut freed = 0u64;
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|e| e == ext.as_str())
                            && let Ok(meta) = path.metadata()
                        {
                            freed += meta.len();
                            rm_rf(&path)?;
                        }
                    }
                }
                Ok(freed)
            }
        }
    }
}

// `rm -rf` instead of fs::remove_* because BSD/macOS `rm -f` chmods read-only
// files before unlink, whereas std::fs returns EACCES. Trees like checked-out
// Go modules or .git packfiles often have read-only entries deep inside.
fn rm_rf(path: &std::path::Path) -> Result<(), String> {
    let output = Command::new("rm")
        .arg("-rf")
        .arg(path)
        .output()
        .map_err(|e| format!("rm: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rm -rf {}: {}", path.display(), stderr.trim()));
    }
    Ok(())
}

/// Discover all cleanup targets and scan their sizes.
pub fn discover_cleanup_targets() -> Vec<Target> {
    platform::cleanup_targets()
}

/// Discover the unified cleanup list: curated smart actions plus every
/// audit-category path, sorted by size descending. Audit paths whose
/// directory already appears as a curated `RemoveContents` target are skipped
/// to avoid duplicate rows.
pub fn discover_all_targets() -> Vec<Target> {
    let mut targets = platform::cleanup_targets();

    let curated_contents: std::collections::HashSet<PathBuf> = targets
        .iter()
        .filter_map(|t| match &t.action {
            CleanAction::RemoveContents(p) | CleanAction::RemoveDir(p) => Some(p.clone()),
            _ => None,
        })
        .collect();

    for (cat_name, paths) in platform::audit_categories() {
        for (label, path) in paths {
            if curated_contents.contains(&path) {
                continue;
            }
            let size = dir_size(&path);
            if size == 0 {
                continue;
            }
            targets.push(Target::new(
                format!("{cat_name} > {label}"),
                path.display().to_string(),
                size,
                CleanAction::RemoveContents(path),
            ));
        }
    }

    targets.sort_by_key(|t| std::cmp::Reverse(t.size));
    targets
}

#[cfg(target_os = "macos")]
pub(crate) fn files_by_extension_size(dir: &std::path::Path, ext: &str) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == ext)
                && let Ok(meta) = path.metadata()
            {
                size += meta.len();
            }
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    #[cfg(unix)]
    fn remove_dir_handles_read_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let nested = dir.path().join("repo").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let locked = nested.join("locked.txt");
        fs::write(&locked, b"contents").unwrap();

        // Make the file read-only — fs::remove_dir_all would EACCES here.
        let mut perms = fs::metadata(&locked).unwrap().permissions();
        perms.set_mode(0o400);
        fs::set_permissions(&locked, perms).unwrap();

        let target = Target::new(
            "test",
            "",
            8,
            CleanAction::RemoveDir(dir.path().join("repo")),
        );
        let result = target.clean();
        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert!(!dir.path().join("repo").exists());
    }

    #[test]
    fn remove_contents_keeps_parent_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.bin"), b"x").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.bin"), b"y").unwrap();

        let target = Target::new("test", "", 0, CleanAction::RemoveContents(root.clone()));
        target.clean().unwrap();

        assert!(root.exists(), "parent dir must remain");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}
