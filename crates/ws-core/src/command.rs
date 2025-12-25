//! Command execution abstraction for testability
//!
//! This module provides a trait for running shell commands, allowing
//! real execution in production and mocked execution in tests.

use crate::Result;
use std::process::Command;

/// Output from a command execution
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Whether the command exited successfully (exit code 0)
    pub success: bool,
    /// Standard output as a string
    pub stdout: String,
    /// Standard error as a string
    pub stderr: String,
    /// Exit code if available
    pub code: Option<i32>,
}

impl CommandOutput {
    /// Create a successful output with the given stdout
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
            code: Some(0),
        }
    }

    /// Create a failed output with the given stderr
    pub fn failure(stderr: impl Into<String>) -> Self {
        Self {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
            code: Some(1),
        }
    }

    /// Create a failed output with a specific exit code
    pub fn failure_with_code(stderr: impl Into<String>, code: i32) -> Self {
        Self {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
            code: Some(code),
        }
    }
}

/// Trait for running shell commands
///
/// This abstraction allows us to:
/// - Run real commands in production via `SystemCommandRunner`
/// - Mock command execution in tests via `MockCommandRunner`
pub trait CommandRunner: Send + Sync + std::fmt::Debug {
    /// Run a command with the given program and arguments
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput>;
}

/// Real command runner that executes via std::process::Command
#[derive(Debug, Clone, Default)]
pub struct SystemCommandRunner;

impl SystemCommandRunner {
    /// Create a new system command runner
    pub fn new() -> Self {
        Self
    }
}

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let output = Command::new(program).args(args).output()?;

        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_output_success() {
        let output = CommandOutput::success("hello");
        assert!(output.success);
        assert_eq!(output.stdout, "hello");
        assert_eq!(output.code, Some(0));
    }

    #[test]
    fn test_command_output_failure() {
        let output = CommandOutput::failure("error message");
        assert!(!output.success);
        assert_eq!(output.stderr, "error message");
        assert_eq!(output.code, Some(1));
    }

    #[test]
    fn test_system_command_runner_echo() {
        let runner = SystemCommandRunner::new();
        let output = runner.run("echo", &["hello"]).unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.trim(), "hello");
    }
}
