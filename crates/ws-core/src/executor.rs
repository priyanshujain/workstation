//! Execution engine for applying resources
//!
//! The executor handles:
//! - Planning: Computing what changes need to be made
//! - Execution: Applying changes in the correct order
//! - Reporting: Tracking results of each operation

use crate::{Change, Context, Resource, ResourceGraph, ResourceId, Result};
use std::sync::Arc;

/// Result of applying a single resource
#[derive(Debug, Clone)]
pub enum ApplyResult {
    /// Successfully applied the change
    Applied,
    /// No change was needed
    Unchanged,
    /// Skipped (dry-run mode or user declined)
    Skipped,
    /// Failed with an error message
    Failed(String),
}

impl ApplyResult {
    /// Check if this result represents a failure
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// Check if this result represents success (applied or unchanged)
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Applied | Self::Unchanged)
    }
}

/// A planned resource change
#[derive(Debug)]
pub struct PlannedResource {
    pub id: ResourceId,
    pub description: String,
    pub change: Change,
    pub resource: Arc<dyn Resource>,
}

/// The execution plan
#[derive(Debug, Default)]
pub struct ExecutionPlan {
    /// Resources to be changed, in execution order
    pub resources: Vec<PlannedResource>,
}

impl ExecutionPlan {
    /// Create an empty plan
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if the plan is empty (no changes needed)
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Get the number of planned changes
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Count of resources to be created
    pub fn creates(&self) -> usize {
        self.resources
            .iter()
            .filter(|r| matches!(r.change, Change::Create))
            .count()
    }

    /// Count of resources to be updated
    pub fn updates(&self) -> usize {
        self.resources
            .iter()
            .filter(|r| matches!(r.change, Change::Update(_)))
            .count()
    }

    /// Count of resources to be removed
    pub fn removes(&self) -> usize {
        self.resources
            .iter()
            .filter(|r| matches!(r.change, Change::Remove))
            .count()
    }
}

/// Result of a single resource application
#[derive(Debug)]
pub struct ResourceResult {
    pub id: ResourceId,
    pub result: ApplyResult,
}

/// Report of the entire execution
#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub results: Vec<ResourceResult>,
}

impl ExecutionReport {
    /// Create a new empty report
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a result to the report
    pub fn add(&mut self, id: ResourceId, result: ApplyResult) {
        self.results.push(ResourceResult { id, result });
    }

    /// Check if any resources failed
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|r| r.result.is_failed())
    }

    /// Count of failed resources
    pub fn failure_count(&self) -> usize {
        self.results.iter().filter(|r| r.result.is_failed()).count()
    }

    /// Count of successful resources
    pub fn success_count(&self) -> usize {
        self.results.iter().filter(|r| r.result.is_success()).count()
    }

    /// Get all failures
    pub fn failures(&self) -> Vec<&ResourceResult> {
        self.results.iter().filter(|r| r.result.is_failed()).collect()
    }
}

/// The execution engine
#[derive(Debug)]
pub struct Executor {
    /// Maximum parallel operations (currently unused, for future)
    parallelism: usize,
}

impl Executor {
    /// Create a new executor
    pub fn new() -> Self {
        Self { parallelism: 4 }
    }

    /// Set the parallelism level
    pub fn with_parallelism(mut self, parallelism: usize) -> Self {
        self.parallelism = parallelism;
        self
    }

    /// Compute the execution plan
    ///
    /// Detects current state of each resource and computes what changes are needed.
    pub fn plan(&self, graph: &ResourceGraph, ctx: &Context) -> Result<ExecutionPlan> {
        let resources = graph.execution_order()?;
        let mut plan = ExecutionPlan::new();

        for resource in resources {
            let current = resource.detect(ctx)?;
            let change = resource.diff(&current)?;

            if !change.is_noop() {
                plan.resources.push(PlannedResource {
                    id: resource.id(),
                    description: resource.description(),
                    change,
                    resource,
                });
            }
        }

        Ok(plan)
    }

    /// Execute the plan
    ///
    /// Applies each change in order. If dry_run is set in context,
    /// changes are not actually applied.
    pub fn execute(&self, plan: ExecutionPlan, ctx: &Context) -> Result<ExecutionReport> {
        let mut report = ExecutionReport::new();

        for planned in plan.resources {
            if ctx.dry_run {
                report.add(planned.id, ApplyResult::Skipped);
                continue;
            }

            let result = match planned.resource.apply(&planned.change, ctx) {
                Ok(()) => ApplyResult::Applied,
                Err(e) => ApplyResult::Failed(e.to_string()),
            };

            report.add(planned.id, result);
        }

        Ok(report)
    }

    /// Plan and execute in one step
    pub fn apply(&self, graph: &ResourceGraph, ctx: &Context) -> Result<ExecutionReport> {
        let plan = self.plan(graph, ctx)?;
        self.execute(plan, ctx)
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}
