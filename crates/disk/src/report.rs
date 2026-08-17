use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audit::{Audit, Category, CategoryPath};
use crate::projects::{Artifact, Project, ProjectKind};
use crate::sweep::{Denial, Denials, RootUsage, Unreadable};

/// Bumped whenever the shape below changes. A cache written by an older
/// version is discarded rather than migrated.
pub const SCHEMA: u32 = 5;

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
    pub children: Vec<(PathBuf, u64)>,
    pub unattributed_total: u64,
    pub unattributed: Vec<(PathBuf, u64)>,
    pub unreadable: Vec<UnreadableSnap>,
    /// The immediate children that would not open. Kept apart from the sample
    /// above because a drill-down has to tell a child of unknown size from one
    /// measured at zero, and `children` records the second for both.
    pub refused_children: Vec<UnreadableSnap>,
    /// Exact counts, which the capped sample above cannot supply.
    pub protected_count: usize,
    pub forbidden_count: usize,
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
                children: r.children.clone(),
                refused_children: refusals(&r.refused_children),
                unattributed_total: r.unattributed_total,
                unattributed: r.unattributed.clone(),
                unreadable: refusals(&r.unreadable),
                denials: Denials {
                    protected: r.protected_count,
                    forbidden: r.forbidden_count,
                },
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

fn refusals(entries: &[UnreadableSnap]) -> Vec<Unreadable> {
    entries
        .iter()
        .map(|u| Unreadable {
            path: u.path.clone(),
            denial: if u.protected {
                Denial::Protected
            } else {
                Denial::Forbidden
            },
        })
        .collect()
}

fn snapshot(entries: Vec<Unreadable>) -> Vec<UnreadableSnap> {
    entries
        .into_iter()
        .map(|u| UnreadableSnap {
            protected: u.denial == Denial::Protected,
            path: u.path,
        })
        .collect()
}

fn changed_since(path: &Path, cutoff: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|m| m > cutoff)
}

/// How this process was invoked, as far as the cache is concerned. Read from
/// the environment once and carried as data so every decision below can be
/// tested without root.
#[derive(Debug, Clone, Default)]
struct Invocation {
    euid: u32,
    sudo_user: Option<String>,
    sudo_uid: Option<String>,
    sudo_gid: Option<String>,
}

/// The user a privileged run is acting for, and who it owes the cache back to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Handback {
    user: String,
    home: PathBuf,
    uid: u32,
    gid: Option<u32>,
}

impl Invocation {
    fn current() -> Self {
        Self {
            // SAFETY: geteuid cannot fail and reads no memory we pass in.
            euid: unsafe { libc::geteuid() },
            sudo_user: env_var("SUDO_USER"),
            sudo_uid: env_var("SUDO_UID"),
            sudo_gid: env_var("SUDO_GID"),
        }
    }

    /// Root, reached through sudo from somebody else's account. Plain root is
    /// a different situation: it owns its own cache and keeps today's path.
    fn under_sudo(&self) -> bool {
        self.euid == 0 && self.sudo_user.as_deref().is_some_and(|u| u != "root")
    }
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_id(raw: &str) -> Option<u32> {
    raw.trim().parse().ok()
}

/// Who a privileged run has to hand the cache back to, or `None` when this is
/// an ordinary run and there is nothing to hand back.
///
/// One decision drives both the path and the ownership, so a run that cannot
/// work out the invoking user does not redirect the write either: it degrades
/// to exactly today's behaviour instead of to a guess.
fn handback(inv: &Invocation, home_of: impl Fn(&str) -> Option<PathBuf>) -> Option<Handback> {
    if !inv.under_sudo() {
        return None;
    }
    let user = inv.sudo_user.clone()?;

    let uid = inv
        .sudo_uid
        .as_deref()
        .and_then(parse_id)
        .filter(|id| *id != 0);
    let Some(uid) = uid else {
        tracing::warn!(
            "running under sudo as {user} but SUDO_UID is {}: the disk report cache stays where root put it and may need a chown",
            inv.sudo_uid.as_deref().unwrap_or("unset")
        );
        return None;
    };

    let Some(home) = home_of(&user).filter(|h| h.is_absolute()) else {
        tracing::warn!(
            "running under sudo as {user} but that account has no home directory here: the disk report cache stays where root put it and may need a chown"
        );
        return None;
    };

    let gid = inv.sudo_gid.as_deref().and_then(parse_id);
    if gid.is_none() && inv.sudo_gid.is_some() {
        tracing::warn!("SUDO_GID is not a number, so the group of the disk report is left alone");
    }

    Some(Handback {
        user,
        home,
        uid,
        gid,
    })
}

/// `dirs::data_dir()` for a home other than this process's own, which is the
/// entire point: under sudo `$HOME` is either the user's or `/var/root` and
/// there is no way to tell which from inside.
fn data_dir_in(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".local").join("share")
    }
}

