//! Homebrew formula and cask resources
//!
//! Manages packages installed via Homebrew (brew install / brew install --cask)

use ws_core::{Change, Context, Resource, ResourceId, ResourceState, Result};

/// A Homebrew formula (CLI tools, libraries)
#[derive(Debug, Clone)]
pub struct BrewFormula {
    /// Formula name (e.g., "git", "ripgrep")
    pub name: String,
}

impl BrewFormula {
    /// Create a new formula resource
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Check if a formula is installed using brew
    fn is_installed(&self, ctx: &Context) -> Result<bool> {
        let output = ctx.run_command("brew", &["list", "--formula", &self.name])?;
        Ok(output.success)
    }

    /// Get installed version
    fn installed_version(&self, ctx: &Context) -> Result<Option<String>> {
        let output = ctx.run_command("brew", &["list", "--versions", &self.name])?;

        if output.success {
            // Output format: "formula_name version1 version2 ..."
            let version = output
                .stdout
                .trim()
                .split_whitespace()
                .nth(1)
                .map(|s| s.to_string());
            Ok(version)
        } else {
            Ok(None)
        }
    }
}

impl Resource for BrewFormula {
    fn id(&self) -> ResourceId {
        ResourceId::new("brew::formula", &self.name)
    }

    fn detect(&self, ctx: &Context) -> Result<ResourceState> {
        if self.is_installed(ctx)? {
            if let Some(version) = self.installed_version(ctx)? {
                Ok(ResourceState::present_with_version(version))
            } else {
                Ok(ResourceState::present())
            }
        } else {
            Ok(ResourceState::Absent)
        }
    }

    fn diff(&self, current: &ResourceState) -> Result<Change> {
        match current {
            ResourceState::Absent => Ok(Change::Create),
            ResourceState::Present { .. } => Ok(Change::NoOp),
            ResourceState::Unknown(msg) => {
                tracing::warn!("Unknown state for {}: {}", self.name, msg);
                Ok(Change::NoOp)
            }
        }
    }

    fn apply(&self, change: &Change, ctx: &Context) -> Result<()> {
        match change {
            Change::Create => {
                if ctx.verbose > 0 {
                    tracing::info!("Installing formula: {}", self.name);
                }

                let output = ctx.run_command("brew", &["install", &self.name])?;

                if !output.success {
                    return Err(ws_core::Error::CommandFailed {
                        command: format!("brew install {}", self.name),
                        stderr: output.stderr,
                    }
                    .into());
                }
                Ok(())
            }
            Change::Remove => {
                if ctx.verbose > 0 {
                    tracing::info!("Uninstalling formula: {}", self.name);
                }

                let output = ctx.run_command("brew", &["uninstall", &self.name])?;

                if !output.success {
                    return Err(ws_core::Error::CommandFailed {
                        command: format!("brew uninstall {}", self.name),
                        stderr: output.stderr,
                    }
                    .into());
                }
                Ok(())
            }
            Change::NoOp | Change::Update(_) => Ok(()),
        }
    }

    fn description(&self) -> String {
        format!("Homebrew formula: {}", self.name)
    }
}

/// A Homebrew cask (GUI applications)
#[derive(Debug, Clone)]
pub struct BrewCask {
    /// Cask name (e.g., "raycast", "visual-studio-code")
    pub name: String,
}

impl BrewCask {
    /// Create a new cask resource
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Check if a cask is installed
    fn is_installed(&self, ctx: &Context) -> Result<bool> {
        let output = ctx.run_command("brew", &["list", "--cask", &self.name])?;
        Ok(output.success)
    }

    /// Get installed version
    fn installed_version(&self, ctx: &Context) -> Result<Option<String>> {
        let output = ctx.run_command("brew", &["list", "--cask", "--versions", &self.name])?;

        if output.success {
            let version = output
                .stdout
                .trim()
                .split_whitespace()
                .nth(1)
                .map(|s| s.to_string());
            Ok(version)
        } else {
            Ok(None)
        }
    }
}

impl Resource for BrewCask {
    fn id(&self) -> ResourceId {
        ResourceId::new("brew::cask", &self.name)
    }

    fn detect(&self, ctx: &Context) -> Result<ResourceState> {
        if self.is_installed(ctx)? {
            if let Some(version) = self.installed_version(ctx)? {
                Ok(ResourceState::present_with_version(version))
            } else {
                Ok(ResourceState::present())
            }
        } else {
            Ok(ResourceState::Absent)
        }
    }

    fn diff(&self, current: &ResourceState) -> Result<Change> {
        match current {
            ResourceState::Absent => Ok(Change::Create),
            ResourceState::Present { .. } => Ok(Change::NoOp),
            ResourceState::Unknown(msg) => {
                tracing::warn!("Unknown state for {}: {}", self.name, msg);
                Ok(Change::NoOp)
            }
        }
    }

