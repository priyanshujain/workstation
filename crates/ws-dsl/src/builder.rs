//! Builder API for workstation configuration

use ws_core::{Profile, Resource, ResourceGraph, Result, Scope, ScopedResources};
use ws_macos::{BrewCask, BrewFormula};
use std::sync::Arc;

/// A complete workstation configuration
#[derive(Debug)]
pub struct Workstation {
    /// Name of this workstation configuration
    pub name: String,
    /// Scoped resources
    pub resources: ScopedResources,
}

impl Workstation {
    /// Create a new workstation builder
    pub fn builder(name: impl Into<String>) -> WorkstationBuilder {
        WorkstationBuilder::new(name)
    }

    /// Build a resource graph for a specific profile
    pub fn build_graph(&self, profile_name: &str) -> Result<ResourceGraph> {
        self.resources.build_graph_for_profile(profile_name)
    }

    /// Get all available profile names
    pub fn profile_names(&self) -> Vec<String> {
        self.resources.profile_names()
    }

    /// Get all available scope names
    pub fn scope_names(&self) -> Vec<String> {
        self.resources.scope_names()
    }
}

/// Builder for constructing a Workstation
pub struct WorkstationBuilder {
    name: String,
    scopes: Vec<Scope>,
    profiles: Vec<Profile>,
}

impl WorkstationBuilder {
    /// Create a new builder
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scopes: Vec::new(),
            profiles: Vec::new(),
        }
    }

    /// Add a scope using a builder closure
    pub fn scope(mut self, name: impl Into<String>, f: impl FnOnce(ScopeBuilder) -> ScopeBuilder) -> Self {
        let name = name.into();
        let builder = ScopeBuilder::new(&name);
        let builder = f(builder);
        self.scopes.push(builder.build());
        self
    }

    /// Add a profile
    pub fn profile(mut self, name: impl Into<String>, scopes: &[&str]) -> Self {
        self.profiles.push(Profile::new(
            name,
            scopes.iter().map(|s| s.to_string()),
        ));
        self
    }

    /// Build the workstation
    pub fn build(self) -> Workstation {
        let mut resources = ScopedResources::new();

        for scope in self.scopes {
            resources.add_scope(scope);
        }

        for profile in self.profiles {
            resources.add_profile(profile);
        }

        Workstation {
            name: self.name,
            resources,
        }
    }
}

/// Builder for constructing a Scope
pub struct ScopeBuilder {
    scope: Scope,
}

impl ScopeBuilder {
    /// Create a new scope builder
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            scope: Scope::new(name),
        }
    }

    /// Add a Homebrew formula
    pub fn brew_formula(mut self, name: impl Into<String>) -> Self {
        self.scope.add(BrewFormula::new(name));
        self
    }

    /// Add multiple Homebrew formulae
    pub fn brew_formulae(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for name in names {
            self.scope.add(BrewFormula::new(name));
        }
        self
    }

    /// Add a Homebrew cask
    pub fn brew_cask(mut self, name: impl Into<String>) -> Self {
        self.scope.add(BrewCask::new(name));
        self
    }

    /// Add multiple Homebrew casks
    pub fn brew_casks(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for name in names {
            self.scope.add(BrewCask::new(name));
        }
        self
    }

    /// Add a generic resource
    pub fn resource(mut self, resource: impl Resource + 'static) -> Self {
        self.scope.add(resource);
        self
    }

    /// Add a boxed resource
    pub fn resource_boxed(mut self, resource: Arc<dyn Resource>) -> Self {
        self.scope.add_boxed(resource);
        self
    }

    /// Build the scope
    pub fn build(self) -> Scope {
        self.scope
    }
}
