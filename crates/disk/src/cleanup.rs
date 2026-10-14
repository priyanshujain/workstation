use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::sweep::{Denials, Root};
use crate::util::{allocated, dir_size};

pub struct Target {
    pub name: String,
    pub description: String,
    size: Size,
    measure: Measure,
    action: CleanAction,
    owner_app: Option<String>,
}

/// What is known about a target's reclaimable bytes.
///
/// Discovery leaves everything it has not walked as `Counting(0)`, because
/// "nothing measured yet" and "measured, and it is empty" are different claims
/// and a list that prints both as `0 B` makes the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Being measured. The bytes so far are a floor, never a total.
    Counting(u64),
    /// The walk finished. Any denial leaves `bytes` a floor too: nothing
    /// readable plus a refusal is unknown, not empty.
    Measured { bytes: u64, denials: Denials },
    /// Nobody is going to measure this: either no path holds the answer, or
    /// the walk was stopped part way.
    Unknown(u64),
}

/// How a target's size gets measured. Decided without touching the disk, so a
/// caller can draw the whole list and then start walking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Measure {
    /// Everything under this path. Cheap to hand to [`crate::sweep`] as a root.
    Tree(PathBuf),
    /// One walk per idle child, so a caller can report between children
    /// instead of blocking for the whole scratch area.
    IdleChildren { dir: PathBuf, idle_days: u64 },
    /// Nothing left to do: the size is either already known or unknowable
    /// until the action runs.
    Nothing,
}

impl Size {
    pub fn known(bytes: u64) -> Self {
        Size::Measured {
            bytes,
            denials: Denials::default(),
        }
    }

    /// What has been counted so far. A floor in every state, so it is safe to
    /// sort by and never safe to present as a total on its own.
    pub fn bytes(self) -> u64 {
        match self {
            Size::Counting(bytes) | Size::Unknown(bytes) => bytes,
            Size::Measured { bytes, .. } => bytes,
        }
    }

    /// `Some` only when the number is complete, so a sum of these cannot
    /// quietly turn a floor into a promise.
    pub fn total(self) -> Option<u64> {
        match self {
            Size::Measured { bytes, denials } if denials.total() == 0 => Some(bytes),
            _ => None,
        }
    }

    pub fn denials(self) -> Denials {
        match self {
            Size::Measured { denials, .. } => denials,
            _ => Denials::default(),
        }
    }
}

pub enum CleanAction {
    /// Empty a directory's contents but keep the directory itself.
    RemoveContents(PathBuf),
    /// Delete a directory and everything inside it.
    RemoveDir(PathBuf),
    /// Delete a single file.
    RemoveFile(PathBuf),
    RunCommand(String, Vec<String>),
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
pub fn idle_children(dir: &Path, idle_days: u64) -> Vec<(PathBuf, u64)> {
    let live = crate::liveness::Liveness::snapshot();
    idle_children_with(dir, idle_days, &live)
}

pub fn idle_children_with(
    dir: &Path,
    idle_days: u64,
    live: &crate::liveness::Liveness,
) -> Vec<(PathBuf, u64)> {
    idle_candidates(dir)
        .into_iter()
        .filter_map(|path| idle_child(&path, idle_days, live).map(|size| (path, size)))
        .collect()
}

/// Every child worth asking about, from one readdir. A caller measuring these
/// one at a time stays responsive; asking for the whole set at once does not.
pub fn idle_candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !p.is_symlink())
        .collect();
    paths.sort();
    paths
}

/// Bytes `path` would free, or `None` when it is too fresh or in use.
///
/// One walk answers both questions, so the size shown and the idle test can
/// never be computed from two different readings of the same tree.
pub fn idle_child(path: &Path, idle_days: u64, live: &crate::liveness::Liveness) -> Option<u64> {
    if path.is_symlink() || live.is_busy(path) {
        return None;
    }
    let (newest, bytes) = walk_summary(path);
    let idle = SystemTime::now()
        .duration_since(newest?)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs()
        / 86_400;
    (idle >= idle_days).then_some(bytes)
}

