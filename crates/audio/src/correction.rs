//! What calibration measured, remembered so the bridge has somewhere to start.
//!
//! This file used to hold the delay the canceller worked to. It does not any
//! more, and the demotion is the point of it: the Insta360 Link 2C draws a
//! different amount of internal buffering every time its stream is opened, six
//! opens of one pair measuring 298, 300, 278, 286, 276 and 293 ms. A number
//! written down here is therefore describing a machine state that ended when
//! the device was next opened. aec3 finds the echo for itself now, and what is
//! stored here is only where it starts looking. See `aec.rs`.
//!
//! That makes a stale entry harmless rather than dangerous, which is worth
//! saying plainly because it was the opposite before: measured on synthetic
//! audio, a hint 240 ms out from the real echo produces exactly the same
//! result as no hint at all, because the estimator overrides it either way.
//! What a stale entry can still do is mislead somebody reading the status
//! line, so [`Correction::is_stale`] exists and the bridge says so out loud.
//!
//! A correction only means anything for the pair of devices it was measured
//! against: a Bluetooth speaker's 200 ms has nothing to say about the built-in
//! ones, and applying it to them would be worse than never having measured
//! anything. So every entry is keyed by both UIDs and a lookup that does not
//! match both exactly finds nothing.
//!
//! Nothing here is allowed to stop the bridge. A file that is missing, corrupt,
//! or written by a version that arranged things differently reads back as
//! "nobody has measured this pair", which is where the bridge started.

use std::path::{Path, PathBuf};

/// How long a measurement is worth quoting for. Nothing breaks when one goes
/// past this: it is the point at which the bridge stops implying the number
/// still describes the hardware in front of it, because over a month the dock,
/// the cable or the device itself may not be the ones that were measured.
pub const STALE_AFTER_SECS: u64 = 30 * 24 * 60 * 60;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Corrections {
    #[serde(default)]
    pairs: Vec<Correction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    pub output_uid: String,
    pub input_uid: String,
    /// The round trip that was actually measured, speaker IO proc to
    /// microphone IO proc, in milliseconds.
    pub measured_ms: f64,
    /// What the bridge worked out for the same pair from what Core Audio
    /// reported, at the time of the measurement.
    pub computed_ms: f64,
    /// How far the correlation peak stood above the next highest one.
    #[serde(default)]
    pub confidence: f64,
    /// Unix seconds, so a stale measurement can be seen for what it is. Absent
    /// in a file written before this was recorded.
    #[serde(default)]
    pub measured_at: Option<u64>,
}

impl Correction {
    /// What to add to the delay the bridge computes. Negative means Core Audio
    /// was claiming more latency than the room actually has.
    pub fn offset_ms(&self) -> f64 {
        self.measured_ms - self.computed_ms
    }

    /// How long ago this was measured, or `None` when the entry predates the
    /// date being recorded and its age cannot be known.
    pub fn age_secs(&self, now: u64) -> Option<u64> {
        self.measured_at.map(|at| now.saturating_sub(at))
    }

    /// Whether this is too old to be quoted as describing the hardware now. An
    /// entry with no date is stale by the same argument: the field exists so
    /// that age can be seen, and one written before it cannot be vouched for.
    pub fn is_stale(&self, now: u64) -> bool {
        self.age_secs(now).is_none_or(|age| age > STALE_AFTER_SECS)
    }
}

