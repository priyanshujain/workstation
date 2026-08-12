//! Find what an app scatters outside its bundle.

use std::path::{Path, PathBuf};
use std::process::Command;

use disk::util::dir_size;

use crate::bundle::AppBundle;
use crate::plist;

/// How sure we are that an item belongs to this app and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Named for the bundle id, or pointing into the bundle. Safe to remove.
    Exact,
    /// Named for the app or its vendor. A vendor folder is shared between all
    /// of that vendor's apps, so this is never removed without being asked for.
    Likely,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Path(PathBuf),
    /// A launchd job: booted out before its plist is deleted.
    LaunchdJob {
        label: String,
        plist: PathBuf,
        domain: String,
    },
    /// An installer receipt, forgotten via `pkgutil --forget`.
    Receipt(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub kind: Kind,
    pub reason: String,
    pub size: u64,
    pub confidence: Confidence,
    pub needs_root: bool,
}

impl Leftover {
    pub fn display(&self) -> String {
        match &self.kind {
            Kind::Path(p) => p.display().to_string(),
            Kind::LaunchdJob { label, .. } => format!("launchd job {label}"),
            Kind::Receipt(id) => format!("pkgutil receipt {id}"),
        }
    }

    /// The file this leftover ultimately deletes, if it is a file at all.
    pub fn target_path(&self) -> Option<&Path> {
        match &self.kind {
            Kind::Path(p) => Some(p),
            Kind::LaunchdJob { plist, .. } => Some(plist),
            Kind::Receipt(_) => None,
        }
    }

    fn path(kind: Kind, reason: impl Into<String>, confidence: Confidence) -> Self {
        let (size, needs_root) = match &kind {
            Kind::Path(p) => (dir_size(p), needs_root(p)),
            Kind::LaunchdJob { plist, .. } => (dir_size(plist), needs_root(plist)),
            Kind::Receipt(_) => (0, true),
        };
        Self {
            kind,
            reason: reason.into(),
            size,
            confidence,
            needs_root,
        }
    }
}

/// Directories searched under each `Library` root.
const LIBRARY_SUBDIRS: &[&str] = &[
    "Application Support",
    "Application Scripts",
    "Autosave Information",
    "Caches",
    "Containers",
    "Cookies",
    "Group Containers",
    "HTTPStorages",
    "Internet Plug-Ins",
    "Logs",
    "Preferences",
    "Preferences/ByHost",
    "PrivilegedHelperTools",
    "Saved Application State",
    "WebKit",
];

/// Where a vendor drops a folder named for itself rather than a bundle id.
const VENDOR_SUBDIRS: &[&str] = &["Application Support", "Caches", "Logs", "Preferences"];

const LAUNCHD_SUBDIRS: &[&str] = &["LaunchAgents", "LaunchDaemons"];

/// Roots to search. Split out so tests can point at a fixture tree.
#[derive(Debug, Clone)]
pub struct Locations {
    pub libraries: Vec<PathBuf>,
    pub bin_dirs: Vec<PathBuf>,
    pub uid: u32,
}

impl Locations {
    pub fn real() -> Self {
        let mut libraries = vec![PathBuf::from("/Library")];
        if let Some(home) = dirs::home_dir() {
            libraries.insert(0, home.join("Library"));
        }
        Self {
            libraries,
            bin_dirs: vec![
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/sbin"),
            ],
            uid: unsafe { libc::geteuid() },
        }
    }
}

pub fn scan(app: &AppBundle) -> Vec<Leftover> {
    scan_in(app, &Locations::real(), true)
}

/// `query_receipts` is off in tests so the suite never shells out to pkgutil.
pub fn scan_in(app: &AppBundle, at: &Locations, query_receipts: bool) -> Vec<Leftover> {
    let mut found = Vec::new();

    if app.path.exists() {
        found.push(Leftover::path(
            Kind::Path(app.path.clone()),
            "the application bundle",
            Confidence::Exact,
        ));
    }

    for library in &at.libraries {
        collect_by_id(app, library, &mut found);
        collect_launchd(app, library, at.uid, &mut found);
    }
    collect_bin_symlinks(app, at, &mut found);
    if query_receipts {
        collect_receipts(app, &mut found);
    }
    // Weak matches last, so an exact hit on the same path wins the dedupe.
    for library in &at.libraries {
        collect_vendor_dirs(app, library, &mut found);
        collect_vendor_namespace(app, library, at.uid, &mut found);
    }

    found.sort_by(|a, b| {
        (a.confidence == Confidence::Likely)
            .cmp(&(b.confidence == Confidence::Likely))
            .then(b.size.cmp(&a.size))
    });
    found
}

