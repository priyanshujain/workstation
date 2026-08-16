//! The audio bridge, as the menu sees it.
//!
//! Enumeration is this crate's, because the menu needs names and the engine
//! only ever wants UIDs. Everything else is the engine in the `audio` crate,
//! which the `wsctl` CLI drives through the same calls.

use anyhow::Result;

use crate::coreaudio::{self, Direction};

pub struct Device {
    pub uid: String,
    pub name: String,
}

pub fn input_devices() -> Result<Vec<Device>> {
    coreaudio::devices(Direction::Input)
}

pub fn output_devices() -> Result<Vec<Device>> {
    coreaudio::devices(Direction::Output)
}

pub fn start(input_uid: &str, output_uid: &str) -> Result<()> {
    audio::bridge::start(input_uid, output_uid)
}

pub fn stop() -> Result<()> {
    audio::bridge::stop()
}

pub fn is_running() -> bool {
    audio::bridge::is_running()
}

/// Whether the microphone is being run through RNNoise.
pub fn denoise() -> bool {
    audio::bridge::denoise()
}

/// Noise suppression and the gate under it are process-wide rather than engine
/// state, so both can be set before anything has been started and neither is
/// lost when the user picks a different device and the engine is replaced.
pub fn set_denoise(on: bool) {
    audio::bridge::set_denoise(on);
}

pub fn voice_threshold() -> f32 {
    audio::bridge::voice_threshold()
}

pub fn set_voice_threshold(threshold: f32) {
    audio::bridge::set_voice_threshold(threshold);
}

/// How likely the model thought the last frame was speech, or `None` when
/// nothing is running for it to have an opinion about.
pub fn voice() -> Option<f32> {
    audio::bridge::stats().map(|s| s.voice)
}