fn walk_summary(path: &Path) -> (Option<SystemTime>, u64) {
    let mut newest = None;
    let mut bytes = 0u64;
    let mut seen = HashSet::new();
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        let Ok(meta) = entry.metadata() else { continue };
        if let Ok(modified) = meta.modified() {
            newest = newest.max(Some(modified));
        }
        bytes += allocated(&meta, &mut seen);
    }
    (newest, bytes)
}

impl Target {
    /// A target whose size is already in hand.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        size: u64,
        action: CleanAction,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size: Size::known(size),
            measure: Measure::Nothing,
            action,
            owner_app: None,
        }
    }

    /// A target listed before it is measured.
    pub fn pending(
        name: impl Into<String>,
        description: impl Into<String>,
        measure: Measure,
        action: CleanAction,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size: Size::Counting(0),
            measure,
            action,
            owner_app: None,
        }
    }

    /// A target inside a directory left closed: unmeasured, and never cleaned.
    pub fn closed(
        name: impl Into<String>,
        description: impl Into<String>,
        action: CleanAction,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size: Size::Measured {
                bytes: 0,
                denials: Denials::one(crate::sweep::Denial::Unattended),
            },
            measure: Measure::Nothing,
            action,
            owner_app: None,
        }
    }

    /// A target nothing can size up front: what a command prunes is only known
    /// once it has run.
    pub fn unknown(
        name: impl Into<String>,
        description: impl Into<String>,
        action: CleanAction,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size: Size::Unknown(0),
            measure: Measure::Nothing,
            action,
            owner_app: None,
        }
    }

    pub fn owned_by_app(mut self, app: impl Into<String>) -> Self {
        self.owner_app = Some(app.into());
        self
    }

    pub fn owner_app(&self) -> Option<&str> {
        self.owner_app.as_deref()
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn set_size(&mut self, size: Size) {
        self.size = size;
    }

    pub fn measure(&self) -> &Measure {
        &self.measure
    }

    pub fn action(&self) -> &CleanAction {
        &self.action
    }

    /// A target that holds anything left closed is never touched: deleting it
    /// opens the closed directory and raises the dialog the walk avoided.
    pub fn clean(&self) -> Result<u64, String> {
        if self.size.denials().unattended > 0 {
            return Err(format!("{} left closed: needs Full Disk Access", self.name));
        }
        self.refuse_if_in_use(&crate::liveness::Liveness::snapshot())?;
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
                let size_before = self.size.bytes();
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
        }
    }
}

impl Target {
    pub(crate) fn refuse_if_in_use(&self, live: &crate::liveness::Liveness) -> Result<(), String> {
        if let Some(app) = &self.owner_app
            && live.app_running(app)
        {
            return Err(format!("{}: quit {app} first", self.name));
        }
        let path = match &self.action {
            CleanAction::RemoveContents(path) | CleanAction::RemoveDir(path) => path,
            _ => return Ok(()),
        };
        if holds_repository(path) {
            return Err(format!("{}: holds a git repository", self.name));
        }
        match live.check(path).first() {
            Some(busy) => Err(format!("{}: {busy}", self.name)),
            None => Ok(()),
        }
    }
}

fn holds_repository(path: &Path) -> bool {
    let children = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path());
    std::iter::once(path.to_path_buf())
        .chain(children)
        .any(|dir| dir.join(".git").exists())
}