fn collect_by_id(app: &AppBundle, library: &Path, out: &mut Vec<Leftover>) {
    let Some(id) = app.bundle_id.as_deref() else {
        return;
    };
    for sub in LIBRARY_SUBDIRS {
        let dir = library.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if matches_id(&name, id) {
                out.push(Leftover::path(
                    Kind::Path(entry.path()),
                    format!("named for bundle id {id}"),
                    Confidence::Exact,
                ));
            }
        }
    }
}

fn collect_vendor_dirs(app: &AppBundle, library: &Path, out: &mut Vec<Leftover>) {
    let vendor = app.vendor();
    let name = app.name.to_lowercase();
    for sub in VENDOR_SUBDIRS {
        let dir = library.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let raw = entry.file_name().to_string_lossy().into_owned();
            let stem = raw.strip_suffix(".plist").unwrap_or(&raw).to_lowercase();
            let reason = if stem == name {
                format!("named for the app '{}'", app.name)
            } else if vendor.as_deref().is_some_and(|v| stem == v) {
                format!(
                    "named for vendor '{}', which its other apps may share",
                    vendor.as_deref().unwrap_or_default()
                )
            } else {
                continue;
            };
            let path = entry.path();
            if already_found(out, &path) {
                continue;
            }
            out.push(Leftover::path(Kind::Path(path), reason, Confidence::Likely));
        }
    }
}

fn already_found(out: &[Leftover], path: &Path) -> bool {
    out.iter().any(|l| l.target_path() == Some(path))
}

fn launchd_domain(sub: &str, uid: u32) -> String {
    if sub == "LaunchDaemons" {
        "system".to_string()
    } else {
        format!("gui/{uid}")
    }
}

fn job(path: &Path, domain: String, reason: &str, confidence: Confidence) -> Leftover {
    let label = plist::read(path)
        .as_ref()
        .and_then(|v| v.get("Label"))
        .and_then(|l| l.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    Leftover::path(
        Kind::LaunchdJob {
            label,
            plist: path.to_path_buf(),
            domain,
        },
        reason.to_string(),
        confidence,
    )
}

/// Helpers a vendor ships alongside the app share its id namespace but are not
/// derived from the bundle id: `com.docker.vmnetd` next to `com.docker.docker`.
/// Real leftovers, but also how one vendor's *other* products look, so weak.
fn collect_vendor_namespace(app: &AppBundle, library: &Path, uid: u32, out: &mut Vec<Leftover>) {
    let Some(prefix) = app.vendor_prefix() else {
        return;
    };
    let reason = format!("in the {prefix} namespace, which the vendor's other apps also use");

    for sub in LIBRARY_SUBDIRS {
        let Ok(entries) = std::fs::read_dir(library.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if matches_id(&name, &prefix) && !already_found(out, &path) {
                out.push(Leftover::path(
                    Kind::Path(path),
                    reason.clone(),
                    Confidence::Likely,
                ));
            }
        }
    }

    for sub in LAUNCHD_SUBDIRS {
        let Ok(entries) = std::fs::read_dir(library.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.extension().is_none_or(|e| e != "plist") {
                continue;
            }
            if matches_id(&name, &prefix) && !already_found(out, &path) {
                out.push(job(
                    &path,
                    launchd_domain(sub, uid),
                    &reason,
                    Confidence::Likely,
                ));
            }
        }
    }
}

fn collect_launchd(app: &AppBundle, library: &Path, uid: u32, out: &mut Vec<Leftover>) {
    for sub in LAUNCHD_SUBDIRS {
        let dir = library.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "plist") {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();

            let by_id = app
                .bundle_id
                .as_deref()
                .is_some_and(|id| matches_id(&name, id));
            let bundle_str = app.path.to_string_lossy().into_owned();
            let by_program = plist::read(&path).as_ref().is_some_and(|v| {
                plist::program_paths(v)
                    .iter()
                    .any(|p| p.starts_with(&bundle_str))
            });
            if !by_id && !by_program {
                continue;
            }

            let reason = if by_program {
                "launchd job running a binary inside the bundle"
            } else {
                "launchd job named for the bundle id"
            };
            out.push(job(
                &path,
                launchd_domain(sub, uid),
                reason,
                Confidence::Exact,
            ));
        }
    }
}

