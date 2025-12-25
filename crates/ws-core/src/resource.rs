//! Resource trait and related types
//!
//! The `Resource` trait is the core abstraction for anything that can be managed
//! declaratively - packages, files, settings, services, etc.

use crate::{Context, Result};
use std::fmt::{Debug, Display};
use std::hash::Hash;

/// Unique identifier for a resource
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId {
    /// Resource type name (e.g., "brew::formula", "brew::cask", "dotfile")
    pub kind: String,
    /// Unique name within the kind
    pub name: String,
}

impl ResourceId {
    /// Create a new ResourceId
    pub fn new(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
        }
    }
}

impl Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", self.kind, self.name)
    }
}

/// The current state of a resource on the system
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceState {
    /// Resource does not exist
    Absent,
    /// Resource exists (with optional version info)
    Present { version: Option<String> },
    /// State cannot be determined
    Unknown(String),
}

impl ResourceState {
    /// Create a Present state with a version
    pub fn present_with_version(version: impl Into<String>) -> Self {
        Self::Present {
            version: Some(version.into()),
        }
    }

    /// Create a Present state without version info
    pub fn present() -> Self {
        Self::Present { version: None }
    }

    /// Check if the resource is present
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present { .. })
    }

    /// Check if the resource is absent
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// What action needs to be taken
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// No change needed
    NoOp,
    /// Create/install the resource
    Create,
    /// Update the resource (with details of what changes)
    Update(Vec<ChangeDetail>),
    /// Remove the resource
    Remove,
}

impl Change {
    /// Check if this is a no-op
    pub fn is_noop(&self) -> bool {
        matches!(self, Self::NoOp)
    }

    /// Get a human-readable description of the change
    pub fn description(&self) -> &'static str {
        match self {
            Self::NoOp => "no change",
            Self::Create => "create",
            Self::Update(_) => "update",
            Self::Remove => "remove",
        }
    }
}

/// Detail about what specifically is changing
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeDetail {
    pub field: String,
    pub from: String,
    pub to: String,
}

impl ChangeDetail {
    /// Create a new change detail
    pub fn new(
        field: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            from: from.into(),
            to: to.into(),
        }
    }
}

/// The core resource trait
///
/// Implement this trait for any type of resource that can be managed declaratively.
/// The executor will call these methods in order: detect → diff → apply
pub trait Resource: Debug + Send + Sync {
    /// Unique identifier for this resource instance
    fn id(&self) -> ResourceId;

    /// Dependencies that must be applied before this resource
    fn depends_on(&self) -> Vec<ResourceId> {
        vec![]
    }

    /// Detect current state on the system
    fn detect(&self, ctx: &Context) -> Result<ResourceState>;

    /// Compute what change is needed to reach desired state
    fn diff(&self, current: &ResourceState) -> Result<Change>;

    /// Apply the resource (make desired state real)
    fn apply(&self, change: &Change, ctx: &Context) -> Result<()>;

    /// Human-readable description for display
    fn description(&self) -> String;

    /// Can this resource be applied in parallel with others of the same kind?
    fn parallelizable(&self) -> bool {
        true
    }
}
