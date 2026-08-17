use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::key::DisplayKey;

/// Which panel should hold the menu bar whenever it is connected.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_main: Option<DisplayKey>,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/wsctl/display.json")
}

pub fn load() -> Result<Prefs> {
    load_from(&config_path())
}

pub fn save(prefs: &Prefs) -> Result<()> {
    save_to(&config_path(), prefs)
}

pub fn load_from(path: &Path) -> Result<Prefs> {
    if !path.exists() {
        return Ok(Prefs::default());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Prefs::default());
    }
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_to(path: &Path, prefs: &Prefs) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(prefs)?;
    std::fs::write(path, format!("{raw}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR_1: DisplayKey = DisplayKey {
        vendor: 4268,
        model: 53466,
        serial: 911690818,
    };

    #[test]
    fn missing_file_yields_empty_prefs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.json");
        assert_eq!(load_from(&path).unwrap(), Prefs::default());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/display.json");
        let prefs = Prefs {
            preferred_main: Some(MONITOR_1),
        };

        save_to(&path, &prefs).unwrap();

        assert_eq!(load_from(&path).unwrap(), prefs);
    }

    #[test]
    fn creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/display.json");
        save_to(&path, &Prefs::default()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn empty_file_yields_empty_prefs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.json");
        std::fs::write(&path, "   \n").unwrap();
        assert_eq!(load_from(&path).unwrap(), Prefs::default());
    }

    #[test]
    fn corrupt_file_is_an_error_rather_than_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_err());
    }

    #[test]
    fn config_path_is_under_home() {
        let path = config_path();
        assert!(path.ends_with(".config/wsctl/display.json"));
    }
}
