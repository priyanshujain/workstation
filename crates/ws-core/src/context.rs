//! Execution context passed to resource operations

use crate::command::{CommandRunner, SystemCommandRunner};
use std::path::PathBuf;
use std::sync::Arc;

/// Context passed to resource operations
///
/// Contains configuration and environment information needed
/// during resource detection and application.
#[derive(Debug, Clone)]
pub struct Context {
    /// If true, don't actually make changes
    pub dry_run: bool,
    /// Verbosity level (0 = quiet, 1 = normal, 2+ = verbose)
    pub verbose: u8,
    /// User's home directory
    pub home_dir: PathBuf,
    /// Directory containing the workstation config
    pub config_dir: PathBuf,
    /// Currently active profile name
    pub profile: String,
    /// Command runner for executing shell commands
    pub command_runner: Arc<dyn CommandRunner>,
}

impl Context {
    /// Create a new context with the given profile
    ///
    /// Uses the real `SystemCommandRunner` by default.
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            dry_run: false,
            verbose: 1,
            home_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")),
            config_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            profile: profile.into(),
            command_runner: Arc::new(SystemCommandRunner::new()),
        }
    }

    /// Create a new context with a custom command runner
    ///
    /// Useful for testing with `MockCommandRunner`.
    pub fn with_command_runner(
        profile: impl Into<String>,
        command_runner: Arc<dyn CommandRunner>,
    ) -> Self {
        Self {
            dry_run: false,
            verbose: 1,
            home_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")),
            config_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            profile: profile.into(),
            command_runner,
        }
    }

    /// Set dry-run mode
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Set verbosity level
    pub fn with_verbose(mut self, verbose: u8) -> Self {
        self.verbose = verbose;
        self
    }

    /// Set config directory
    pub fn with_config_dir(mut self, dir: PathBuf) -> Self {
        self.config_dir = dir;
        self
    }

    /// Run a command using the configured command runner
    pub fn run_command(&self, program: &str, args: &[&str]) -> crate::Result<crate::CommandOutput> {
        self.command_runner.run(program, args)
    }

    /// Expand ~ in a path to the home directory
    pub fn expand_path(&self, path: &str) -> PathBuf {
        if let Some(stripped) = path.strip_prefix("~/") {
            self.home_dir.join(stripped)
        } else if path == "~" {
            self.home_dir.clone()
        } else {
            PathBuf::from(path)
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new("default")
    }
}
