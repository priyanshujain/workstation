//! Testing utilities for ws
//!
//! This module provides mock implementations and test helpers.

use crate::command::{CommandOutput, CommandRunner};
use crate::Result;
use std::collections::VecDeque;
use std::sync::Mutex;

/// A mock command runner for testing
///
/// Records all commands that were run and returns pre-configured responses.
///
/// # Example
///
/// ```rust
/// use ws_core::testing::MockCommandRunner;
/// use ws_core::command::{CommandRunner, CommandOutput};
///
/// let mock = MockCommandRunner::new()
///     .expect("brew", &["list", "--formula", "git"], CommandOutput::success("git 2.40.0"));
///
/// // Use mock in tests...
/// let output = mock.run("brew", &["list", "--formula", "git"]).unwrap();
/// assert!(output.success);
/// assert_eq!(output.stdout, "git 2.40.0");
///
/// // Verify all expected commands were called
/// mock.verify();
/// ```
#[derive(Debug)]
pub struct MockCommandRunner {
    /// Queue of expected commands and their responses
    expectations: Mutex<VecDeque<Expectation>>,
    /// Record of all commands that were actually called
    calls: Mutex<Vec<Call>>,
    /// If true, unexpected commands will panic
    strict: bool,
    /// Default response for unexpected commands (if not strict)
    default_response: CommandOutput,
}

#[derive(Debug, Clone)]
struct Expectation {
    program: String,
    args: Vec<String>,
    response: CommandOutput,
}

#[derive(Debug, Clone)]
struct Call {
    program: String,
    args: Vec<String>,
}

impl MockCommandRunner {
    /// Create a new mock command runner
    pub fn new() -> Self {
        Self {
            expectations: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            strict: true,
            default_response: CommandOutput::failure("unexpected command"),
        }
    }

    /// Add an expected command with its response
    ///
    /// Expectations are matched in order - the first matching expectation is used.
    pub fn expect(self, program: &str, args: &[&str], response: CommandOutput) -> Self {
        self.expectations.lock().unwrap().push_back(Expectation {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            response,
        });
        self
    }

    /// Allow unexpected commands (returns default failure response)
    pub fn lenient(mut self) -> Self {
        self.strict = false;
        self
    }

    /// Set the default response for unexpected commands (when lenient)
    pub fn with_default_response(mut self, response: CommandOutput) -> Self {
        self.default_response = response;
        self
    }

    /// Get all commands that were called
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| (c.program.clone(), c.args.clone()))
            .collect()
    }

    /// Verify that all expected commands were called
    ///
    /// Panics if there are remaining expectations that weren't matched.
    pub fn verify(&self) {
        let remaining = self.expectations.lock().unwrap();
        if !remaining.is_empty() {
            let expected: Vec<_> = remaining
                .iter()
                .map(|e| format!("{} {:?}", e.program, e.args))
                .collect();
            panic!(
                "MockCommandRunner: {} expected commands were not called:\n  {}",
                remaining.len(),
                expected.join("\n  ")
            );
        }
    }

    /// Check if a command was called with specific arguments
    pub fn was_called(&self, program: &str, args: &[&str]) -> bool {
        self.calls.lock().unwrap().iter().any(|c| {
            c.program == program && c.args == args.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        })
    }

    /// Get the number of times any command was called
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Default for MockCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRunner for MockCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        // Record the call
        self.calls.lock().unwrap().push(Call {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });

        // Find matching expectation
        let mut expectations = self.expectations.lock().unwrap();
        let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        // Find and remove the first matching expectation
        if let Some(pos) = expectations
            .iter()
            .position(|e| e.program == program && e.args == args_vec)
        {
            let expectation = expectations.remove(pos).unwrap();
            return Ok(expectation.response);
        }

        // No matching expectation found
        if self.strict {
            panic!(
                "MockCommandRunner: unexpected command: {} {:?}\nExpected one of:\n  {}",
                program,
                args,
                expectations
                    .iter()
                    .map(|e| format!("{} {:?}", e.program, e.args))
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
        }

        Ok(self.default_response.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_command_runner_basic() {
        let mock = MockCommandRunner::new().expect(
            "brew",
            &["list"],
            CommandOutput::success("git\nripgrep"),
        );

        let output = mock.run("brew", &["list"]).unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, "git\nripgrep");

        mock.verify();
    }

    #[test]
    fn test_mock_command_runner_multiple_expectations() {
        let mock = MockCommandRunner::new()
            .expect(
                "brew",
                &["list", "--formula", "git"],
                CommandOutput::success("git 2.40.0"),
            )
            .expect("brew", &["install", "ripgrep"], CommandOutput::success(""));

        // Can call in any order
        let output1 = mock.run("brew", &["install", "ripgrep"]).unwrap();
        assert!(output1.success);

        let output2 = mock.run("brew", &["list", "--formula", "git"]).unwrap();
        assert!(output2.success);
        assert_eq!(output2.stdout, "git 2.40.0");

        mock.verify();
    }

    #[test]
    fn test_mock_command_runner_was_called() {
        let mock =
            MockCommandRunner::new().expect("echo", &["hello"], CommandOutput::success("hello"));

        mock.run("echo", &["hello"]).unwrap();

        assert!(mock.was_called("echo", &["hello"]));
        assert!(!mock.was_called("echo", &["world"]));
    }

    #[test]
    fn test_mock_command_runner_lenient() {
        let mock = MockCommandRunner::new()
            .lenient()
            .with_default_response(CommandOutput::failure("not found"));

        let output = mock.run("unknown", &["command"]).unwrap();
        assert!(!output.success);
        assert_eq!(output.stderr, "not found");
    }

    #[test]
    #[should_panic(expected = "unexpected command")]
    fn test_mock_command_runner_strict_panics() {
        let mock = MockCommandRunner::new();
        let _ = mock.run("unexpected", &["command"]);
    }

    #[test]
    #[should_panic(expected = "expected commands were not called")]
    fn test_mock_command_runner_verify_panics() {
        let mock = MockCommandRunner::new().expect(
            "brew",
            &["install", "git"],
            CommandOutput::success(""),
        );

        // Don't call the expected command
        mock.verify();
    }
}
