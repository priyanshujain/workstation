//! Homebrew cask resource (GUI applications).

use wsctl_core::{Change, Context, Resource, ResourceId, ResourceState, Result};

#[derive(Debug, Clone)]
pub struct Cask {
    pub name: String,
}

impl Cask {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    fn is_installed(&self, ctx: &Context) -> Result<bool> {
        let output = ctx.run_command("brew", &["list", "--cask", &self.name])?;
        Ok(output.success)
    }

    fn installed_version(&self, ctx: &Context) -> Result<Option<String>> {
        let output = ctx.run_command("brew", &["list", "--cask", "--versions", &self.name])?;

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

impl Resource for Cask {
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
                    return Err(wsctl_core::Error::CommandFailed {
                        command: format!("brew install --cask {}", self.name),
                        stderr: output.stderr,
                    });
                }
                Ok(())
            }
            Change::Remove => {
                if ctx.verbose > 0 {
                    tracing::info!("Uninstalling cask: {}", self.name);
                }

                let output = ctx.run_command("brew", &["uninstall", "--cask", &self.name])?;

                if !output.success {
                    return Err(wsctl_core::Error::CommandFailed {
                        command: format!("brew uninstall --cask {}", self.name),
                        stderr: output.stderr,
                    });
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
    use wsctl_core::{CommandOutput, MockCommandRunner};

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
        let cask = Cask::new("raycast");

        let state = cask.detect(&ctx).unwrap();
        assert!(matches!(
            state,
            ResourceState::Present { version: Some(v) } if v == "1.65.0"
        ));

        mock.verify();
    }

    #[test]
    fn test_brew_cask_apply_install() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["install", "--cask", "docker"],
            CommandOutput::success("==> Installing docker"),
        ));

        let ctx = Context::with_command_runner("test", mock.clone()).with_verbose(0);
        let cask = Cask::new("docker");

        cask.apply(&Change::Create, &ctx).unwrap();
        mock.verify();
    }
}
