use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::util::dir_size;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Cargo,
    Node,
    Python,
    Gradle,
}

impl ProjectKind {
    pub fn label(&self) -> &'static str {
        match self {
            ProjectKind::Cargo => "cargo",
            ProjectKind::Node => "node",
            ProjectKind::Python => "python",
            ProjectKind::Gradle => "gradle",
        }
    }

    /// How to rebuild what cleanup removes. Shown so the cost is visible
    /// before anything is deleted.
    pub fn rebuild_hint(&self) -> &'static str {
        match self {
            ProjectKind::Cargo => "cargo build",
            ProjectKind::Node => "npm install",
            ProjectKind::Python => "recreate the virtualenv",
            ProjectKind::Gradle => "gradle build",
        }
    }

    fn markers(&self) -> &'static [&'static str] {
        match self {
            ProjectKind::Cargo => &["Cargo.toml"],
            ProjectKind::Node => &["package.json"],
            ProjectKind::Python => &["pyproject.toml", "requirements.txt", "setup.py"],
            ProjectKind::Gradle => &[
                "build.gradle",
                "build.gradle.kts",
                "settings.gradle",
                "settings.gradle.kts",
            ],
        }
    }

    fn artifact_dirs(&self) -> &'static [&'static str] {
        match self {
            ProjectKind::Cargo => &["target"],
            ProjectKind::Node => &["node_modules"],
            ProjectKind::Python => &[".venv", "venv"],
            ProjectKind::Gradle => &["build", ".gradle"],
        }
    }
}

const ALL_KINDS: [ProjectKind; 4] = [
    ProjectKind::Cargo,
    ProjectKind::Node,
    ProjectKind::Python,
    ProjectKind::Gradle,
];

/// Directory names never descended into, whichever project owns them. Keeps
/// the walk cheap and stops one project's artifacts hiding another project.
const PRUNE: [&str; 8] = [
    "target",
    "node_modules",
    ".venv",
    "venv",
    ".git",
    ".gradle",
    "__pycache__",
    "build",
];

#[derive(Debug, Clone)]
pub struct Artifact {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub kind: ProjectKind,
    pub artifacts: Vec<Artifact>,
    /// Newest mtime among the project's own sources, artifacts excluded.
    /// `None` when the project has no readable source files.
    pub last_active: Option<SystemTime>,
}

impl Project {
    pub fn artifact_size(&self) -> u64 {
        self.artifacts.iter().map(|a| a.size).sum()
    }

    /// Whole days since a source file last changed, measured from `now`.
    /// `None` when activity could not be determined, which callers must treat
    /// as "not idle" rather than guessing.
    pub fn idle_days(&self, now: SystemTime) -> Option<u64> {
        let last = self.last_active?;
        let elapsed = now.duration_since(last).unwrap_or(Duration::ZERO);
        Some(elapsed.as_secs() / 86_400)
    }

    pub fn is_idle_for(&self, days: u64, now: SystemTime) -> bool {
        self.idle_days(now).is_some_and(|d| d >= days)
    }
}

/// Where projects are looked for when no explicit root is given.
pub fn default_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    [home.join("Workspace"), home.join("go/src")]
        .into_iter()
        .filter(|p| p.is_dir())
        .collect()
}

/// Maximum directory depth the project walk descends.
pub const MAX_DEPTH: usize = 8;

/// Find every project under `roots` that has build artifacts on disk.
///
/// Projects without artifacts are skipped: there is nothing to clean and
/// listing them only buries the ones that matter. A directory can yield more
/// than one project when markers for different toolchains sit side by side.
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Vec<Project> {
    let mut found = Vec::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }

        let walk = WalkDir::new(root)
            .follow_links(false)
            .max_depth(max_depth)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                !(e.file_type().is_dir() && PRUNE.contains(&name.as_ref()))
            });

        for entry in walk.flatten() {
            if !entry.file_type().is_dir() {
                continue;
            }
            for kind in ALL_KINDS {
                if let Some(project) = classify(entry.path(), kind) {
                    found.push(project);
                }
            }
        }
    }

    found.sort_by_key(|p| std::cmp::Reverse(p.artifact_size()));
    found
}

fn classify(dir: &Path, kind: ProjectKind) -> Option<Project> {
    if !kind.markers().iter().any(|m| dir.join(m).is_file()) {
        return None;
    }

    let artifacts: Vec<Artifact> = kind
        .artifact_dirs()
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p.is_dir())
        .map(|path| {
            let size = dir_size(&path);
            Artifact { path, size }
        })
        .filter(|a| a.size > 0)
        .collect();

    if artifacts.is_empty() {
        return None;
    }

    Some(Project {
        last_active: newest_source_mtime(dir),
        root: dir.to_path_buf(),
        kind,
        artifacts,
    })
}

