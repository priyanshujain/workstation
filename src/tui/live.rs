//! The shared core of a live drill-down.
//!
//! Measuring one directory a level at a time is the same job on every screen:
//! treat each subdirectory as its own sweep root so it finishes, and reports,
//! on its own, and measure the loose files up front because no root will ever
//! account for them. The screens differ in what they draw, not in this.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use disk::sweep::{Denial, Root};
use disk::util::allocated;

/// Measured as smooth: fast enough to read as live, slow enough that drawing
/// does not compete with the walk for the terminal.
pub const REDRAW: Duration = Duration::from_millis(60);

/// One directory, split the way [`disk::sweep::sweep_streaming`] wants it.
/// Siblings never nest, so the roots do not overlap and nothing needs
/// excluding.
#[derive(Debug, Clone, Default)]
pub struct Children {
    pub roots: Vec<Root>,
    /// Everything that is not a subdirectory, already final. A caller that
    /// ignores this reports a directory as smaller than it is.
    pub loose_bytes: u64,
    pub loose_files: usize,
    /// Why the directory would not open. `None` means it opened, whatever it
    /// held: a refusal has to be told apart from an empty directory, or a
    /// screen draws "(empty)" over bytes it was never allowed to see.
    pub denied: Option<Denial>,
}

/// A symlink is a loose entry whatever it points at: walking one would bill
/// this directory for bytes that live elsewhere, and bill them twice if the
/// target is also on the volume.
pub fn children_as_roots(dir: &Path) -> Children {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            return Children {
                denied: refusal(&e),
                ..Children::default()
            };
        }
    };

    let mut children = Children::default();
    let mut seen = HashSet::new();

    for entry in entries.flatten() {
        // `DirEntry::metadata` is `lstat` on unix, so a symlink describes
        // itself rather than its target.
        let Ok(meta) = entry.metadata() else { continue };

        if meta.is_dir() {
            children.roots.push(Root {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
            });
        } else {
            children.loose_bytes += allocated(&meta, &mut seen);
            children.loose_files += 1;
        }
    }

    // No size is known until the sweep reports one, so name order is the only
    // stable order the first frame can have.
    children.roots.sort_by(|a, b| a.name.cmp(&b.name));
    children
}

/// Only a refusal is a refusal. `read_dir` fails the same way for a plain file
/// (ENOTDIR) and for a path that is gone (ENOENT), and calling either of those
/// a permission problem sends the user off to try sudo on something sudo cannot
/// fix. The two kinds of genuine refusal are kept apart by
/// [`Denial::of`]: only one of them is what Full Disk Access is for.
fn refusal(error: &std::io::Error) -> Option<Denial> {
    (error.kind() == std::io::ErrorKind::PermissionDenied).then(|| Denial::of(error))
}

