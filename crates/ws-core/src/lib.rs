//! ws-core: Core abstractions for declarative workstation configuration
//!
//! This crate provides the fundamental building blocks:
//! - `Resource` trait: The core abstraction for anything that can be managed
//! - `ResourceGraph`: Dependency-aware graph of resources
//! - `Executor`: Parallel execution engine
//! - `Scope` and `Profile`: Organization of resources by purpose and machine
//! - `CommandRunner`: Abstraction for shell commands (enables testing)

pub mod command;
pub mod context;
pub mod error;
pub mod executor;
pub mod graph;
pub mod resource;
pub mod scope;
pub mod testing;

pub use command::{CommandOutput, CommandRunner, SystemCommandRunner};
pub use context::Context;
pub use error::{Error, Result};
pub use executor::{ApplyResult, ExecutionPlan, ExecutionReport, Executor};
pub use graph::ResourceGraph;
pub use resource::{Change, ChangeDetail, Resource, ResourceId, ResourceState};
pub use scope::{Profile, Scope, ScopedResources};
pub use testing::MockCommandRunner;
