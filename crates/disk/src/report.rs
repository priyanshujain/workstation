use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audit::{Audit, Category, CategoryPath};
use crate::projects::{Artifact, Project, ProjectKind};
use crate::sweep::{RootUsage, Unreadable};

/// Bumped whenever the shape below changes. A cache written by an older
/// version is discarded rather than migrated.
pub const SCHEMA: u32 = 2;

/// Past this the cache is ignored even if present, so an unloaded or broken
/// refresh agent degrades to slow-but-correct instead of silently ancient.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    /// Unix seconds. Stored as an instant, never as a precomputed age, so
    /// everything derived from it stays right as the cache gets older.
    pub generated_at: u64,
    /// The volume, partitioned. Sums to what the walk could see, so the
    /// categories below can be checked against it instead of floating free.
    pub roots: Vec<RootSnap>,
    pub categories: Vec<CategorySnap>,
    pub projects: Vec<ProjectSnap>,
}

#[derive(Serialize, Deserialize)]
pub struct RootSnap {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub unattributed_total: u64,
    pub unattributed: Vec<(PathBuf, u64)>,
    pub unreadable: Vec<UnreadableSnap>,
    pub unreadable_count: usize,
}

#[derive(Serialize, Deserialize)]
pub struct UnreadableSnap {
    pub path: PathBuf,
    /// Serialised as a bool because there are exactly two kinds and a bare
    /// enum in the cache would need a migration the first time a third shows up.
    pub protected: bool,
}

#[derive(Serialize, Deserialize)]
pub struct CategorySnap {
    pub name: String,
    pub total_size: u64,
    pub paths: Vec<PathSnap>,
}

#[derive(Serialize, Deserialize)]
pub struct PathSnap {
    pub label: String,
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectSnap {
    pub root: PathBuf,
    pub kind: ProjectKind,
    pub artifacts: Vec<ArtifactSnap>,
    /// Unix seconds of the newest source file at scan time.
    pub last_active: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct ArtifactSnap {
    pub path: PathBuf,
    pub size: u64,
}

/// Where a set of numbers came from, so callers can say so out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Fresh,
    Cache { age: Duration },
}

impl Report {
    pub fn age(&self, now: SystemTime) -> Duration {
        let generated = UNIX_EPOCH + Duration::from_secs(self.generated_at);
        now.duration_since(generated).unwrap_or(Duration::ZERO)
    }

