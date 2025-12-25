//! Resource dependency graph
//!
//! Uses petgraph to manage resource dependencies and compute execution order.

use crate::{Error, Resource, ResourceId, Result};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use std::sync::Arc;

/// A graph of resources with their dependencies
#[derive(Debug)]
pub struct ResourceGraph {
    /// The underlying directed graph
    graph: DiGraph<ResourceId, ()>,
    /// Map from ResourceId to node index
    node_indices: HashMap<ResourceId, NodeIndex>,
    /// Map from ResourceId to the actual resource
    resources: HashMap<ResourceId, Arc<dyn Resource>>,
}

impl ResourceGraph {
    /// Create a new empty resource graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_indices: HashMap::new(),
            resources: HashMap::new(),
        }
    }

    /// Add a resource to the graph
    pub fn add(&mut self, resource: impl Resource + 'static) {
        let id = resource.id();
        if self.node_indices.contains_key(&id) {
            // Resource already exists, skip
            return;
        }
        let idx = self.graph.add_node(id.clone());
        self.node_indices.insert(id.clone(), idx);
        self.resources.insert(id, Arc::new(resource));
    }

    /// Add a boxed resource to the graph
    pub fn add_boxed(&mut self, resource: Arc<dyn Resource>) {
        let id = resource.id();
        if self.node_indices.contains_key(&id) {
            return;
        }
        let idx = self.graph.add_node(id.clone());
        self.node_indices.insert(id.clone(), idx);
        self.resources.insert(id, resource);
    }

    /// Build edges based on each resource's depends_on()
    pub fn build_edges(&mut self) -> Result<()> {
        // Collect all edges to add (to avoid borrowing issues)
        let mut edges_to_add = Vec::new();

        for (id, resource) in &self.resources {
            let to_idx = self.node_indices[id];
            for dep_id in resource.depends_on() {
                let from_idx = self.node_indices.get(&dep_id).ok_or_else(|| {
                    Error::MissingDependency {
                        resource: id.clone(),
                        dependency: dep_id.clone(),
                    }
                })?;
                edges_to_add.push((*from_idx, to_idx));
            }
        }

        for (from, to) in edges_to_add {
            self.graph.add_edge(from, to, ());
        }

        Ok(())
    }

    /// Get resources in topological order (dependencies first)
    pub fn execution_order(&self) -> Result<Vec<Arc<dyn Resource>>> {
        let sorted = toposort(&self.graph, None).map_err(|cycle| Error::CyclicDependency {
            resource: self.graph[cycle.node_id()].clone(),
        })?;

        Ok(sorted
            .into_iter()
            .filter_map(|idx| {
                let id = &self.graph[idx];
                self.resources.get(id).cloned()
            })
            .collect())
    }

    /// Get a resource by ID
    pub fn get(&self, id: &ResourceId) -> Option<Arc<dyn Resource>> {
        self.resources.get(id).cloned()
    }

    /// Get all resource IDs
    pub fn resource_ids(&self) -> impl Iterator<Item = &ResourceId> {
        self.resources.keys()
    }

    /// Get the number of resources
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Check if the graph is empty
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Get parallelizable batches of resources
    ///
    /// Returns groups of resources where all resources in a group can be
    /// executed in parallel (all their dependencies are in previous groups).
    pub fn parallel_batches(&self) -> Result<Vec<Vec<Arc<dyn Resource>>>> {
        let order = self.execution_order()?;

        // For MVP, just return each resource in its own batch (sequential)
        // TODO: Implement proper batch computation using Kahn's algorithm
        Ok(order.into_iter().map(|r| vec![r]).collect())
    }
}

impl Default for ResourceGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Change, Context, ResourceState};

    #[derive(Debug)]
    struct TestResource {
        id: ResourceId,
        deps: Vec<ResourceId>,
    }

    impl Resource for TestResource {
        fn id(&self) -> ResourceId {
            self.id.clone()
        }

        fn depends_on(&self) -> Vec<ResourceId> {
            self.deps.clone()
        }

        fn detect(&self, _ctx: &Context) -> Result<ResourceState> {
            Ok(ResourceState::Absent)
        }

        fn diff(&self, _current: &ResourceState) -> Result<Change> {
            Ok(Change::Create)
        }

        fn apply(&self, _change: &Change, _ctx: &Context) -> Result<()> {
            Ok(())
        }

        fn description(&self) -> String {
            format!("Test resource: {}", self.id)
        }
    }

    #[test]
    fn test_topological_order() {
        let mut graph = ResourceGraph::new();

        // A depends on B, B depends on C
        // Execution order should be: C, B, A
        graph.add(TestResource {
            id: ResourceId::new("test", "a"),
            deps: vec![ResourceId::new("test", "b")],
        });
        graph.add(TestResource {
            id: ResourceId::new("test", "b"),
            deps: vec![ResourceId::new("test", "c")],
        });
        graph.add(TestResource {
            id: ResourceId::new("test", "c"),
            deps: vec![],
        });

        graph.build_edges().unwrap();
        let order = graph.execution_order().unwrap();

        let names: Vec<_> = order.iter().map(|r| r.id().name.clone()).collect();
        assert_eq!(names, vec!["c", "b", "a"]);
    }
}