    fn apply(&self, change: &Change, ctx: &Context) -> Result<()> {
        match change {
            Change::Create => {
                if ctx.verbose > 0 {
                    tracing::info!("Installing cask: {}", self.name);
                }

                let output = ctx.run_command("brew", &["install", "--cask", &self.name])?;

                if !output.success {
                    return Err(ws_core::Error::CommandFailed {
                        command: format!("brew install --cask {}", self.name),
                        stderr: output.stderr,
                    }
                    .into());
                }
                Ok(())
            }
            Change::Remove => {
                if ctx.verbose > 0 {
                    tracing::info!("Uninstalling cask: {}", self.name);
                }

                let output = ctx.run_command("brew", &["uninstall", "--cask", &self.name])?;

                if !output.success {
                    return Err(ws_core::Error::CommandFailed {
                        command: format!("brew uninstall --cask {}", self.name),
                        stderr: output.stderr,
                    }
                    .into());
                }
                Ok(())
            }
            Change::NoOp | Change::Update(_) => Ok(()),
        }
    }

    fn description(&self) -> String {
        format!("Homebrew cask: {}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ws_core::{CommandOutput, MockCommandRunner};

    #[test]
    fn test_brew_formula_detect_installed() {
        let mock = Arc::new(
            MockCommandRunner::new()
                .expect(
                    "brew",
                    &["list", "--formula", "git"],
                    CommandOutput::success(""),
                )
                .expect(
                    "brew",
                    &["list", "--versions", "git"],
                    CommandOutput::success("git 2.43.0"),
                ),
        );

        let ctx = Context::with_command_runner("test", mock.clone());
        let formula = BrewFormula::new("git");

        let state = formula.detect(&ctx).unwrap();
        assert!(matches!(
            state,
            ResourceState::Present { version: Some(v) } if v == "2.43.0"
        ));

        mock.verify();
    }

    #[test]
    fn test_brew_formula_detect_not_installed() {
        let mock = Arc::new(
            MockCommandRunner::new().expect(
                "brew",
                &["list", "--formula", "ripgrep"],
                CommandOutput::failure("Error: No such keg"),
            ),
        );

        let ctx = Context::with_command_runner("test", mock.clone());
        let formula = BrewFormula::new("ripgrep");

        let state = formula.detect(&ctx).unwrap();
        assert!(matches!(state, ResourceState::Absent));

        mock.verify();
    }

    #[test]
    fn test_brew_formula_diff_needs_install() {
        let formula = BrewFormula::new("fzf");
        let change = formula.diff(&ResourceState::Absent).unwrap();
        assert!(matches!(change, Change::Create));
    }

    #[test]
    fn test_brew_formula_diff_already_installed() {
        let formula = BrewFormula::new("fzf");
        let change = formula
            .diff(&ResourceState::present_with_version("0.45.0"))
            .unwrap();
        assert!(matches!(change, Change::NoOp));
    }

    #[test]
    fn test_brew_formula_apply_install() {
        let mock = Arc::new(
            MockCommandRunner::new().expect(
                "brew",
                &["install", "neovim"],
                CommandOutput::success("==> Installing neovim"),
            ),
        );

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let formula = BrewFormula::new("neovim");

        formula.apply(&Change::Create, &ctx).unwrap();
        mock.verify();
    }

    #[test]
    fn test_brew_cask_detect_installed() {
        let mock = Arc::new(
            MockCommandRunner::new()
                .expect(
                    "brew",
                    &["list", "--cask", "raycast"],
                    CommandOutput::success(""),
                )
                .expect(
                    "brew",
                    &["list", "--cask", "--versions", "raycast"],
                    CommandOutput::success("raycast 1.65.0"),
                ),
        );

        let ctx = Context::with_command_runner("test", mock.clone());
        let cask = BrewCask::new("raycast");

        let state = cask.detect(&ctx).unwrap();
        assert!(matches!(
            state,
            ResourceState::Present { version: Some(v) } if v == "1.65.0"
        ));

        mock.verify();
    }

    #[test]
    fn test_brew_cask_apply_install() {
        let mock = Arc::new(
            MockCommandRunner::new().expect(
                "brew",
                &["install", "--cask", "docker"],
                CommandOutput::success("==> Installing docker"),
            ),
        );

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let cask = BrewCask::new("docker");

        cask.apply(&Change::Create, &ctx).unwrap();
        mock.verify();
    }

    #[test]
    fn test_brew_formula_apply_install_failure() {
        let mock = Arc::new(
            MockCommandRunner::new().expect(
                "brew",
                &["install", "nonexistent"],
                CommandOutput::failure("Error: No formulae found"),
            ),
        );

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let formula = BrewFormula::new("nonexistent");

        let result = formula.apply(&Change::Create, &ctx);
        assert!(result.is_err());

        mock.verify();
    }
}
