use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    App(String),
    Removed,
    Unresolved,
}

#[derive(Debug, Clone, Default)]
pub struct InstalledApps {
    names: HashMap<String, String>,
    vendors: Vec<String>,
    commands: HashSet<String>,
}

impl InstalledApps {
    pub fn scan() -> InstalledApps {
        let mut dirs = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
        ];
        dirs.extend(dirs::home_dir().map(|home| home.join("Applications")));
        let mut apps = InstalledApps::from_bundles(dirs.iter().flat_map(|dir| app_bundles(dir, 2)));
        for dir in std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                apps.add_command(&entry.file_name().to_string_lossy());
            }
        }
        apps
    }

    pub fn add_command(&mut self, name: &str) {
        self.commands.insert(name.to_lowercase());
    }

    pub fn from_bundles(bundles: impl IntoIterator<Item = PathBuf>) -> InstalledApps {
        let mut apps = InstalledApps::default();
        for bundle in bundles {
            if let Some((id, name)) = identity(&bundle) {
                apps.add(&id, &name);
            }
        }
        apps
    }

    pub fn add(&mut self, bundle_id: &str, name: &str) {
        let id = bundle_id.to_lowercase();
        if let Some(vendor) = vendor(&id) {
            self.vendors.push(vendor);
        }
        self.names.insert(id, name.to_string());
    }

    pub fn resolve(&self, folder: &str) -> Owner {
        let Some(id) = bundle_id_in(folder) else {
            return Owner::Unresolved;
        };
        if let Some(name) = self.app_for(&id) {
            return Owner::App(name.clone());
        }
        let shared_vendor = vendor(&id).is_some_and(|v| self.vendors.contains(&v));
        let names_a_command = id
            .split('.')
            .skip(1)
            .any(|part| self.commands.contains(part));
        if id.starts_with("com.apple.") || shared_vendor || names_a_command {
            Owner::Unresolved
        } else {
            Owner::Removed
        }
    }

    pub fn has_app(&self, name: &str) -> bool {
        self.names.values().any(|app| app == name)
    }

    fn app_for(&self, id: &str) -> Option<&String> {
        let mut prefix = id;
        loop {
            if let Some(name) = self.names.get(prefix) {
                return Some(name);
            }
            prefix = &prefix[..prefix.rfind('.')?];
        }
    }
}

fn bundle_id_in(folder: &str) -> Option<String> {
    let name = folder.strip_suffix(".savedState").unwrap_or(folder);
    let name = name.strip_prefix("group.").unwrap_or(name);
    let parts: Vec<&str> = name.split('.').collect();
    let looks_like_id = parts.len() >= 3
        && parts.iter().all(|p| !p.is_empty())
        && parts[0].chars().all(|c| c.is_ascii_lowercase());
    looks_like_id.then(|| name.to_lowercase())
}

fn vendor(id: &str) -> Option<String> {
    let mut parts = id.split('.');
    let first = parts.next()?;
    let second = parts.next()?;
    parts.next()?;
    Some(format!("{first}.{second}"))
}

fn app_bundles(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "app") {
            found.push(path);
        } else if depth > 1
            && entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || t.is_symlink())
        {
            found.extend(app_bundles(&path, depth - 1));
        }
    }
    found
}

fn identity(bundle: &Path) -> Option<(String, String)> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let info = info.as_dictionary()?;
    let id = info.get("CFBundleIdentifier")?.as_string()?.to_string();
    let name = bundle.file_stem()?.to_string_lossy().into_owned();
    Some((id, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn installed() -> InstalledApps {
        let mut apps = InstalledApps::default();
        apps.add("com.example.Editor", "Editor");
        apps.add("io.vendor.Tool", "Tool");
        apps
    }

    #[test]
    fn a_folder_named_by_an_installed_bundle_id_belongs_to_that_app() {
        let apps = installed();
        assert_eq!(
            apps.resolve("com.example.Editor"),
            Owner::App("Editor".into())
        );
        assert_eq!(
            apps.resolve("com.example.editor.helper"),
            Owner::App("Editor".into())
        );
        assert_eq!(
            apps.resolve("com.example.Editor.savedState"),
            Owner::App("Editor".into())
        );
    }

    #[test]
    fn a_bundle_id_no_installed_app_shares_is_left_by_a_removed_app() {
        assert_eq!(installed().resolve("net.gone.Player"), Owner::Removed);
        assert_eq!(installed().resolve("group.net.gone.Player"), Owner::Removed);
    }

    #[test]
    fn a_vendor_with_an_installed_app_or_apple_is_never_called_removed() {
        assert_eq!(installed().resolve("io.vendor.Updater"), Owner::Unresolved);
        assert_eq!(installed().resolve("com.apple.Safari"), Owner::Unresolved);
    }

    #[test]
    fn a_bundle_id_naming_a_command_on_the_path_belongs_to_that_command() {
        let mut apps = installed();
        apps.add_command("zellij");
        assert_eq!(
            apps.resolve("org.Zellij-Contributors.Zellij"),
            Owner::Unresolved
        );
    }

    #[test]
    fn names_that_are_not_bundle_ids_are_unresolved() {
        for name in ["pip", "Google", "JetBrains", "TEAMID.example", "Code.app"] {
            assert_eq!(installed().resolve(name), Owner::Unresolved, "{name}");
        }
    }

    #[test]
    fn bundles_are_read_from_their_info_plist() {
        let dir = tempfile::tempdir().unwrap();
        let contents = dir.path().join("Nested/Example.app/Contents");
        fs::create_dir_all(&contents).unwrap();
        fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.example.Thing</string></dict></plist>"#,
        )
        .unwrap();

        let apps = InstalledApps::from_bundles(app_bundles(dir.path(), 2));

        assert_eq!(
            apps.resolve("org.example.Thing"),
            Owner::App("Example".into())
        );
    }
}