// `rm -rf` instead of fs::remove_* because BSD/macOS `rm -f` chmods read-only
// files before unlink, whereas std::fs returns EACCES. Go module/toolchain
// caches go further and mark parent dirs read-only too (mode 555), so even
// `rm -f` cannot unlink children: `chmod -R u+w` restores write perms first.
fn rm_rf(path: &Path) -> Result<(), String> {
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

/// Sweep roots for every target with a tree to walk, grouped so that no two
/// roots in one group overlap.
///
/// One sweep cannot measure a path and something inside it at the same time:
/// the walk charges nested bytes to the innermost root only, which is right for
/// attribution and wrong here. A cleanup row has to state what its own action
/// deletes, so `~/.android` counts `~/.android/avd` even though that is a row
/// of its own. Overlapping roots therefore go in separate groups and get
/// separate walks.
pub fn measurement_groups(targets: &[Target]) -> Vec<Vec<(usize, Root)>> {
    let mut groups: Vec<Vec<(usize, Root)>> = Vec::new();

    for (index, target) in targets.iter().enumerate() {
        let Measure::Tree(path) = &target.measure else {
            continue;
        };
        let root = Root {
            name: target.name.clone(),
            path: path.clone(),
        };
        match groups
            .iter_mut()
            .find(|group| group.iter().all(|(_, r)| !overlaps(&r.path, path)))
        {
            Some(group) => group.push((index, root)),
            None => groups.push(vec![(index, root)]),
        }
    }

    groups
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_target_holding_something_left_closed_is_never_cleaned() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("keep");
        fs::write(&file, b"x").unwrap();
        let target = Target::closed(
            "Closed folder",
            "",
            CleanAction::RemoveContents(dir.path().to_path_buf()),
        );

        assert_eq!(target.size().total(), None, "unmeasured, not empty");
        assert!(target.clean().is_err());
        assert!(file.exists());
    }

    #[test]
    fn a_folder_holding_a_git_repository_is_never_cleaned() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("checkout");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join("work.txt"), b"x").unwrap();
        let live = crate::liveness::Liveness::empty();

        for action in [
            CleanAction::RemoveContents(dir.path().to_path_buf()),
            CleanAction::RemoveDir(repo.clone()),
        ] {
            let target = Target::new("Caches > tool", "", 1, action);
            assert!(target.refuse_if_in_use(&live).is_err());
            assert!(target.clean().is_err());
        }
        assert!(repo.join("work.txt").exists());
    }

    #[test]
    #[cfg(unix)]
    fn remove_dir_handles_read_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let nested = dir.path().join("repo").join("deep");
        fs::create_dir_all(&nested).unwrap();
        let locked = nested.join("locked.txt");
        fs::write(&locked, b"contents").unwrap();

        // Make the file read-only: fs::remove_dir_all would EACCES here.
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
        // parent dir lacks write perm, so chmod -R u+w must restore them first.
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
    fn a_size_is_only_a_total_once_the_walk_finished() {
        // Sorting wants the floor; a total shown to a user wants the truth.
        assert_eq!(Size::Counting(4096).bytes(), 4096);
        assert_eq!(Size::Counting(4096).total(), None);
        assert_eq!(Size::Unknown(4096).total(), None);
        assert_eq!(Size::known(4096).total(), Some(4096));
    }

    #[test]
    fn a_refusal_leaves_the_bytes_a_floor() {
        // du returns 0 for a directory it was not allowed to open. A number
        // built on top of one of those is not a measurement of anything.
        let refused = Size::Measured {
            bytes: 8192,
            denials: Denials {
                protected: 1,
                ..Denials::default()
            },
        };
        assert_eq!(refused.total(), None);
        assert_eq!(refused.bytes(), 8192);
        assert_eq!(refused.denials().protected, 1);
    }

    #[test]
    fn a_nested_pair_of_paths_is_measured_by_two_separate_walks() {
        // One sweep charges nested bytes to the innermost root only, which is
        // right for attribution and wrong for a cleanup row: emptying
        // ~/.android takes ~/.android/avd with it, so its number has to
        // include it.
        let outer = PathBuf::from("/tmp/wsctl-groups/.android");
        let inner = outer.join("avd");
        let elsewhere = PathBuf::from("/tmp/wsctl-groups/.npm");
        let targets = vec![
            Target::pending(
                "outer",
                "",
                Measure::Tree(outer.clone()),
                CleanAction::RemoveContents(outer),
            ),
            Target::pending(
                "inner",
                "",
                Measure::Tree(inner.clone()),
                CleanAction::RemoveContents(inner),
            ),
            Target::pending(
                "elsewhere",
                "",
                Measure::Tree(elsewhere.clone()),
                CleanAction::RemoveContents(elsewhere),
            ),
        ];

        let groups = measurement_groups(&targets);

        assert_eq!(groups.len(), 2, "{groups:?}");
        let names: Vec<Vec<&str>> = groups
            .iter()
            .map(|g| g.iter().map(|(_, r)| r.name.as_str()).collect())
            .collect();
        assert_eq!(names, vec![vec!["outer", "elsewhere"], vec!["inner"]]);
    }

    #[test]
    fn the_same_path_twice_is_measured_twice() {
        // Two rows can share a directory: one runs `brew cleanup`, the other
        // empties the cache outright. Both have to state their own size.
        let path = PathBuf::from("/tmp/wsctl-groups/Homebrew");
        let targets = vec![
            Target::pending(
                "curated",
                "",
                Measure::Tree(path.clone()),
                CleanAction::RunCommand("brew".into(), vec![]),
            ),
            Target::pending(
                "audit",
                "",
                Measure::Tree(path.clone()),
                CleanAction::RemoveContents(path),
            ),
        ];

        assert_eq!(measurement_groups(&targets).len(), 2);
    }

    #[test]
    fn a_target_with_nothing_to_walk_gets_no_root() {
        let targets = vec![Target::unknown(
            "pnpm store",
            "",
            CleanAction::RunCommand("pnpm".into(), vec![]),
        )];
        assert!(measurement_groups(&targets).is_empty());
    }

    #[test]
    fn a_sibling_with_a_shared_name_prefix_is_not_nested() {
        // /a/bc is not inside /a/b, whatever a string comparison says.
        let first = PathBuf::from("/tmp/wsctl-groups/cache");
        let second = PathBuf::from("/tmp/wsctl-groups/cache-2");
        let targets = vec![
            Target::pending(
                "first",
                "",
                Measure::Tree(first.clone()),
                CleanAction::RemoveContents(first),
            ),
            Target::pending(
                "second",
                "",
                Measure::Tree(second.clone()),
                CleanAction::RemoveContents(second),
            ),
        ];

        assert_eq!(measurement_groups(&targets).len(), 1);
    }

    #[test]
    fn candidates_are_listed_without_walking_anything() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("session-a")).unwrap();
        fs::create_dir(dir.path().join("session-b")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), dir.path().join("link")).unwrap();

        let candidates = idle_candidates(dir.path());
        let names: Vec<String> = candidates
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["session-a", "session-b"]);
    }

    #[test]
    fn one_child_measured_alone_agrees_with_the_whole_set() {
        // The screen measures these one at a time so it can report between
        // them; the delete asks for the whole set. Both have to say the same
        // thing about the same child.
        let dir = tempdir().unwrap();
        let stale = dir.path().join("session-stale");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("blob.bin"), vec![0u8; 8192]).unwrap();
        age(&stale, 30);

        let live = crate::liveness::Liveness::empty();
        let one = idle_child(&stale, 7, &live).expect("stale enough to count");
        let all = idle_children_with(dir.path(), 7, &live);

        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, one);
        assert!(one >= 8192, "{one}");
    }

    #[test]
    fn a_fresh_child_is_worth_nothing_to_the_estimate() {
        let dir = tempdir().unwrap();
        let fresh = dir.path().join("session-fresh");
        fs::create_dir(&fresh).unwrap();
        fs::write(fresh.join("blob.bin"), vec![0u8; 8192]).unwrap();

        assert_eq!(
            idle_child(&fresh, 7, &crate::liveness::Liveness::empty()),
            None
        );
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
