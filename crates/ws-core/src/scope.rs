//! Scope and Profile types for organizing resources
//!
//! Scopes group related resources (e.g., "personal", "okcredit")
//! Profiles define which scopes are active on a machine (e.g., "work-macbook")

use crate::{Error, Resource, ResourceGraph, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// A scope containing related resources
#[derive(Debug, Clone)]
pub struct Scope {
    /// Name of the scope
    pub name: String,
    /// Resources in this scope
    resources: Vec<Arc<dyn Resource>>,
}

impl Scope {
    /// Create a new scope with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            resources: Vec::new(),
        }
    }

    /// Add a resource to this scope
    pub fn add(&mut self, resource: impl Resource + 'static) {
        self.resources.push(Arc::new(resource));
    }

    /// Add a boxed resource to this scope
    pub fn add_boxed(&mut self, resource: Arc<dyn Resource>) {
        self.resources.push(resource);
    }

    /// Get resources in this scope
    pub fn resources(&self) -> &[Arc<dyn Resource>] {
        &self.resources
    }

    /// Get the number of resources in this scope
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Check if this scope is empty
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

/// A profile that activates specific scopes
#[derive(Debug, Clone)]
pub struct Profile {
    /// Name of the profile (e.g., "work-macbook")
    pub name: String,
    /// Names of scopes this profile includes
    pub scopes: Vec<String>,
}

impl Profile {
    /// Create a new profile with the given name and scopes
    pub fn new(
        name: impl Into<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            scopes: scopes.into_iter().map(|s| s.into()).collect(),
        }
    }
}

/// Collection of scopes and profiles
#[derive(Debug, Default)]
pub struct ScopedResources {
    /// All defined scopes
    scopes: HashMap<String, Scope>,
    /// All defined profiles
    profiles: HashMap<String, Profile>,
}

impl ScopedResources {
    /// Create a new empty collection
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a scope
    pub fn add_scope(&mut self, scope: Scope) {
        self.scopes.insert(scope.name.clone(), scope);
    }

    /// Add a profile
    pub fn add_profile(&mut self, profile: Profile) {
        self.profiles.insert(profile.name.clone(), profile);
    }

    /// Get a scope by name
    pub fn get_scope(&self, name: &str) -> Option<&Scope> {
        self.scopes.get(name)
    }

    /// Get a profile by name
    pub fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    /// Get all scope names
    pub fn scope_names(&self) -> Vec<String> {
        self.scopes.keys().cloned().collect()
    }

    /// Get all profile names
    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Build a ResourceGraph for a specific profile
    ///
    /// Collects all resources from scopes included in the profile
    /// and builds a dependency graph.
    pub fn build_graph_for_profile(&self, profile_name: &str) -> Result<ResourceGraph> {
        let profile = self
            .profiles
            .get(profile_name)
            .ok_or_else(|| Error::ProfileNotFound {
                name: profile_name.to_string(),
                available: self.profile_names(),
            })?;

        let mut graph = ResourceGraph::new();

        for scope_name in &profile.scopes {
            let scope = self
                .scopes
                .get(scope_name)
                .ok_or_else(|| Error::ScopeNotFound {
                    name: scope_name.clone(),
                    available: self.scope_names(),
                })?;

            for resource in scope.resources() {
                graph.add_boxed(resource.clone());
            }
        }

        graph.build_edges()?;
        Ok(graph)
    }
}