/// How often a screen is allowed to repaint from inside a walk. Both screens
/// share it so a redraw costs the same wherever it happens.
pub struct Throttle {
    interval: Duration,
    painted: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Throttle {
            interval,
            painted: None,
        }
    }

    pub fn ready(&mut self) -> bool {
        if self.painted.is_none_or(|at| at.elapsed() >= self.interval) {
            self.painted = Some(Instant::now());
            return true;
        }
        false
    }

    /// The next [`Throttle::ready`] says yes whatever the clock says, for the
    /// frames that have to land: a finished walk, a cancel, a new view.
    pub fn force(&mut self) {
        self.painted = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// The binary crate has no dev-dependencies, so scratch trees are built by
    /// hand rather than with `tempfile`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("wsctl-live-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn file(&self, name: &str, size: usize) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, vec![0u8; size]).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(children: &Children) -> Vec<String> {
        children.roots.iter().map(|r| r.name.clone()).collect()
    }

    #[test]
    fn subdirectories_become_roots_and_files_do_not() {
        let scratch = Scratch::new("roots");
        let sub = scratch.dir("sub");
        scratch.file("loose.bin", 8 * 1024);

        let children = children_as_roots(scratch.path());

        assert_eq!(names(&children), vec!["sub".to_string()]);
        assert_eq!(children.roots[0].path, sub);
        assert_eq!(children.loose_files, 1);
        assert_eq!(children.denied, None);
    }

    #[test]
    fn an_empty_readable_directory_is_empty_rather_than_refused() {
        let scratch = Scratch::new("empty");

        let children = children_as_roots(scratch.path());

        assert!(children.roots.is_empty());
        assert_eq!(children.loose_files, 0);
        assert_eq!(children.loose_bytes, 0);
        assert_eq!(
            children.denied, None,
            "an empty directory opened fine and must not read as refused"
        );
    }

    #[test]
    fn roots_are_alphabetical_before_any_size_is_known() {
        let scratch = Scratch::new("order");
        for name in ["zulu", "alpha", "mike"] {
            scratch.dir(name);
        }

        let children = children_as_roots(scratch.path());

        assert_eq!(names(&children), vec!["alpha", "mike", "zulu"]);
    }

    #[test]
    fn a_root_keeps_the_full_path_and_the_bare_name() {
        let scratch = Scratch::new("naming");
        let sub = scratch.dir("Application Support");

        let children = children_as_roots(scratch.path());

        assert_eq!(children.roots[0].name, "Application Support");
        assert_eq!(children.roots[0].path, sub);
        assert!(children.roots[0].path.is_absolute());
    }

    #[test]
    fn loose_files_are_summed_as_allocated_blocks() {
        let scratch = Scratch::new("loose");
        scratch.file("small.bin", 100);
        scratch.file("also-small.bin", 1000);

        let children = children_as_roots(scratch.path());

        assert_eq!(children.loose_files, 2);
        // Two files under a block each still occupy a block each: apparent
        // length would report 1100, which is not what the volume gave away.
        assert!(
            children.loose_bytes >= 8 * 1024,
            "blocks not counted: {}",
            children.loose_bytes
        );
    }

    #[test]
    fn a_sparse_loose_file_costs_what_it_allocates() {
        let scratch = Scratch::new("sparse");
        let file = fs::File::create(scratch.path().join("sparse.bin")).unwrap();
        file.set_len(512 * 1024 * 1024).unwrap();
        drop(file);

        let children = children_as_roots(scratch.path());

        assert!(
            children.loose_bytes < 1024 * 1024,
            "apparent length used instead of blocks: {}",
            children.loose_bytes
        );
    }

    #[test]
    fn a_hardlinked_pair_of_loose_files_is_charged_once() {
        let scratch = Scratch::new("hardlink");
        let original = scratch.file("original.bin", 512 * 1024);
        fs::hard_link(&original, scratch.path().join("clone.bin")).unwrap();

        let children = children_as_roots(scratch.path());

        assert_eq!(children.loose_files, 2);
        assert!(
            children.loose_bytes < 1024 * 1024,
            "one inode billed twice: {}",
            children.loose_bytes
        );
    }

    #[test]
    fn a_symlink_to_a_directory_is_loose_and_never_walked() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new("symlink");
        let elsewhere = Scratch::new("symlink-target");
        elsewhere.file("huge.bin", 4 * 1024 * 1024);
        symlink(elsewhere.path(), scratch.path().join("link")).unwrap();

        let children = children_as_roots(scratch.path());

        assert!(
            children.roots.is_empty(),
            "a symlink was handed to the walker: {:?}",
            names(&children)
        );
        assert_eq!(children.loose_files, 1);
        assert!(
            children.loose_bytes < 4 * 1024 * 1024,
            "followed the symlink: {}",
            children.loose_bytes
        );
    }

    #[test]
    fn an_unreadable_directory_reports_the_refusal_instead_of_reading_as_empty() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("locked");
        let locked = scratch.dir("locked");
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let children = children_as_roots(&locked);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        assert!(children.roots.is_empty());
        assert_eq!(children.loose_bytes, 0);
        assert_eq!(children.loose_files, 0);
        // chmod 000 is a unix refusal, not a privacy one, so the advice is sudo
        // and not Full Disk Access.
        assert_eq!(children.denied, Some(Denial::Forbidden));
    }

    #[test]
    fn a_missing_directory_is_not_a_refusal() {
        let children = children_as_roots(Path::new("/this/path/does/not/exist/xyz"));

        assert!(children.roots.is_empty());
        assert_eq!(children.loose_bytes, 0);
        assert_eq!(children.loose_files, 0);
        assert_eq!(children.denied, None, "a path that is gone was not refused");
    }

    #[test]
    fn a_file_handed_in_as_a_directory_is_not_a_refusal() {
        let scratch = Scratch::new("not-a-dir");
        let file = scratch.file("plain.bin", 8 * 1024);

        let children = children_as_roots(&file);

        assert!(children.roots.is_empty());
        assert_eq!(children.loose_files, 0);
        // ENOTDIR. Reporting it as Forbidden would tell the user to try sudo on
        // something sudo cannot fix.
        assert_eq!(children.denied, None);
    }

    #[test]
    fn the_throttle_paints_once_and_then_suppresses_the_next_call() {
        let mut throttle = Throttle::new(Duration::from_secs(60));

        assert!(throttle.ready(), "the first frame must always be drawn");
        assert!(!throttle.ready());
        assert!(!throttle.ready());
    }

    #[test]
    fn forcing_the_throttle_overrides_the_interval() {
        let mut throttle = Throttle::new(Duration::from_secs(60));

        assert!(throttle.ready());
        assert!(!throttle.ready());
        throttle.force();
        assert!(throttle.ready(), "a forced frame must land");
        assert!(
            !throttle.ready(),
            "force is one frame, not a reset to always"
        );
    }

    #[test]
    fn a_zero_interval_throttle_never_suppresses() {
        let mut throttle = Throttle::new(Duration::ZERO);

        assert!(throttle.ready());
        assert!(throttle.ready());
    }
}
