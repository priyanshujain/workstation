use std::path::{Path, PathBuf};

/// Whether this process holds Full Disk Access. The probe opens a directory
/// only Full Disk Access opens. macOS never asks about that grant, so without
/// it the open fails with EPERM and no dialog appears.
pub fn has_full_disk_access() -> bool {
    dirs::home_dir().is_some_and(|home| holds_full_disk_access(&home))
}

fn holds_full_disk_access(home: &Path) -> bool {
    std::fs::read_dir(home.join("Library/Application Support/com.apple.TCC")).is_ok()
}

/// What every walk leaves unopened: nothing with Full Disk Access, every
/// consent-gated directory without it.
pub fn closed_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    closed_paths_in(&home, holds_full_disk_access(&home))
}

fn closed_paths_in(home: &Path, full_disk_access: bool) -> Vec<PathBuf> {
    if full_disk_access {
        Vec::new()
    } else {
        consent_gated_paths_in(home)
    }
}

/// Directories macOS asks the user about before it lets a process open them.
///
/// The grant that covers all of these at once is Full Disk Access. Without it,
/// the first open of each directory here puts a dialog on the screen and
/// blocks until somebody answers it. Everything else macOS protects refuses
/// silently with EPERM and needs no list: the walk simply records the refusal.
fn consent_gated_paths_in(home: &Path) -> Vec<PathBuf> {
    let folders = [
        "Desktop",
        "Documents",
        "Downloads",
        // iCloud Drive, and the Google Drive, Dropbox and OneDrive mounts.
        "Library/Mobile Documents",
        "Library/CloudStorage",
        // Other apps' data, asked about since macOS 15.
        "Library/Containers",
        "Library/Group Containers",
        // Contacts, Calendars and Reminders, asked about as those, not as folders.
        "Library/Application Support/AddressBook",
        "Library/Calendars",
        "Library/Reminders",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
        "Library/Metadata",
        "Library/Suggestions",
        "Library/HomeKit",
        "Library/Biome",
        "Library/Autosave Information",
        // The Photos and media libraries are asked about as Photos and as
        // Media Library. Their bundle names vary by version and by what is set
        // up, so the folders holding them stay closed as a whole.
        "Pictures",
        "Music",
        "Movies",
        // The Shared with You library, a photo library outside ~/Pictures.
        "Library/Photos",
    ];
    let mut paths: Vec<PathBuf> = folders.iter().map(|p| home.join(p)).collect();
    paths.extend(children_named(
        &home.join("Library/Application Support"),
        &["com.apple."],
    ));
    paths.extend(media_agent_dirs(home));
    paths
}

/// The Music and TV apps keep working state under `~/Library/Caches`, and
/// opening one of those bundle directories raises the Media Library dialog
/// even though it is nowhere near `~/Music`. The bundle names drift between
/// releases and grow new siblings, so rather than list them this reads the
/// parent, which is itself never gated, and closes every child whose name
/// starts a media agent's bundle id. Every `com.apple.*` under Application
/// Support is closed the same way, without the prefix list.
fn media_agent_dirs(home: &Path) -> Vec<PathBuf> {
    const PREFIXES: [&str; 8] = [
        "com.apple.Music",
        "com.apple.TV",
        "com.apple.iTunes",
        "com.apple.AMP",
        "com.apple.MediaPlaybackCore",
        "com.apple.MediaPlayer",
        "com.apple.AppleMediaServices",
        "com.apple.mediaanalysisd",
    ];
    children_named(&home.join("Library/Caches"), &PREFIXES)
}

fn children_named(parent: &Path, prefixes: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            prefixes.iter().any(|p| name.starts_with(p))
        })
        .map(|entry| entry.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_consent_gated_path_is_absolute_and_inside_home() {
        // These are compared to walked paths by equality, so a relative one
        // would never match and its dialog would come straight back.
        let home = tempfile::tempdir().unwrap();
        let paths = consent_gated_paths_in(home.path());
        assert!(!paths.is_empty());
        for path in &paths {
            assert!(path.is_absolute(), "{}", path.display());
            assert!(path.starts_with(home.path()), "{}", path.display());
        }
    }

    #[test]
    fn full_disk_access_opens_everything_and_its_absence_closes_the_gated_paths() {
        let home = tempfile::tempdir().unwrap();
        assert!(closed_paths_in(home.path(), true).is_empty());
        assert_eq!(
            closed_paths_in(home.path(), false),
            consent_gated_paths_in(home.path())
        );
    }

    #[test]
    fn the_probe_reads_the_tcc_directory_and_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        assert!(!holds_full_disk_access(home.path()));
        std::fs::create_dir_all(
            home.path()
                .join("Library/Application Support/com.apple.TCC"),
        )
        .unwrap();
        assert!(holds_full_disk_access(home.path()));
    }

    #[test]
    fn every_apple_directory_under_application_support_is_closed() {
        let home = tempfile::tempdir().unwrap();
        let support = home.path().join("Library/Application Support");
        for name in ["com.apple.sharedfilelist", "com.apple.NewThing", "Code"] {
            std::fs::create_dir_all(support.join(name)).unwrap();
        }

        let closed = consent_gated_paths_in(home.path());
        assert!(closed.contains(&support.join("com.apple.sharedfilelist")));
        assert!(closed.contains(&support.join("com.apple.NewThing")));
        assert!(!closed.contains(&support.join("Code")), "{closed:?}");
    }

    #[test]
    fn a_media_agent_cache_directory_is_closed_by_prefix() {
        // ~/Library/Caches/com.apple.Music raises the Media Library dialog and
        // is nowhere near ~/Music, so it has to be caught here or the prompt
        // comes back on the next scan.
        let home = tempfile::tempdir().unwrap();
        let caches = home.path().join("Library/Caches");
        std::fs::create_dir_all(&caches).unwrap();
        for name in [
            "com.apple.Music",
            "com.apple.AMPLibraryAgent",
            "com.example.app",
        ] {
            std::fs::create_dir(caches.join(name)).unwrap();
        }

        let closed = media_agent_dirs(home.path());
        assert!(
            closed.contains(&caches.join("com.apple.Music")),
            "{closed:?}"
        );
        assert!(
            closed.contains(&caches.join("com.apple.AMPLibraryAgent")),
            "{closed:?}"
        );
        assert!(
            !closed.contains(&caches.join("com.example.app")),
            "a third-party cache is not media and must stay open: {closed:?}"
        );
    }

    #[test]
    fn the_folders_everyone_is_asked_about_are_listed() {
        let home = tempfile::tempdir().unwrap();
        let paths = consent_gated_paths_in(home.path());
        for folder in [
            "Desktop",
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Movies",
            "Library/Containers",
            "Library/Group Containers",
            "Library/Mail",
            "Library/Messages",
            "Library/Safari",
            "Library/Metadata",
            "Library/Suggestions",
            "Library/HomeKit",
            "Library/Biome",
            "Library/Autosave Information",
        ] {
            assert!(
                paths.contains(&home.path().join(folder)),
                "{folder} missing"
            );
        }
    }
}
