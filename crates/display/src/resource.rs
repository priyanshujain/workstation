use wsctl_core::{Change, Context, Error, Resource, ResourceId, ResourceState, Result};

use crate::key::DisplayKey;
use crate::{agent, config, spaces};

/// Declares which panel keeps the menu bar, and keeps a watcher running to enforce it.
///
/// This is the declarative half of `wsctl display`: putting one in a scope means a fresh
/// machine gets the pin and the background watcher from `wsctl apply`, with no manual step.
#[derive(Debug, Clone)]
pub struct MainDisplay {
    key: DisplayKey,
}

impl MainDisplay {
    pub fn new(key: DisplayKey) -> Self {
        Self { key }
    }
}

impl Resource for MainDisplay {
    fn id(&self) -> ResourceId {
        ResourceId::new("display::main", self.key.to_string())
    }

    fn detect(&self, _ctx: &Context) -> Result<ResourceState> {
        let prefs = config::load().map_err(|e| Error::DetectionFailed {
            resource: self.id(),
            message: format!("{e:#}"),
        })?;

        // Both halves matter: the recorded pin survives reboots, the agent survives
        // reconnects. Missing either means this is not fully set up.
        if prefs.preferred_main == Some(self.key) && agent::is_installed() {
            Ok(ResourceState::present())
        } else {
            Ok(ResourceState::Absent)
        }
    }

    fn diff(&self, current: &ResourceState) -> Result<Change> {
        Ok(if current.is_present() {
            Change::NoOp
        } else {
            Change::Create
        })
    }

    fn apply(&self, change: &Change, ctx: &Context) -> Result<()> {
        if change.is_noop() || ctx.dry_run {
            return Ok(());
        }

        let failed = |e: anyhow::Error| Error::ApplyFailed {
            resource: self.id(),
            message: format!("{e:#}"),
        };

        let mut prefs = config::load().map_err(&failed)?;
        prefs.preferred_main = Some(self.key);
        config::save(&prefs).map_err(&failed)?;

        // Correct the layout now rather than waiting for the next reconnect.
        crate::enforce().map_err(&failed)?;

        let exe = std::env::current_exe().map_err(|e| Error::ApplyFailed {
            resource: self.id(),
            message: format!("could not locate the wsctl binary: {e}"),
        })?;
        agent::install(&exe).map_err(&failed)?;

        Ok(())
    }

    fn description(&self) -> String {
        format!(
            "main display pinned to EDID serial {}",
            self.key.serial_hex()
        )
    }

    fn parallelizable(&self) -> bool {
        false
    }
}

/// Keeps Mission Control's "Displays have separate Spaces" turned on.
///
/// With it off, one Space stretches across every panel, so a three-finger swipe switches both
/// screens at once and only the main panel draws a menu bar. Nothing in `wsctl` turns it off,
/// but it is a single preference key that a stray click in System Settings or any setup script
/// can flip, and the WindowServer only reads it at login, so the damage stays invisible until
/// the next reboot. Declaring it means `wsctl apply` notices and puts it back.
#[derive(Debug, Clone)]
pub struct SeparateSpaces;

impl Resource for SeparateSpaces {
    fn id(&self) -> ResourceId {
        ResourceId::new("display::spaces", "separate")
    }

    fn detect(&self, _ctx: &Context) -> Result<ResourceState> {
        let separate = spaces::separate_spaces_enabled().map_err(|e| Error::DetectionFailed {
            resource: self.id(),
            message: format!("{e:#}"),
        })?;

        if separate {
            Ok(ResourceState::present())
        } else {
            Ok(ResourceState::Absent)
        }
    }

    fn diff(&self, current: &ResourceState) -> Result<Change> {
        Ok(if current.is_present() {
            Change::NoOp
        } else {
            Change::Create
        })
    }

    fn apply(&self, change: &Change, ctx: &Context) -> Result<()> {
        if change.is_noop() || ctx.dry_run {
            return Ok(());
        }

        spaces::set_separate_spaces(true).map_err(|e| Error::ApplyFailed {
            resource: self.id(),
            message: format!("{e:#}"),
        })?;

        tracing::warn!("separate Spaces restored; log out and back in for it to take effect");
        Ok(())
    }

    fn description(&self) -> String {
        "each display keeps its own Spaces".to_string()
    }
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
    fn id_is_scoped_to_the_panel() {
        let id = MainDisplay::new(MONITOR_1).id();
        assert_eq!(id.kind, "display::main");
        assert_eq!(id.name, "4268:53466:911690818");
    }

    #[test]
    fn absent_state_asks_for_creation() {
        let resource = MainDisplay::new(MONITOR_1);
        assert_eq!(
            resource.diff(&ResourceState::Absent).unwrap(),
            Change::Create
        );
    }

    #[test]
    fn present_state_is_a_no_op() {
        let resource = MainDisplay::new(MONITOR_1);
        assert_eq!(
            resource.diff(&ResourceState::present()).unwrap(),
            Change::NoOp
        );
    }

    #[test]
    fn dry_run_changes_nothing() {
        let resource = MainDisplay::new(MONITOR_1);
        let ctx = Context::new("test").with_dry_run(true);
        assert!(resource.apply(&Change::Create, &ctx).is_ok());
    }

    #[test]
    fn noop_change_does_not_touch_the_system() {
        let resource = MainDisplay::new(MONITOR_1);
        let ctx = Context::new("test");
        assert!(resource.apply(&Change::NoOp, &ctx).is_ok());
    }

    #[test]
    fn description_names_the_panel_the_way_the_docs_do() {
        let resource = MainDisplay::new(MONITOR_1);
        assert!(resource.description().contains("36574c42"));
    }

    #[test]
    fn spaces_resource_is_a_singleton_id() {
        let id = SeparateSpaces.id();
        assert_eq!(id.kind, "display::spaces");
        assert_eq!(id.name, "separate");
    }

    #[test]
    fn spanning_spaces_ask_for_correction() {
        assert_eq!(
            SeparateSpaces.diff(&ResourceState::Absent).unwrap(),
            Change::Create
        );
    }

    #[test]
    fn separate_spaces_are_left_alone() {
        assert_eq!(
            SeparateSpaces.diff(&ResourceState::present()).unwrap(),
            Change::NoOp
        );
    }

    #[test]
    fn spaces_dry_run_does_not_write_the_preference() {
        let ctx = Context::new("test").with_dry_run(true);
        assert!(SeparateSpaces.apply(&Change::Create, &ctx).is_ok());
        assert!(
            SeparateSpaces
                .apply(&Change::NoOp, &Context::new("test"))
                .is_ok()
        );
    }
}