impl Corrections {
    pub fn load() -> Self {
        match path() {
            Ok(path) => Self::read(&path),
            Err(e) => {
                tracing::warn!("no calibration file: {e:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        self.write(&path()?)
    }

    /// The correction measured for exactly this pair, in this direction.
    pub fn find(&self, output_uid: &str, input_uid: &str) -> Option<&Correction> {
        self.pairs
            .iter()
            .find(|c| c.output_uid == output_uid && c.input_uid == input_uid)
    }

    /// Remembers a measurement, replacing whatever was known about that pair.
    /// Only the newest matters: the point of measuring again is that the room
    /// or the hardware moved.
    pub fn record(&mut self, correction: Correction) {
        self.pairs.retain(|c| {
            c.output_uid != correction.output_uid || c.input_uid != correction.input_uid
        });
        self.pairs.push(correction);
    }

    /// Anything unreadable is treated as nothing having been measured. A bridge
    /// that quietly falls back to what Core Audio reports is better than one
    /// that refuses to start.
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

/// Unix seconds now, or `None` if the clock is somewhere before 1970.
pub fn now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no Application Support directory")?
        .join("wsctl");
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(dir.join("audio-calibration.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const PHILIPS: &str = "30-21-39-26-64-C7:output";
    const INSTA: &str = "AppleUSBAudioEngine:Insta360:Insta360 Link 2C:111000:3";

    fn correction(output_uid: &str, measured_ms: f64) -> Correction {
        Correction {
            output_uid: output_uid.into(),
            input_uid: INSTA.into(),
            measured_ms,
            computed_ms: 42.0,
            confidence: 18.5,
            measured_at: Some(1_755_300_000),
        }
    }

    fn saved(corrections: &Corrections) -> Corrections {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-calibration.json");
        corrections.write(&path).unwrap();
        Corrections::read(&path)
    }

    #[test]
    fn survives_a_round_trip() {
        let mut corrections = Corrections::default();
        corrections.record(correction(PHILIPS, 214.0));
        corrections.record(correction("BuiltInSpeakerDevice", 41.0));
        assert_eq!(saved(&corrections), corrections);
    }

    // The whole reason the file is keyed by both UIDs. A Bluetooth speaker's
    // correction applied to the built-in ones would be a worse delay than
    // never having measured anything.
    #[test]
    fn a_correction_for_one_pair_is_not_applied_to_another() {
        let mut corrections = Corrections::default();
        corrections.record(correction(PHILIPS, 214.0));

        assert_eq!(
            corrections.find(PHILIPS, INSTA).map(Correction::offset_ms),
            Some(172.0)
        );
        assert!(corrections.find("BuiltInSpeakerDevice", INSTA).is_none());
        assert!(
            corrections
                .find(PHILIPS, "BuiltInMicrophoneDevice")
                .is_none()
        );
        // The pair is directional: a speaker UID in the microphone slot is a
        // different pair, not the same one backwards.
        assert!(corrections.find(INSTA, PHILIPS).is_none());
    }

    #[test]
    fn measuring_again_replaces_what_was_known() {
        let mut corrections = Corrections::default();
        corrections.record(correction(PHILIPS, 214.0));
        corrections.record(correction(PHILIPS, 168.0));
        assert_eq!(corrections.pairs.len(), 1);
        assert_eq!(
            corrections.find(PHILIPS, INSTA).map(|c| c.measured_ms),
            Some(168.0)
        );
    }

    #[test]
    fn a_missing_file_means_nothing_has_been_measured() {
        let dir = tempdir().unwrap();
        let corrections = Corrections::read(&dir.path().join("audio-calibration.json"));
        assert_eq!(corrections, Corrections::default());
        assert!(corrections.find(PHILIPS, INSTA).is_none());
    }

    #[test]
    fn a_corrupt_file_means_nothing_has_been_measured() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-calibration.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Corrections::read(&path), Corrections::default());
    }

    // Written by a version that recorded neither a confidence nor a date. It
    // still holds the two numbers that matter, so it is still usable.
    #[test]
    fn a_file_from_before_the_extra_fields_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-calibration.json");
        std::fs::write(
            &path,
            br#"{"pairs":[{"output_uid":"BuiltInSpeakerDevice",
                          "input_uid":"BuiltInMicrophoneDevice",
                          "measured_ms":80.0,"computed_ms":70.0}]}"#,
        )
        .unwrap();
        let corrections = Corrections::read(&path);
        let found = corrections
            .find("BuiltInSpeakerDevice", "BuiltInMicrophoneDevice")
            .unwrap();
        assert_eq!(found.offset_ms(), 10.0);
        assert_eq!(found.measured_at, None);
    }

    // An entry missing one of the two numbers the correction is made of cannot
    // be repaired, and guessing at it would be the exact failure this whole
    // file exists to avoid.
    #[test]
    fn an_entry_without_a_measurement_takes_the_file_with_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("audio-calibration.json");
        std::fs::write(
            &path,
            br#"{"pairs":[{"output_uid":"a","input_uid":"b","computed_ms":70.0}]}"#,
        )
        .unwrap();
        assert_eq!(Corrections::read(&path), Corrections::default());
    }

    #[test]
    fn the_offset_can_go_either_way() {
        let mut over = correction(PHILIPS, 30.0);
        over.computed_ms = 42.0;
        assert_eq!(over.offset_ms(), -12.0);
    }

    // Measured at 1_755_300_000, so a day later is current and five weeks
    // later is not.
    #[test]
    fn a_measurement_goes_stale_after_a_month() {
        let fresh = correction(PHILIPS, 214.0);
        assert!(!fresh.is_stale(1_755_300_000 + 24 * 60 * 60));
        assert!(fresh.is_stale(1_755_300_000 + 35 * 24 * 60 * 60));
        assert_eq!(fresh.age_secs(1_755_300_000 + 3600), Some(3600));
    }

    // A clock that has gone backwards since the measurement reads as brand new
    // rather than as an enormous age, because there is nothing else honest to
    // say and the bridge must not refuse to start over it.
    #[test]
    fn a_measurement_from_the_future_is_not_stale() {
        let fresh = correction(PHILIPS, 214.0);
        assert_eq!(fresh.age_secs(1_755_200_000), Some(0));
        assert!(!fresh.is_stale(1_755_200_000));
    }

    // An entry from before the date was recorded could be any age at all, and
    // the whole point of the field is that age is visible.
    #[test]
    fn a_measurement_with_no_date_is_stale() {
        let mut undated = correction(PHILIPS, 214.0);
        undated.measured_at = None;
        assert_eq!(undated.age_secs(1_755_300_000), None);
        assert!(undated.is_stale(1_755_300_000));
    }
}
