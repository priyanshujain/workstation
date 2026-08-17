use std::time::Duration;

use anyhow::Result;

use crate::arrange::{DisplayInfo, Move};

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    macos::list_displays()
}

#[cfg(target_os = "macos")]
pub fn apply_moves(moves: &[Move]) -> Result<()> {
    macos::apply_moves(moves)
}

#[cfg(target_os = "macos")]
pub fn watch(interval: Duration, handler: impl FnMut()) -> Result<()> {
    macos::watch(interval, handler)
}

#[cfg(not(target_os = "macos"))]
pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    anyhow::bail!("display arrangement is only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn apply_moves(_moves: &[Move]) -> Result<()> {
    anyhow::bail!("display arrangement is only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn watch(_interval: Duration, _handler: impl FnMut()) -> Result<()> {
    anyhow::bail!("display arrangement is only supported on macOS")
}