/// Newest mtime among source files, skipping artifact dirs.
///
/// Capped because the answer only needs to be good enough to bucket a project
/// as "touched recently" or not, and an uncapped walk of a large monorepo
/// costs far more than that answer is worth.
fn newest_source_mtime(dir: &Path) -> Option<SystemTime> {
    const MAX_ENTRIES: usize = 20_000;

    let walk = WalkDir::new(dir)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && PRUNE.contains(&name.as_ref()))
        });

    walk.flatten()
        .take(MAX_ENTRIES)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn finds_a_cargo_project_with_a_target_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root.join("Cargo.toml"), 10);
        write(&root.join("src/main.rs"), 20);
        write(&root.join("target/debug/blob.rlib"), 5000);

        let found = discover(&[dir.path().to_path_buf()], 6);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProjectKind::Cargo);
        assert_eq!(found[0].root, root);
        assert!(found[0].artifact_size() >= 5000);
    }

    #[test]
    fn skips_a_project_with_no_artifacts() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("clean-app");
        write(&root.join("Cargo.toml"), 10);
        write(&root.join("src/main.rs"), 20);

        assert!(discover(&[dir.path().to_path_buf()], 6).is_empty());
    }

    #[test]
    fn finds_a_cargo_project_nested_inside_another_project() {
        // Mirrors a Tauri layout: src-tauri/ sits inside a JS or Python app.
        let dir = tempdir().unwrap();
        let outer = dir.path().join("margin");
        write(&outer.join("pyproject.toml"), 10);
        write(&outer.join("app.py"), 20);
        write(&outer.join(".venv/lib/pkg.py"), 3000);

        let inner = outer.join("src-tauri");
        write(&inner.join("Cargo.toml"), 10);
        write(&inner.join("src/lib.rs"), 20);
        write(&inner.join("target/debug/blob.rlib"), 9000);

        let found = discover(&[dir.path().to_path_buf()], 6);

        let kinds: Vec<ProjectKind> = found.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&ProjectKind::Cargo), "got {kinds:?}");
        assert!(kinds.contains(&ProjectKind::Python), "got {kinds:?}");

        // Biggest first, so the cargo target leads.
        assert_eq!(found[0].kind, ProjectKind::Cargo);
        assert_eq!(found[0].root, inner);
    }

    #[test]
    fn artifact_dirs_are_never_descended_into() {
        // A stray Cargo.toml vendored inside node_modules must not register
        // as a project of its own.
        let dir = tempdir().unwrap();
        let root = dir.path().join("web");
        write(&root.join("package.json"), 10);
        write(&root.join("index.js"), 20);
        write(&root.join("node_modules/dep/Cargo.toml"), 10);
        write(&root.join("node_modules/dep/target/x.rlib"), 4000);

        let found = discover(&[dir.path().to_path_buf()], 8);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProjectKind::Node);
    }

    #[test]
    fn idle_days_counts_from_the_newest_source_file() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root.join("Cargo.toml"), 10);
        write(&root.join("src/main.rs"), 20);
        write(&root.join("target/debug/blob.rlib"), 5000);

        let found = discover(&[dir.path().to_path_buf()], 6);
        let project = &found[0];

        // Sources were just written, so zero days idle right now.
        assert_eq!(project.idle_days(SystemTime::now()), Some(0));
        assert!(!project.is_idle_for(1, SystemTime::now()));

        // Ten days on, the same project reads as ten days idle.
        let later = SystemTime::now() + Duration::from_secs(10 * 86_400);
        assert_eq!(project.idle_days(later), Some(10));
        assert!(project.is_idle_for(7, later));
        assert!(!project.is_idle_for(30, later));
    }

    #[test]
    fn fresh_artifacts_do_not_make_a_stale_project_look_active() {
        // The whole point of excluding artifacts from the activity signal:
        // a rebuild of untouched sources must not reset the idle clock.
        let dir = tempdir().unwrap();
        let root = dir.path().join("app");
        write(&root.join("Cargo.toml"), 10);
        write(&root.join("src/main.rs"), 20);

        let old = SystemTime::now() - Duration::from_secs(30 * 86_400);
        let old_ft = filetime::FileTime::from_system_time(old);
        filetime::set_file_mtime(root.join("Cargo.toml"), old_ft).unwrap();
        filetime::set_file_mtime(root.join("src/main.rs"), old_ft).unwrap();

        // Artifact written now, long after the sources.
        write(&root.join("target/debug/blob.rlib"), 5000);

        let found = discover(&[dir.path().to_path_buf()], 6);
        let idle = found[0].idle_days(SystemTime::now()).unwrap();

        assert!(idle >= 29, "expected ~30 days idle, got {idle}");
    }

    #[test]
    fn unknown_activity_is_never_treated_as_idle() {
        let project = Project {
            root: PathBuf::from("/nowhere"),
            kind: ProjectKind::Cargo,
            artifacts: Vec::new(),
            last_active: None,
        };
        assert_eq!(project.idle_days(SystemTime::now()), None);
        assert!(!project.is_idle_for(0, SystemTime::now()));
        assert!(!project.is_idle_for(365, SystemTime::now()));
    }
}
