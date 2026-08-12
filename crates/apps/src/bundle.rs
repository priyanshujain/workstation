//! Locate an installed `.app` and read the identity macOS files things under.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::plist;

/// An application bundle on disk, plus whatever identity it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppBundle {
    pub path: PathBuf,
    /// Bundle filename with `.app` stripped, e.g. `Cloudflare WARP`.
    pub name: String,
    /// `CFBundleIdentifier`, absent for bundles with a broken Info.plist.
    pub bundle_id: Option<String>,
}

impl AppBundle {
    pub fn read(path: &Path) -> Self {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bundle_id = plist::string_at(&path.join("Contents/Info.plist"), "CFBundleIdentifier");
        Self {
            path: path.to_path_buf(),
            name,
            bundle_id,
        }
    }

    /// First two components of the bundle id: `com.docker.docker` gives
    /// `com.docker`, the namespace its helper daemons also sit in.
    pub fn vendor_prefix(&self) -> Option<String> {
        let id = self.bundle_id.as_ref()?;
        let mut parts = id.split('.');
        let first = parts.next()?;
        let second = parts.next()?;
        parts.next()?;
        Some(format!("{first}.{second}"))
    }

    /// Vendor token from the bundle id: `com.cloudflare.warp` gives `cloudflare`.
    /// Shared by every app from one vendor, so only ever a weak signal.
    pub fn vendor(&self) -> Option<String> {
        let id = self.bundle_id.as_ref()?;
        let parts: Vec<&str> = id.split('.').collect();
        if parts.len() < 3 {
            return None;
        }
        let vendor = parts[1];
        (vendor.len() > 2).then(|| vendor.to_string())
    }
}

/// Where user-installable apps live. `/System/Applications` is deliberately
/// absent: those are Apple's, SIP-protected, and must never be touched.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// Bundles in `dirs` matching `query`. An exact stem match wins outright;
/// otherwise every substring match is returned for the caller to disambiguate.
pub fn find_in(dirs: &[PathBuf], query: &str) -> Vec<PathBuf> {
    let needle = query.trim().trim_end_matches(".app").to_lowercase();
    let mut exact = Vec::new();
    let mut partial = Vec::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "app") {
                continue;
            }
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else {
                continue;
            };
            if stem == needle {
                exact.push(path);
            } else if stem.contains(&needle) {
                partial.push(path);
            }
        }
    }

    exact.sort();
    partial.sort();
    if exact.is_empty() { partial } else { exact }
}