fn collect_bin_symlinks(app: &AppBundle, at: &Locations, out: &mut Vec<Leftover>) {
    let bundle = app.path.to_string_lossy().into_owned();
    for dir in &at.bin_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(target) = std::fs::read_link(&path) else {
                continue;
            };
            if target.to_string_lossy().starts_with(&bundle) {
                out.push(Leftover::path(
                    Kind::Path(path),
                    "symlink into the bundle".to_string(),
                    Confidence::Exact,
                ));
            }
        }
    }
}

fn collect_receipts(app: &AppBundle, out: &mut Vec<Leftover>) {
    let Some(id) = app.bundle_id.as_deref() else {
        return;
    };
    let Ok(output) = Command::new("pkgutil").arg("--pkgs").output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for pkg in String::from_utf8_lossy(&output.stdout).lines() {
        let pkg = pkg.trim();
        if pkg.is_empty() {
            continue;
        }
        if matches_id(pkg, id) || matches_id(id, pkg) {
            out.push(Leftover::path(
                Kind::Receipt(pkg.to_string()),
                format!("installer receipt for {id}"),
                Confidence::Exact,
            ));
        }
    }
}

/// True when `entry` is the bundle id, or a file/folder derived from it.
/// `com.foo.bar` matches `com.foo.bar.plist` and `com.foo.bar.helper`,
/// but never `com.foo.barbaz`.
pub fn matches_id(entry: &str, id: &str) -> bool {
    const STRIPPABLE: &[&str] = &[".plist", ".binarycookies", ".savedState", ".sfl3", ".sfl2"];
    let mut stem = entry;
    for suffix in STRIPPABLE {
        if let Some(s) = stem.strip_suffix(suffix) {
            stem = s;
            break;
        }
    }
    let prefix = format!("{id}.");
    candidate_forms(stem)
        .iter()
        .any(|f| f == id || f.starts_with(&prefix))
}

/// Container names carry a `group.` and/or team-id prefix; peel them off.
fn candidate_forms(stem: &str) -> Vec<String> {
    let mut forms = vec![stem.to_string()];
    let mut cur = stem;
    for _ in 0..2 {
        if let Some(rest) = cur.strip_prefix("group.") {
            cur = rest;
        } else if let Some((first, rest)) = cur.split_once('.')
            && is_team_id(first)
        {
            cur = rest;
        } else {
            break;
        }
        forms.push(cur.to_string());
    }
    forms
}

