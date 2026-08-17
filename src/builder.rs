use display::{DisplayKey, MainDisplay, SeparateSpaces};
use packages::brew::{Cask, Formula};
use std::sync::Arc;
use wsctl_core::{Profile, Resource, ResourceGraph, Result, Scope, ScopedResources};

pub struct Workstation {
    pub name: String,
    pub resources: ScopedResources,
}

impl Workstation {
    pub fn builder(name: impl Into<String>) -> WorkstationBuilder {
        WorkstationBuilder::new(name)
    }

    pub fn build_graph(&self, profile_name: &str) -> Result<ResourceGraph> {
        self.resources.build_graph_for_profile(profile_name)
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.resources.profile_names()
    }

    pub fn scope_names(&self) -> Vec<String> {
        self.resources.scope_names()
    }
}

pub struct WorkstationBuilder {
    name: String,
    scopes: Vec<Scope>,
    profiles: Vec<Profile>,
}

impl WorkstationBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scopes: Vec::new(),
            profiles: Vec::new(),
        }
    }

    pub fn scope(
        mut self,
        name: impl Into<String>,
        f: impl FnOnce(ScopeBuilder) -> ScopeBuilder,
    ) -> Self {
        let name = name.into();
        let builder = ScopeBuilder::new(&name);
        let builder = f(builder);
        self.scopes.push(builder.build());
        self
    }

    pub fn profile(mut self, name: impl Into<String>, scopes: &[&str]) -> Self {
        self.profiles
            .push(Profile::new(name, scopes.iter().map(|s| s.to_string())));
        self
    }

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

pub struct ScopeBuilder {
    scope: Scope,
}

#[allow(dead_code)]
impl ScopeBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            scope: Scope::new(name),
        }
    }

    pub fn brew_formula(mut self, name: impl Into<String>) -> Self {
        self.scope.add(Formula::new(name));
        self
    }

    pub fn brew_formulae(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for name in names {
            self.scope.add(Formula::new(name));
        }
        self
    }

    pub fn brew_cask(mut self, name: impl Into<String>) -> Self {
        self.scope.add(Cask::new(name));
        self
    }

    pub fn brew_casks(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for name in names {
            self.scope.add(Cask::new(name));
        }
        self
    }

    /// Pin the panel that keeps the menu bar, and run a watcher to hold it there.
    pub fn main_display(mut self, key: DisplayKey) -> Self {
        self.scope.add(MainDisplay::new(key));
        self
    }

    /// Keep every panel on its own Spaces, so a swipe moves one screen and not both.
    pub fn separate_display_spaces(mut self) -> Self {
        self.scope.add(SeparateSpaces);
        self
    }

    pub fn resource(mut self, resource: impl Resource + 'static) -> Self {
        self.scope.add(resource);
        self
    }

    pub fn resource_boxed(mut self, resource: Arc<dyn Resource>) -> Self {
        self.scope.add_boxed(resource);
        self
    }

    pub fn build(self) -> Scope {
        self.scope
    }
}
