//! ws-dsl: DSL and builder API for declarative workstation configuration
//!
//! This crate provides an ergonomic API for defining workstation configurations:
//!
//! ```rust,ignore
//! use ws_dsl::prelude::*;
//!
//! pub fn config() -> Workstation {
//!     Workstation::builder("my-workstation")
//!         .scope("personal", |s| s
//!             .brew_formula("git")
//!             .brew_formula("ripgrep")
//!             .brew_cask("raycast"))
//!         .scope("work", |s| s
//!             .brew_cask("datagrip"))
//!         .profile("personal-mac", &["personal"])
//!         .profile("work-mac", &["personal", "work"])
//!         .build()
//! }
//! ```

mod builder;

pub use builder::{ScopeBuilder, Workstation, WorkstationBuilder};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::{ScopeBuilder, Workstation, WorkstationBuilder};
    pub use ws_core::{Context, Profile, Resource, ResourceId, Scope, ScopedResources};
    pub use ws_macos::{BrewCask, BrewFormula};
}
