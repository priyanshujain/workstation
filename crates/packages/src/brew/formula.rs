//! Homebrew formula resource (CLI tools, libraries).

use wsctl_core::{Change, Context, Resource, ResourceId, ResourceState, Result};

#[derive(Debug, Clone)]
pub struct Formula {
    pub name: String,
}

impl Formula {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    fn is_installed(&self, ctx: &Context) -> Result<bool> {
        let output = ctx.run_command("brew", &["list", "--formula", &self.name])?;
        Ok(output.success)
    }

    fn installed_version(&self, ctx: &Context) -> Result<Option<String>> {
        let output = ctx.run_command("brew", &["list", "--versions", &self.name])?;

        if output.success {
            let version = output
                .stdout
                .split_whitespace()
                .nth(1)
                .map(|s| s.to_string());
            Ok(version)
        } else {
            Ok(None)
        }
    }
}

impl Resource for Formula {
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
                    return Err(wsctl_core::Error::CommandFailed {
                        command: format!("brew install {}", self.name),
                        stderr: output.stderr,
                    });
                }
                Ok(())
            }
            Change::Remove => {
                if ctx.verbose > 0 {
                    tracing::info!("Uninstalling formula: {}", self.name);
                }

                let output = ctx.run_command("brew", &["uninstall", &self.name])?;

                if !output.success {
                    return Err(wsctl_core::Error::CommandFailed {
                        command: format!("brew uninstall {}", self.name),
                        stderr: output.stderr,
                    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wsctl_core::{CommandOutput, MockCommandRunner};

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
        let formula = Formula::new("git");

        let state = formula.detect(&ctx).unwrap();
        assert!(matches!(
            state,
            ResourceState::Present { version: Some(v) } if v == "2.43.0"
        ));

        mock.verify();
    }

    #[test]
    fn test_brew_formula_detect_not_installed() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["list", "--formula", "ripgrep"],
            CommandOutput::failure("Error: No such keg"),
        ));

        let ctx = Context::with_command_runner("test", mock.clone());
        let formula = Formula::new("ripgrep");

        let state = formula.detect(&ctx).unwrap();
        assert!(matches!(state, ResourceState::Absent));

        mock.verify();
    }

    #[test]
    fn test_brew_formula_diff_needs_install() {
        let formula = Formula::new("fzf");
        let change = formula.diff(&ResourceState::Absent).unwrap();
        assert!(matches!(change, Change::Create));
    }

    #[test]
    fn test_brew_formula_diff_already_installed() {
        let formula = Formula::new("fzf");
        let change = formula
            .diff(&ResourceState::present_with_version("0.45.0"))
            .unwrap();
        assert!(matches!(change, Change::NoOp));
    }

    #[test]
    fn test_brew_formula_apply_install() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["install", "neovim"],
            CommandOutput::success("==> Installing neovim"),
        ));

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let formula = Formula::new("neovim");

        formula.apply(&Change::Create, &ctx).unwrap();
        mock.verify();
    }

    #[test]
    fn test_brew_formula_apply_install_failure() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["install", "nonexistent"],
            CommandOutput::failure("Error: No formulae found"),
        ));

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let formula = Formula::new("nonexistent");

        let result = formula.apply(&Change::Create, &ctx);
        assert!(result.is_err());

        mock.verify();
    }
}
