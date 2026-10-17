use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::key::DisplayKey;

/// Which panel should hold the menu bar whenever it is connected.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct Prefs {
    #[serde(default)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_empty_prefs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("display.json");
        assert_eq!(load_from(&path).unwrap(), Prefs::default());
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
