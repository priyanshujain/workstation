pub mod agent;
pub mod arrange;
pub mod config;
pub mod key;
pub mod names;
pub mod platform;
pub mod resource;
pub mod spaces;

pub use arrange::{DisplayInfo, Move, current_main, has_duplicate_key, plan_moves, position_label};
pub use key::DisplayKey;
pub use resource::{MainDisplay, SeparateSpaces};

use anyhow::Result;

/// What a single enforcement pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No panel has been pinned yet.
    NotConfigured,
    /// The pinned panel is not connected, so the current layout is left alone.
    Disconnected,
    /// The pinned panel already holds the menu bar.
    AlreadyMain,
    /// The arrangement was shifted to put the pinned panel back on (0,0).
    Moved(DisplayKey),
}

/// Put the pinned panel back at (0,0) if it is connected and not already there.
pub fn enforce() -> Result<Outcome> {
    let Some(target) = config::load()?.preferred_main else {
        return Ok(Outcome::NotConfigured);
    };

    let displays = platform::list_displays()?;
    let Some(moves) = plan_moves(&displays, target) else {
        return Ok(Outcome::Disconnected);
    };

    if moves.is_empty() {
        return Ok(Outcome::AlreadyMain);
    }

    platform::apply_moves(&moves)?;
    Ok(Outcome::Moved(target))
}