fn cache_path_with(handback: Option<&Handback>) -> Option<PathBuf> {
    // Application Support, deliberately not Caches: wsctl's own cleanup
    // empties Caches, which would have it delete its own report.
    let data = match handback {
        Some(hb) => data_dir_in(&hb.home),
        None => dirs::data_dir()?,
    };
    Some(data.join("wsctl").join("disk-report.json"))
}

fn cache_path_for(inv: &Invocation, home_of: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    cache_path_with(handback(inv, home_of).as_ref())
}

/// The invoking user's home, from the password database rather than `$HOME`.
/// `None` unless it resolves to a directory that exists.
fn existing_home(user: &str) -> Option<PathBuf> {
    let name = CString::new(user).ok()?;
    // SAFETY: getpwnam returns null or a pointer to a static passwd whose
    // fields are copied out here, before any other libc call can reuse it.
    let home = unsafe {
        let pw = libc::getpwnam(name.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        PathBuf::from(OsStr::from_bytes(CStr::from_ptr((*pw).pw_dir).to_bytes()))
    };
    home.is_dir().then_some(home)
}

pub fn cache_path() -> Option<PathBuf> {
    cache_path_for(&Invocation::current(), existing_home)
}

/// One line saying how a privileged run differs from an ordinary one, or
/// `None` when it does not. Callers print it so the redirect is never silent.
pub fn privileged_notice() -> Option<String> {
    let hb = handback(&Invocation::current(), existing_home)?;
    let path = cache_path_with(Some(&hb))?;
    let gid = hb.gid.map_or_else(|| "-".to_string(), |g| g.to_string());
    Some(format!(
        "running as root under sudo: the disk report goes to {} and is handed back to {} ({}:{})",
        path.display(),
        hb.user,
        hb.uid,
        gid
    ))
}

/// Read the cache. `None` for missing, unreadable, corrupt, wrong schema, or
/// older than `MAX_AGE`; every one of those means "just rescan".
pub fn load(now: SystemTime) -> Option<Report> {
    let path = cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let report: Report = serde_json::from_slice(&bytes).ok()?;
    usable(&report, now).then_some(report)
}

/// A cache written by any other version of the shape is discarded rather than
/// migrated, so a field added below can be trusted to be there.
fn usable(report: &Report, now: SystemTime) -> bool {
    report.schema == SCHEMA && report.age(now) <= MAX_AGE
}

pub fn save(report: &Report) -> std::io::Result<()> {
    let handback = handback(&Invocation::current(), existing_home);
    let Some(path) = cache_path_with(handback.as_ref()) else {
        return Ok(());
    };
    write_cache(&path, report, handback.as_ref())
}

/// `sudo wsctl disk audit` writes into the user's own home as root. Anything
/// left there owned by root outlives the run: the unprivileged runs that
/// follow cannot rewrite a root-owned temp file, and cannot write into a
/// root-owned directory at all. So a privileged run gives back everything it
/// creates, and takes its temp file with it if it fails.
fn write_cache(path: &Path, report: &Report, handback: Option<&Handback>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        // Every directory this run brings into existence, plus the one the
        // report lands in: an earlier privileged run may have left that one
        // owned by root, and this is where that gets repaired.
        let created: Vec<PathBuf> = parent
            .ancestors()
            .skip(1)
            .take_while(|a| !a.exists())
            .map(PathBuf::from)
            .collect();
        std::fs::create_dir_all(parent)?;
        give_back(parent, handback);
        for dir in created {
            give_back(&dir, handback);
        }
    }
    let json = serde_json::to_vec_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Write-then-rename so a crash mid-write cannot leave a half-parsed file
    // that every later read has to reject.
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &json) {
        discard(&tmp);
        return Err(e);
    }
    // Handed back before the rename, so the report is never visible at its
    // real path still owned by root.
    give_back(&tmp, handback);
    if let Err(e) = std::fs::rename(&tmp, path) {
        discard(&tmp);
        return Err(e);
    }
    Ok(())
}