fn is_team_id(s: &str) -> bool {
    s.len() == 10
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn needs_root(path: &Path) -> bool {
    let parent = path.parent().unwrap_or(path);
    !writable(parent)
}

fn writable(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    const ID: &str = "com.cloudflare.warp.macos";

    #[test]
    fn matches_id_accepts_the_id_itself_and_derived_names() {
        assert!(matches_id(ID, ID));
        assert!(matches_id("com.cloudflare.warp.macos.plist", ID));
        assert!(matches_id("com.cloudflare.warp.macos.daemon", ID));
        assert!(matches_id("com.cloudflare.warp.macos.binarycookies", ID));
        assert!(matches_id("com.cloudflare.warp.macos.savedState", ID));
        assert!(matches_id("com.cloudflare.warp.macos.4C2F.plist", ID));
    }

    #[test]
    fn matches_id_rejects_a_longer_neighbouring_id() {
        assert!(!matches_id("com.cloudflare.warp.macosx", ID));
        assert!(!matches_id("com.cloudflare.warp.macosx.plist", ID));
        assert!(!matches_id("com.cloudflare.warp", ID));
        assert!(!matches_id("com.other.app", ID));
    }

    #[test]
    fn matches_id_peels_group_and_team_prefixes() {
        assert!(matches_id("group.com.cloudflare.warp.macos", ID));
        assert!(matches_id("ABCDE12345.com.cloudflare.warp.macos", ID));
        assert!(matches_id("ABCDE12345.group.com.cloudflare.warp.macos", ID));
        assert!(!matches_id("notateam.com.cloudflare.warp.macos", ID));
    }

    fn touch(path: PathBuf) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();
        path
    }

    fn mkdir(path: PathBuf) -> PathBuf {
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn job_plist(path: &Path, label: &str, program: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{program}</string></array>
</dict></plist>"#
            ),
        )
        .unwrap();
    }

    struct Fixture {
        _guard: TempDir,
        app: AppBundle,
        at: Locations,
    }

    fn fixture() -> Fixture {
        let guard = tempdir().unwrap();
        let root = guard.path();
        let bundle = mkdir(root.join("Applications/Cloudflare WARP.app"));
        let lib = root.join("Library");

        mkdir(lib.join("Application Support").join(ID));
        mkdir(lib.join("Application Support/Cloudflare"));
        mkdir(lib.join("Application Support/com.other.app"));
        mkdir(lib.join("Caches").join(format!("{ID}.helper")));
        mkdir(lib.join("Caches/com.cloudflare.warp.macosx"));
        touch(lib.join("Preferences").join(format!("{ID}.plist")));
        touch(
            lib.join("Preferences/ByHost")
                .join(format!("{ID}.4C2F.plist")),
        );
        mkdir(lib.join("Containers").join(format!("ABCDE12345.{ID}")));
        mkdir(lib.join("Group Containers").join(format!("group.{ID}")));
        touch(lib.join("HTTPStorages").join(format!("{ID}.binarycookies")));

        job_plist(
            &lib.join("LaunchDaemons").join(format!("{ID}.daemon.plist")),
            "com.cloudflare.warp.macos.daemon",
            "/usr/bin/true",
        );
        job_plist(
            &lib.join("LaunchDaemons/com.unrelated.updater.plist"),
            "com.unrelated.updater",
            &format!("{}/Contents/Resources/updater", bundle.display()),
        );
        job_plist(
            &lib.join("LaunchAgents/com.someone.else.plist"),
            "com.someone.else",
            "/usr/bin/true",
        );
        // A helper the vendor ships beside the app: shares the namespace but
        // is not derived from the bundle id, and is not inside the bundle.
        job_plist(
            &lib.join("LaunchDaemons/com.cloudflare.vmnetd.plist"),
            "com.cloudflare.vmnetd",
            "/Library/PrivilegedHelperTools/com.cloudflare.vmnetd",
        );
        touch(lib.join("PrivilegedHelperTools/com.cloudflare.vmnetd"));

        let bin = mkdir(root.join("bin"));
        std::os::unix::fs::symlink(
            bundle.join("Contents/Resources/warp-cli"),
            bin.join("warp-cli"),
        )
        .unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", bin.join("unrelated")).unwrap();

        let app = AppBundle {
            path: bundle,
            name: "Cloudflare WARP".into(),
            bundle_id: Some(ID.into()),
        };
        let at = Locations {
            libraries: vec![lib],
            bin_dirs: vec![bin],
            uid: 501,
        };
        Fixture {
            _guard: guard,
            app,
            at,
        }
    }

    fn paths(found: &[Leftover], confidence: Confidence) -> Vec<String> {
        found
            .iter()
            .filter(|l| l.confidence == confidence)
            .map(|l| l.display())
            .collect()
    }

    fn has(found: &[Leftover], needle: &str) -> bool {
        found.iter().any(|l| l.display().contains(needle))
    }

    #[test]
    fn scan_finds_bundle_id_named_data_everywhere() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let exact = paths(&found, Confidence::Exact);
        for expected in [
            "Cloudflare WARP.app",
            &format!("Application Support/{ID}"),
            &format!("Caches/{ID}.helper"),
            &format!("Preferences/{ID}.plist"),
            &format!("ByHost/{ID}.4C2F.plist"),
            &format!("Containers/ABCDE12345.{ID}"),
            &format!("Group Containers/group.{ID}"),
            &format!("HTTPStorages/{ID}.binarycookies"),
        ] {
            assert!(
                exact.iter().any(|p| p.contains(expected)),
                "missing {expected} in {exact:#?}"
            );
        }
    }

    #[test]
    fn scan_leaves_other_vendors_data_alone() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        assert!(!has(&found, "com.other.app"));
        assert!(!has(&found, "bin/unrelated"));
        assert!(!has(&found, "com.someone.else"));
    }

    #[test]
    fn scan_never_marks_a_neighbouring_id_as_exact() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let neighbour = found
            .iter()
            .find(|l| l.display().contains("com.cloudflare.warp.macosx"))
            .expect("same-namespace data should still be surfaced");
        assert_eq!(
            neighbour.confidence,
            Confidence::Likely,
            "a longer neighbouring id is never this app's data"
        );
    }

    #[test]
    fn scan_classifies_vendor_folders_as_likely() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let vendor = found
            .iter()
            .find(|l| l.display().ends_with("Application Support/Cloudflare"))
            .expect("vendor folder should be found");
        assert_eq!(vendor.confidence, Confidence::Likely);
        assert!(vendor.reason.contains("vendor"), "got: {}", vendor.reason);
    }

    #[test]
    fn scan_surfaces_vendor_helpers_as_likely_not_exact() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);

        let daemon = found
            .iter()
            .find(|l| matches!(&l.kind, Kind::LaunchdJob { label, .. } if label == "com.cloudflare.vmnetd"))
            .expect("a helper daemon in the vendor namespace should be found");
        assert_eq!(
            daemon.confidence,
            Confidence::Likely,
            "a sibling of the bundle id is never assumed to be this app's"
        );

        let helper = found
            .iter()
            .find(|l| {
                l.display()
                    .contains("PrivilegedHelperTools/com.cloudflare.vmnetd")
            })
            .expect("its privileged helper binary should be found too");
        assert_eq!(helper.confidence, Confidence::Likely);
    }

    #[test]
    fn exact_matches_are_not_downgraded_by_the_namespace_pass() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let support = found
            .iter()
            .filter(|l| l.display().ends_with(&format!("Application Support/{ID}")))
            .collect::<Vec<_>>();
        assert_eq!(support.len(), 1, "listed once, not once per pass");
        assert_eq!(support[0].confidence, Confidence::Exact);
    }

    #[test]
    fn scan_finds_launchd_jobs_by_id_and_by_program_path() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let jobs: Vec<_> = found
            .iter()
            .filter(|l| l.confidence == Confidence::Exact)
            .filter_map(|l| match &l.kind {
                Kind::LaunchdJob { label, domain, .. } => Some((label.clone(), domain.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(jobs.len(), 2, "got {jobs:#?}");
        assert!(jobs.contains(&("com.cloudflare.warp.macos.daemon".into(), "system".into())));
        assert!(
            jobs.contains(&("com.unrelated.updater".into(), "system".into())),
            "a job whose binary lives in the bundle counts, whatever it is called"
        );
    }

    #[test]
    fn scan_finds_symlinks_pointing_into_the_bundle() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        assert!(has(&found, "bin/warp-cli"));
        assert!(!has(&found, "bin/unrelated"));
    }

    #[test]
    fn scan_sorts_exact_before_likely() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        let first_likely = found
            .iter()
            .position(|l| l.confidence == Confidence::Likely)
            .unwrap();
        assert!(
            found[first_likely..]
                .iter()
                .all(|l| l.confidence == Confidence::Likely)
        );
    }

    #[test]
    fn scan_without_a_bundle_id_still_finds_bundle_and_symlinks() {
        let mut f = fixture();
        f.app.bundle_id = None;
        let found = scan_in(&f.app, &f.at, false);
        assert!(has(&found, "Cloudflare WARP.app"));
        assert!(has(&found, "bin/warp-cli"));
        assert!(!has(&found, "Application Support/com.cloudflare"));
    }

    #[test]
    fn scan_skips_a_bundle_that_is_already_deleted() {
        let f = fixture();
        fs::remove_dir_all(&f.app.path).unwrap();
        let found = scan_in(&f.app, &f.at, false);
        assert!(!has(&found, "Cloudflare WARP.app"));
        assert!(has(&found, &format!("Application Support/{ID}")));
    }

    #[test]
    fn tempdir_items_do_not_need_root() {
        let f = fixture();
        let found = scan_in(&f.app, &f.at, false);
        assert!(
            found
                .iter()
                .filter(|l| !matches!(l.kind, Kind::Receipt(_)))
                .all(|l| !l.needs_root)
        );
    }
}