    pub fn to_categories(&self) -> Vec<Category> {
        self.categories
            .iter()
            .map(|c| Category {
                name: c.name.clone(),
                total_size: c.total_size,
                paths: c
                    .paths
                    .iter()
                    .map(|p| CategoryPath {
                        label: p.label.clone(),
                        path: p.path.clone(),
                        size: p.size,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn to_roots(&self) -> Vec<RootUsage> {
        self.roots
            .iter()
            .map(|r| RootUsage {
                name: r.name.clone(),
                path: r.path.clone(),
                total: r.total,
                unattributed_total: r.unattributed_total,
                unattributed: r.unattributed.clone(),
                unreadable: r
                    .unreadable
                    .iter()
                    .map(|u| Unreadable {
                        path: u.path.clone(),
                        denial: if u.protected {
                            crate::sweep::Denial::Protected
                        } else {
                            crate::sweep::Denial::Forbidden
                        },
                    })
                    .collect(),
                unreadable_count: r.unreadable_count,
            })
            .collect()
    }

    pub fn to_audit(&self) -> Audit {
        Audit {
            roots: self.to_roots(),
            categories: self.to_categories(),
        }
    }

    pub fn to_projects(&self) -> Vec<Project> {
        self.projects
            .iter()
            .map(|p| Project {
                root: p.root.clone(),
                kind: p.kind,
                artifacts: p
                    .artifacts
                    .iter()
                    .map(|a| Artifact {
                        path: a.path.clone(),
                        size: a.size,
                    })
                    .collect(),
                last_active: p.last_active.map(|s| UNIX_EPOCH + Duration::from_secs(s)),
            })
            .collect()
    }

    /// Artifact dirs touched since the scan, whose cached size is now a guess.
    /// One stat per dir, cheap enough to run on every read.
    pub fn stale_paths(&self) -> Vec<&Path> {
        let cutoff = UNIX_EPOCH + Duration::from_secs(self.generated_at);
        self.projects
            .iter()
            .flat_map(|p| p.artifacts.iter())
            .filter(|a| changed_since(&a.path, cutoff))
            .map(|a| a.path.as_path())
            .collect()
    }
}

fn changed_since(path: &Path, cutoff: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|m| m > cutoff)
}

pub fn cache_path() -> Option<PathBuf> {
    // Application Support, deliberately not Caches: wsctl's own cleanup
    // empties Caches, which would have it delete its own report.
    Some(dirs::data_dir()?.join("wsctl").join("disk-report.json"))
}

/// Read the cache. `None` for missing, unreadable, corrupt, wrong schema, or
/// older than `MAX_AGE`; every one of those means "just rescan".
pub fn load(now: SystemTime) -> Option<Report> {
    let path = cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let report: Report = serde_json::from_slice(&bytes).ok()?;
    if report.schema != SCHEMA {
        return None;
    }
    if report.age(now) > MAX_AGE {
        return None;
    }
    Some(report)
}

pub fn save(report: &Report) -> std::io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Write-then-rename so a crash mid-write cannot leave a half-parsed file
    // that every later read has to reject.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

/// Walk the disk and build a fresh report. This is the slow path, tens of
/// seconds, and is what the scheduled refresh runs.
pub fn generate(project_roots: &[PathBuf], max_depth: usize, now: SystemTime) -> Report {
    let audit = crate::audit::scan();
    let roots = audit
        .roots
        .into_iter()
        .map(|r| RootSnap {
            name: r.name,
            path: r.path,
            total: r.total,
            unattributed_total: r.unattributed_total,
            unattributed: r.unattributed,
            unreadable: r
                .unreadable
                .into_iter()
                .map(|u| UnreadableSnap {
                    protected: u.denial == crate::sweep::Denial::Protected,
                    path: u.path,
                })
                .collect(),
            unreadable_count: r.unreadable_count,
        })
        .collect();
    let categories = audit
        .categories
        .into_iter()
        .map(|c| CategorySnap {
            name: c.name,
            total_size: c.total_size,
            paths: c
                .paths
                .into_iter()
                .map(|p| PathSnap {
                    label: p.label,
                    path: p.path,
                    size: p.size,
                })
                .collect(),
        })
        .collect();

    let projects = crate::projects::discover(project_roots, max_depth)
        .into_iter()
        .map(|p| ProjectSnap {
            root: p.root,
            kind: p.kind,
            artifacts: p
                .artifacts
                .into_iter()
                .map(|a| ArtifactSnap {
                    path: a.path,
                    size: a.size,
                })
                .collect(),
            last_active: p
                .last_active
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        })
        .collect();

    Report {
        schema: SCHEMA,
        generated_at: now
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs(),
        roots,
        categories,
        projects,
    }
}

/// The one entry point commands use.
///
/// `no_cache` forces a walk. Either way a successful walk is written back, so
/// there is no separate refresh command to remember: the slow path always
/// leaves the fast path better off than it found it.
pub fn load_or_refresh(
    project_roots: &[PathBuf],
    max_depth: usize,
    no_cache: bool,
) -> (Report, Source) {
    let now = SystemTime::now();

    if !no_cache && let Some(report) = load(now) {
        let age = report.age(now);
        return (report, Source::Cache { age });
    }

    let report = generate(project_roots, max_depth, now);
    if let Err(e) = save(&report) {
        tracing::warn!("could not write disk report cache: {e}");
    }
    (report, Source::Fresh)
}

/// "4h ago", for telling the user how old the numbers are.
pub fn humanize_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 90 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 90 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 36 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_at(secs_ago: u64) -> Report {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Report {
            schema: SCHEMA,
            generated_at: now - secs_ago,
            roots: Vec::new(),
            categories: Vec::new(),
            projects: Vec::new(),
        }
    }

    #[test]
    fn age_is_derived_not_stored() {
        let report = report_at(7200);
        let age = report.age(SystemTime::now());
        assert!(
            (7195..=7205).contains(&age.as_secs()),
            "got {}s",
            age.as_secs()
        );
    }

    #[test]
    fn round_trips_through_json() {
        let report = Report {
            schema: SCHEMA,
            generated_at: 1_700_000_000,
            roots: Vec::new(),
            categories: vec![CategorySnap {
                name: "Rust".into(),
                total_size: 4096,
                paths: vec![PathSnap {
                    label: "cargo".into(),
                    path: PathBuf::from("/tmp/cargo"),
                    size: 4096,
                }],
            }],
            projects: vec![ProjectSnap {
                root: PathBuf::from("/tmp/app"),
                kind: ProjectKind::Cargo,
                artifacts: vec![ArtifactSnap {
                    path: PathBuf::from("/tmp/app/target"),
                    size: 9000,
                }],
                last_active: Some(1_699_000_000),
            }],
        };

        let json = serde_json::to_vec(&report).unwrap();
        let back: Report = serde_json::from_slice(&json).unwrap();

        assert_eq!(back.generated_at, 1_700_000_000);
        assert_eq!(back.categories[0].name, "Rust");
        assert_eq!(back.projects[0].kind, ProjectKind::Cargo);
        assert_eq!(back.projects[0].last_active, Some(1_699_000_000));

        let projects = back.to_projects();
        assert_eq!(projects[0].artifact_size(), 9000);
        assert!(projects[0].last_active.is_some());
    }

    #[test]
    fn idle_days_recompute_as_the_cache_ages() {
        // The reason last_active is an instant: a cache written two days ago
        // must report two more days of idleness, not the number it froze.
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let snap = ProjectSnap {
            root: PathBuf::from("/tmp/app"),
            kind: ProjectKind::Cargo,
            artifacts: Vec::new(),
            last_active: Some(ten_days_ago.duration_since(UNIX_EPOCH).unwrap().as_secs()),
        };
        let report = Report {
            schema: SCHEMA,
            generated_at: 0,
            roots: Vec::new(),
            categories: Vec::new(),
            projects: vec![snap],
        };

        let project = &report.to_projects()[0];
        assert_eq!(project.idle_days(SystemTime::now()), Some(10));

        let in_a_week = SystemTime::now() + Duration::from_secs(7 * 86_400);
        assert_eq!(project.idle_days(in_a_week), Some(17));
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let mut report = report_at(60);
        report.schema = SCHEMA + 1;
        let json = serde_json::to_vec(&report).unwrap();
        let parsed: Report = serde_json::from_slice(&json).unwrap();
        assert_ne!(parsed.schema, SCHEMA, "guard must reject this");
    }

    #[test]
    fn cache_lives_outside_the_caches_directory() {
        // wsctl's own cleanup empties ~/Library/Caches, so the report must
        // not live there or it would delete its own cache.
        let path = cache_path().expect("a data dir");
        assert!(
            !path.to_string_lossy().contains("/Caches/"),
            "report would be self-deleted at {}",
            path.display()
        );
        assert!(
            path.ends_with("wsctl/disk-report.json"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn stale_paths_flags_dirs_touched_since_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("target");
        std::fs::create_dir(&artifact).unwrap();

        // Scan claims to predate the directory.
        let report = Report {
            schema: SCHEMA,
            generated_at: 1,
            roots: Vec::new(),
            categories: Vec::new(),
            projects: vec![ProjectSnap {
                root: dir.path().to_path_buf(),
                kind: ProjectKind::Cargo,
                artifacts: vec![ArtifactSnap {
                    path: artifact.clone(),
                    size: 0,
                }],
                last_active: None,
            }],
        };

        assert_eq!(report.stale_paths(), vec![artifact.as_path()]);
    }

    #[test]
    fn humanize_age_reads_naturally() {
        assert_eq!(humanize_age(Duration::from_secs(10)), "just now");
        assert_eq!(humanize_age(Duration::from_secs(600)), "10m ago");
        assert_eq!(humanize_age(Duration::from_secs(4 * 3600)), "4h ago");
        assert_eq!(humanize_age(Duration::from_secs(3 * 86_400)), "3d ago");
    }
}