/// Resolve `query` to exactly one bundle, or explain why it could not.
pub fn find(query: &str) -> Result<AppBundle> {
    let direct = Path::new(query);
    if direct.is_absolute() && direct.extension().is_some_and(|e| e == "app") {
        if !direct.exists() {
            bail!("no bundle at {}", direct.display());
        }
        return Ok(AppBundle::read(direct));
    }

    let matches = find_in(&search_dirs(), query);
    match matches.len() {
        0 => bail!(
            "no application matching '{query}' in /Applications or ~/Applications.\n\
             If it is already deleted, its leftovers are still findable by bundle id."
        ),
        1 => Ok(AppBundle::read(&matches[0])),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|p| {
                    p.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            bail!(
                "'{query}' matches {} apps: {}",
                names.len(),
                names.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    fn make_app(dir: &Path, name: &str, bundle_id: Option<&str>) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        fs::create_dir_all(app.join("Contents")).unwrap();
        let body = match bundle_id {
            Some(id) => format!("<key>CFBundleIdentifier</key><string>{id}</string>"),
            None => "<key>CFBundleName</key><string>x</string>".to_string(),
        };
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>{body}</dict></plist>"#
            ),
        )
        .unwrap();
        app
    }

    fn fixture() -> (TempDir, Vec<PathBuf>) {
        let dir = tempdir().unwrap();
        make_app(
            dir.path(),
            "Cloudflare WARP",
            Some("com.cloudflare.warp.macos"),
        );
        make_app(dir.path(), "Slack", Some("com.tinyspeck.slackmacgap"));
        make_app(
            dir.path(),
            "Slack Dev",
            Some("com.tinyspeck.slackmacgap.dev"),
        );
        let dirs = vec![dir.path().to_path_buf()];
        (dir, dirs)
    }

    #[test]
    fn read_extracts_name_and_bundle_id() {
        let dir = tempdir().unwrap();
        let app = make_app(
            dir.path(),
            "Cloudflare WARP",
            Some("com.cloudflare.warp.macos"),
        );
        let bundle = AppBundle::read(&app);
        assert_eq!(bundle.name, "Cloudflare WARP");
        assert_eq!(
            bundle.bundle_id.as_deref(),
            Some("com.cloudflare.warp.macos")
        );
    }

    #[test]
    fn read_tolerates_missing_bundle_id() {
        let dir = tempdir().unwrap();
        let app = make_app(dir.path(), "Broken", None);
        assert_eq!(AppBundle::read(&app).bundle_id, None);
    }

    #[test]
    fn vendor_is_the_second_component() {
        let dir = tempdir().unwrap();
        let app = make_app(dir.path(), "A", Some("com.cloudflare.warp.macos"));
        assert_eq!(
            AppBundle::read(&app).vendor().as_deref(),
            Some("cloudflare")
        );
    }

    #[test]
    fn vendor_prefix_is_the_first_two_components() {
        let dir = tempdir().unwrap();
        let app = AppBundle::read(&make_app(dir.path(), "A", Some("com.docker.docker")));
        assert_eq!(app.vendor_prefix().as_deref(), Some("com.docker"));
    }

    #[test]
    fn vendor_prefix_needs_at_least_three_components() {
        let dir = tempdir().unwrap();
        let app = AppBundle::read(&make_app(dir.path(), "B", Some("com.slack")));
        assert_eq!(app.vendor_prefix(), None);
        let none = AppBundle::read(&make_app(dir.path(), "C", None));
        assert_eq!(none.vendor_prefix(), None);
    }

    #[test]
    fn vendor_rejects_short_or_shallow_ids() {
        let dir = tempdir().unwrap();
        assert_eq!(
            AppBundle::read(&make_app(dir.path(), "B", Some("com.slack"))).vendor(),
            None
        );
        assert_eq!(
            AppBundle::read(&make_app(dir.path(), "C", Some("com.hi.app"))).vendor(),
            None
        );
        assert_eq!(
            AppBundle::read(&make_app(dir.path(), "D", None)).vendor(),
            None
        );
    }

    #[test]
    fn find_in_prefers_exact_over_substring() {
        let (_guard, dirs) = fixture();
        let hits = find_in(&dirs, "Slack");
        assert_eq!(hits.len(), 1, "exact match should not drag in 'Slack Dev'");
        assert!(hits[0].ends_with("Slack.app"));
    }

    #[test]
    fn find_in_is_case_insensitive_and_ignores_dot_app() {
        let (_guard, dirs) = fixture();
        assert_eq!(find_in(&dirs, "sLaCk.app").len(), 1);
    }

    #[test]
    fn find_in_returns_every_substring_match() {
        let (_guard, dirs) = fixture();
        let hits = find_in(&dirs, "slac");
        assert_eq!(hits.len(), 2, "both Slack apps should be offered");
    }

    #[test]
    fn find_in_skips_non_app_entries_and_missing_dirs() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("Slack")).unwrap();
        fs::write(dir.path().join("Slack.txt"), b"x").unwrap();
        let dirs = vec![dir.path().to_path_buf(), PathBuf::from("/no/such/dir")];
        assert!(find_in(&dirs, "Slack").is_empty());
    }

    #[test]
    fn search_dirs_excludes_system_applications() {
        let dirs = search_dirs();
        assert!(!dirs.iter().any(|d| d.starts_with("/System")));
        assert!(dirs.contains(&PathBuf::from("/Applications")));
    }

    #[test]
    fn find_rejects_an_absolute_path_that_does_not_exist() {
        let err = find("/Applications/Definitely Not Here.app").unwrap_err();
        assert!(format!("{err}").contains("no bundle at"));
    }
}
