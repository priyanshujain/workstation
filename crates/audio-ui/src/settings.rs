//! Remembers which hardware was picked and how the microphone is treated.
//!
//! Names change and are not unique, so nothing but the UID is stored for a
//! device; the menu looks the name up again from the live device list on every
//! open. Every field is optional, and a file written before a field existed
//! reads back as nobody having chosen yet rather than as an error.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub input_uid: Option<String>,
    pub output_uid: Option<String>,
    pub denoise: Option<bool>,
    pub voice_threshold: Option<f32>,
}

impl Settings {
    pub fn load() -> Self {
        match path() {
            Ok(path) => Self::read(&path),
            Err(e) => {
                tracing::warn!("no settings file: {e:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        self.write(&path()?)
    }

    /// Anything unreadable is treated as no choice yet. A menu that quietly
    /// forgets is better than one that refuses to open.
    fn read(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable {}: {e}", path.display());
            Self::default()
        })
    }

    fn write(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, body).with_context(|| format!("could not write {}", path.display()))
    }
}

fn path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no Application Support directory")?
        .join("wsctl");
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(dir.join("audio-ui.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn settings() -> Settings {
        Settings {
            input_uid: Some("AppleUSBAudioEngine:Insta360:Insta360 Link 2C:111000:3".into()),
            output_uid: Some("30-21-39-26-64-C7:output".into()),
            denoise: Some(true),
            voice_threshold: Some(0.8),
        }
    }

    #[test]
    fn survives_a_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-ui.json");
        settings().write(&path).unwrap();
        assert_eq!(Settings::read(&path), settings());
    }

    #[test]
    fn a_missing_file_means_nothing_is_chosen() {
        let dir = tempdir().unwrap();
        assert_eq!(
            Settings::read(&dir.path().join("audio-ui.json")),
            Settings::default()
        );
    }

    #[test]
    fn a_corrupt_file_means_nothing_is_chosen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-ui.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Settings::read(&path), Settings::default());
    }

    #[test]
    fn a_half_filled_file_keeps_the_half_that_is_there() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-ui.json");
        std::fs::write(&path, br#"{"input_uid":"BuiltInMicrophoneDevice"}"#).unwrap();
        let loaded = Settings::read(&path);
        assert_eq!(loaded.input_uid.as_deref(), Some("BuiltInMicrophoneDevice"));
        assert_eq!(loaded.output_uid, None);
    }

    // Written by a version that had no suppressor to remember anything about.
    #[test]
    fn a_file_from_before_the_suppressor_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-ui.json");
        std::fs::write(
            &path,
            br#"{"input_uid":"BuiltInMicrophoneDevice","output_uid":"BuiltInSpeakerDevice"}"#,
        )
        .unwrap();
        let loaded = Settings::read(&path);
        assert_eq!(loaded.output_uid.as_deref(), Some("BuiltInSpeakerDevice"));
        assert_eq!(loaded.denoise, None);
        assert_eq!(loaded.voice_threshold, None);
    }

    #[test]
    fn the_gate_is_off_until_somebody_turns_it_on() {
        let settings = Settings::default();
        assert!(!settings.denoise.unwrap_or(false));
        assert_eq!(settings.voice_threshold.unwrap_or(0.0), 0.0);
    }
}
