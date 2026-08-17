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
    /// Delete only those immediate children of `dir` that nothing has touched
    /// in `idle_days` and that no running process is using.
    ///
    /// Session scratch areas need this: the directory as a whole is never
    /// disposable, because some of its children belong to work still running.
    RemoveIdleChildren {
        dir: PathBuf,
        idle_days: u64,
    },
}

/// Children of `dir` that are old enough and that nothing is using.
/// Shared by size estimation and by the delete itself so the number shown
/// and the set removed cannot drift apart.
pub fn idle_children(dir: &std::path::Path, idle_days: u64) -> Vec<(PathBuf, u64)> {
    let live = crate::liveness::Liveness::snapshot();
    idle_children_with(dir, idle_days, &live)
}

pub fn idle_children_with(
    dir: &std::path::Path,
    idle_days: u64,
    live: &crate::liveness::Liveness,
) -> Vec<(PathBuf, u64)> {
    let now = std::time::SystemTime::now();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !p.is_symlink())
        .filter(|p| crate::util::idle_days(p, now).is_some_and(|d| d >= idle_days))
        .filter(|p| !live.is_busy(p))
        .map(|p| {
            let size = dir_size(&p);
            (p, size)
        })
        .collect()
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
            CleanAction::RemoveIdleChildren { dir, idle_days } => {
                // Recomputed here rather than reusing the discovery-time list:
                // a child that went busy since the scan must survive.
                let mut freed = 0u64;
                for (path, size) in idle_children(dir, *idle_days) {
                    rm_rf(&path)?;
                    freed += size;
                }
                Ok(freed)
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
// files before unlink, whereas std::fs returns EACCES. Go module/toolchain
// caches go further and mark parent dirs read-only too (mode 555), so even
// `rm -f` can't unlink children — `chmod -R u+w` restores write perms first.
fn rm_rf(path: &std::path::Path) -> Result<(), String> {
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(path).output();

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
    #[cfg(unix)]
    fn remove_dir_handles_go_module_cache_layout() {
        // Go module/toolchain caches mark BOTH files (0444) AND their parent
        // dirs (0555) read-only. Even `rm -f` can't unlink files when the
        // parent dir lacks write perm — chmod -R u+w must restore them first.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let cache = dir.path().join("toolchain");
        let bin = cache.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let go_bin = bin.join("go");
        fs::write(&go_bin, b"#!/bin/sh\n").unwrap();

        // Lock down innermost file, then its parent dir, then the cache root.
        fs::set_permissions(&go_bin, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o555)).unwrap();

        let target = Target::new("test", "", 8, CleanAction::RemoveDir(cache.clone()));
        let result = target.clean();
        assert!(result.is_ok(), "expected ok, got {result:?}");
        assert!(!cache.exists());
    }

    fn age(path: &std::path::Path, days: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        let ft = filetime::FileTime::from_system_time(when);
        for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
            let _ = filetime::set_file_mtime(entry.path(), ft);
        }
    }

    #[test]
    fn idle_children_picks_only_the_old_ones() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let stale = root.join("session-stale");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("blob.bin"), vec![0u8; 4096]).unwrap();
        age(&stale, 30);

        let fresh = root.join("session-fresh");
        fs::create_dir(&fresh).unwrap();
        fs::write(fresh.join("blob.bin"), vec![0u8; 4096]).unwrap();

        let picked = idle_children_with(root, 7, &crate::liveness::Liveness::empty());
        let names: Vec<String> = picked
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["session-stale"], "got {names:?}");
    }

    #[test]
    fn idle_children_never_returns_a_busy_child() {
        // The exact failure this action exists to prevent: an old-looking
        // scratch dir that a live process is still working in.
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let stale = root.join("session-stale");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("blob.bin"), vec![0u8; 4096]).unwrap();
        age(&stale, 30);

        // Nothing running: it is a candidate.
        let free = idle_children_with(&root, 7, &crate::liveness::Liveness::empty());
        assert_eq!(free.len(), 1, "expected the stale dir to be picked");

        // Same dir, but a process is sitting in it.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30; true")
            .current_dir(&stale)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let pid = child.id() as i32;
        let mut live = crate::liveness::Liveness::snapshot();
        for _ in 0..40 {
            if live.is_busy(&stale) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            live = crate::liveness::Liveness::snapshot();
        }

        let picked = idle_children_with(&root, 7, &live);
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            picked.is_empty(),
            "stale-but-busy dir was offered for deletion (pid {pid}): {picked:?}"
        );
    }

    #[test]
    fn idle_children_ignores_symlinks() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("big.bin"), vec![0u8; 8192]).unwrap();
        age(outside.path(), 60);

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();

        let picked = idle_children_with(root, 7, &crate::liveness::Liveness::empty());
        assert!(picked.is_empty(), "symlink was followed: {picked:?}");
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