fn discard(tmp: &Path) {
    let _ = std::fs::remove_file(tmp);
}

/// Never fatal: a filesystem that will not chown costs the user a manual fix,
/// not the scan they just waited for.
fn give_back(path: &Path, handback: Option<&Handback>) {
    let Some(hb) = handback else {
        return;
    };
    if let Err(e) = std::os::unix::fs::chown(path, Some(hb.uid), hb.gid) {
        let gid = hb.gid.map_or_else(|| "-".to_string(), |g| g.to_string());
        tracing::warn!(
            "could not give {} back to {} ({}:{}): {e}. Later runs may need: sudo chown {}:{} {}",
            path.display(),
            hb.user,
            hb.uid,
            gid,
            hb.uid,
            gid,
            path.display()
        );
    }
}

/// Walk the disk and build a fresh report. This is the slow path, tens of
/// seconds, and is what the scheduled refresh runs.
pub fn generate(project_roots: &[PathBuf], max_depth: usize, now: SystemTime) -> Report {
    from_audit(crate::audit::scan(), project_roots, max_depth, now)
}

/// The same report from a measurement that has already been taken, so a caller
/// that walked the volume itself can write the cache back instead of walking it
/// a second time.
pub fn from_audit(
    audit: Audit,
    project_roots: &[PathBuf],
    max_depth: usize,
    now: SystemTime,
) -> Report {
    let roots = audit
        .roots
        .into_iter()
        .map(|r| RootSnap {
            name: r.name,
            path: r.path,
            total: r.total,
            children: r.children,
            refused_children: snapshot(r.refused_children),
            unattributed_total: r.unattributed_total,
            unattributed: r.unattributed,
            unreadable: snapshot(r.unreadable),
            protected_count: r.denials.protected,
            forbidden_count: r.denials.forbidden,
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
        assert!(!usable(&parsed, SystemTime::now()));
    }

    #[test]
    fn a_cache_at_the_previous_schema_is_rejected() {
        // The refused children were added at schema 5. A cache written before
        // them has no way to say that a child is unknown rather than zero, so it
        // is discarded rather than read with the field defaulted away.
        let mut report = report_at(60);
        report.schema = SCHEMA - 1;
        assert!(!usable(&report, SystemTime::now()));
    }

    #[test]
    fn a_report_at_the_new_schema_round_trips_its_refused_children() {
        let mut report = report_at(60);
        report.roots = vec![RootSnap {
            name: "Home".into(),
            path: PathBuf::from("/Users/x"),
            total: 8192,
            children: vec![(PathBuf::from("/Users/x/.Trash"), 0)],
            refused_children: vec![UnreadableSnap {
                path: PathBuf::from("/Users/x/.Trash"),
                protected: true,
            }],
            unattributed_total: 0,
            unattributed: Vec::new(),
            unreadable: vec![UnreadableSnap {
                path: PathBuf::from("/Users/x/deep/locked"),
                protected: false,
            }],
            protected_count: 1,
            forbidden_count: 1,
        }];

        let json = serde_json::to_vec(&report).unwrap();
        let back: Report = serde_json::from_slice(&json).unwrap();
        assert!(usable(&back, SystemTime::now()));

        let root = &back.to_roots()[0];
        assert_eq!(root.refused_children.len(), 1);
        assert_eq!(root.refused_children[0].denial, Denial::Protected);
        assert_eq!(root.unreadable[0].denial, Denial::Forbidden);
        assert_eq!(
            root.denials,
            Denials {
                protected: 1,
                forbidden: 1
            }
        );
        assert_eq!(
            root.children,
            vec![(PathBuf::from("/Users/x/.Trash"), 0)],
            "the zero stays where it was; the refusal is what reads it right"
        );
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

    fn sudo(uid: Option<&str>, gid: Option<&str>) -> Invocation {
        Invocation {
            euid: 0,
            sudo_user: Some("invoker".into()),
            sudo_uid: uid.map(Into::into),
            sudo_gid: gid.map(Into::into),
        }
    }

    fn home_of_invoker(user: &str) -> Option<PathBuf> {
        (user == "invoker").then(|| PathBuf::from("/Users/invoker"))
    }

    fn todays_path() -> PathBuf {
        dirs::data_dir()
            .expect("a data dir")
            .join("wsctl")
            .join("disk-report.json")
    }

    #[test]
    fn a_sudo_run_writes_into_the_invoking_users_home() {
        let path = cache_path_for(&sudo(Some("501"), Some("20")), home_of_invoker).unwrap();

        assert!(
            path.starts_with("/Users/invoker"),
            "went to {} instead of the invoking user's home",
            path.display()
        );
        assert!(
            path.ends_with("wsctl/disk-report.json"),
            "{}",
            path.display()
        );
        assert_ne!(
            path,
            todays_path(),
            "the path must come from SUDO_USER, not from whatever HOME says"
        );
        assert!(!path.starts_with("/var/root"));
    }

    #[test]
    fn an_ordinary_run_keeps_todays_path() {
        let plain = Invocation {
            euid: 501,
            ..Invocation::default()
        };
        assert_eq!(
            cache_path_for(&plain, home_of_invoker).unwrap(),
            todays_path()
        );
        assert!(handback(&plain, home_of_invoker).is_none());
    }

    #[test]
    fn an_unprivileged_shell_carrying_sudo_variables_is_not_a_sudo_run() {
        // `sudo -u someone bash` leaves SUDO_* set for everything run inside
        // it. Without euid 0 there is nothing to hand back and nothing to move.
        let inv = Invocation {
            euid: 501,
            ..sudo(Some("501"), Some("20"))
        };
        assert!(handback(&inv, home_of_invoker).is_none());
        assert_eq!(
            cache_path_for(&inv, home_of_invoker).unwrap(),
            todays_path()
        );
    }

    #[test]
    fn plain_root_and_sudo_from_root_keep_todays_behaviour() {
        for user in [None, Some("root")] {
            let inv = Invocation {
                euid: 0,
                sudo_user: user.map(Into::into),
                sudo_uid: Some("0".into()),
                sudo_gid: Some("0".into()),
            };
            assert!(handback(&inv, home_of_invoker).is_none(), "{user:?}");
            assert_eq!(
                cache_path_for(&inv, home_of_invoker).unwrap(),
                todays_path(),
                "{user:?}"
            );
        }
    }

    #[test]
    fn an_unusable_sudo_uid_falls_back_instead_of_panicking() {
        for uid in [None, Some(""), Some("nine hundred"), Some("0")] {
            let inv = sudo(uid, Some("20"));
            assert!(handback(&inv, home_of_invoker).is_none(), "{uid:?}");
            assert_eq!(
                cache_path_for(&inv, home_of_invoker).unwrap(),
                todays_path(),
                "{uid:?}"
            );
        }
    }

    #[test]
    fn a_home_that_does_not_resolve_falls_back() {
        let inv = sudo(Some("501"), Some("20"));
        assert!(handback(&inv, |_| None).is_none());
        assert!(handback(&inv, |_| Some(PathBuf::from("relative/home"))).is_none());
        assert_eq!(cache_path_for(&inv, |_| None).unwrap(), todays_path());
    }

    #[test]
    fn the_handback_carries_the_invoking_users_ids() {
        let hb = handback(&sudo(Some("501"), Some("20")), home_of_invoker).unwrap();
        assert_eq!(hb.uid, 501);
        assert_eq!(hb.gid, Some(20));
        assert_eq!(hb.user, "invoker");
        assert_eq!(hb.home, PathBuf::from("/Users/invoker"));
    }

    #[test]
    fn an_unusable_sudo_gid_still_hands_the_file_back() {
        // The uid is what lets the next unprivileged run replace the file, so
        // a junk group is worth a warning, not a lost handback.
        for gid in [None, Some("staff")] {
            let hb = handback(&sudo(Some("501"), gid), home_of_invoker).unwrap();
            assert_eq!(hb.uid, 501);
            assert_eq!(hb.gid, None, "{gid:?}");
        }
    }

    #[test]
    fn a_write_lands_the_report_where_it_was_asked_to() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wsctl").join("disk-report.json");

        write_cache(&path, &report_at(0), None).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let back: Report = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.schema, SCHEMA);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn the_password_database_finds_a_real_home_and_refuses_a_fake_one() {
        // The one part of the redirect that cannot be faked: if this lookup is
        // wrong, a privileged run quietly falls back instead of redirecting.
        if let Ok(user) = std::env::var("USER") {
            let home = existing_home(&user).expect("the current user has a home");
            assert!(home.is_absolute());
            assert_eq!(
                home.canonicalize().unwrap(),
                dirs::home_dir().unwrap().canonicalize().unwrap()
            );
        }
        assert!(existing_home("no-such-account-on-this-machine").is_none());
        assert!(existing_home("has\0a\0nul").is_none());
    }

    #[test]
    fn a_handback_run_leaves_the_report_and_its_directory_to_the_named_user() {
        // Handing back to the ids this process already has is the most a test
        // without root can do: it exercises the chown, not the privilege drop.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wsctl").join("disk-report.json");
        let hb = Handback {
            user: "invoker".into(),
            home: dir.path().to_path_buf(),
            // SAFETY: neither call can fail or touch memory we pass in.
            uid: unsafe { libc::geteuid() },
            gid: Some(unsafe { libc::getegid() }),
        };

        write_cache(&path, &report_at(0), Some(&hb)).unwrap();

        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&path).unwrap().uid(), hb.uid);
        assert_eq!(
            std::fs::metadata(path.parent().unwrap()).unwrap().uid(),
            hb.uid,
            "a root-owned directory is worse than a root-owned file"
        );
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn every_directory_the_run_creates_is_handed_back() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("Application Support").join("wsctl");
        let path = deep.join("disk-report.json");
        let hb = Handback {
            user: "invoker".into(),
            home: dir.path().to_path_buf(),
            // SAFETY: neither call can fail or touch memory we pass in.
            uid: unsafe { libc::geteuid() },
            gid: Some(unsafe { libc::getegid() }),
        };

        write_cache(&path, &report_at(0), Some(&hb)).unwrap();

        use std::os::unix::fs::MetadataExt;
        for created in [deep.as_path(), deep.parent().unwrap()] {
            assert_eq!(
                std::fs::metadata(created).unwrap().uid(),
                hb.uid,
                "{} was left behind",
                created.display()
            );
        }
    }

    #[test]
    fn a_failed_write_is_reported_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wsctl").join("disk-report.json");
        // A directory sitting where the report goes: the rename cannot win.
        std::fs::create_dir_all(&path).unwrap();

        assert!(write_cache(&path, &report_at(0), None).is_err());
        assert!(
            !path.with_extension("json.tmp").exists(),
            "a leftover temp file is what blocks the next unprivileged write"
        );
    }

    #[test]
    fn a_cache_that_cannot_be_written_is_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, b"not a directory").unwrap();

        let err = write_cache(&blocked.join("wsctl/disk-report.json"), &report_at(0), None)
            .expect_err("a file cannot be a parent directory");
        // save() returns this to callers that only warn, which is the contract
        // load_or_refresh relies on: no cache is slower, never broken.
        tracing::warn!("could not write disk report cache: {err}");
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
