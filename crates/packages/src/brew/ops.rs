//! Homebrew uninstall operations: removal, autoremove, verification, dependents.

use crate::brew::info::PkgKind;
use wsctl_core::{CommandRunner, Error, Result};

/// `brew uninstall <name>`. Fails if other installed packages depend on it.
pub fn uninstall_formula(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let out = runner.run("brew", &["uninstall", name])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: format!("brew uninstall {name}"),
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// `brew uninstall --cask --zap <name>` — removes app and associated prefs/data.
pub fn uninstall_cask_zap(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let out = runner.run("brew", &["uninstall", "--cask", "--zap", name])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: format!("brew uninstall --cask --zap {name}"),
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// `brew bundle dump --file=<path>` — write current state to a new Brewfile.
pub fn bundle_dump(runner: &dyn CommandRunner, path: &std::path::Path) -> Result<()> {
    let path_str = path.to_string_lossy();
    let file_arg = format!("--file={path_str}");
    let out = runner.run("brew", &["bundle", "dump", &file_arg])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: format!("brew bundle dump {file_arg}"),
            stderr: out.stderr,
        });
    }
    Ok(())
}

/// `brew autoremove` — removes orphaned dependencies (no-op if none).
pub fn autoremove(runner: &dyn CommandRunner) -> Result<String> {
    let out = runner.run("brew", &["autoremove"])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: "brew autoremove".into(),
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

/// `brew list --formula/--cask <name>` — returns Ok(true) if still installed.
pub fn is_installed(runner: &dyn CommandRunner, name: &str, kind: PkgKind) -> Result<bool> {
    let kind_flag = match kind {
        PkgKind::Formula => "--formula",
        PkgKind::Cask => "--cask",
    };
    let out = runner.run("brew", &["list", kind_flag, name])?;
    Ok(out.success)
}

/// `brew uses --installed <name>` — returns names of installed packages that
/// directly depend on `name`. Only meaningful for formulae.
pub fn dependents(runner: &dyn CommandRunner, name: &str) -> Result<Vec<String>> {
    let out = runner.run("brew", &["uses", "--installed", name])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: format!("brew uses --installed {name}"),
            stderr: out.stderr,
        });
    }
    Ok(out
        .stdout
        .split_whitespace()
        .map(|s| s.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wsctl_core::CommandOutput;
    use wsctl_core::testing::MockCommandRunner;

    #[test]
    fn uninstall_formula_invokes_brew() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["uninstall", "fzf"],
            CommandOutput::success(""),
        ));
        uninstall_formula(mock.as_ref(), "fzf").unwrap();
        mock.verify();
    }

    #[test]
    fn uninstall_formula_surfaces_failure() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["uninstall", "pcre2"],
            CommandOutput::failure("Refusing to uninstall: ripgrep depends on it"),
        ));
        let err = uninstall_formula(mock.as_ref(), "pcre2").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("depends on it"), "got: {msg}");
        mock.verify();
    }

    #[test]
    fn uninstall_cask_uses_zap_flag() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["uninstall", "--cask", "--zap", "ghostty"],
            CommandOutput::success(""),
        ));
        uninstall_cask_zap(mock.as_ref(), "ghostty").unwrap();
        mock.verify();
    }

    #[test]
    fn bundle_dump_passes_file_flag() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["bundle", "dump", "--file=/tmp/Brewfile"],
            CommandOutput::success(""),
        ));
        bundle_dump(mock.as_ref(), std::path::Path::new("/tmp/Brewfile")).unwrap();
        mock.verify();
    }

    #[test]
    fn autoremove_returns_stdout() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["autoremove"],
            CommandOutput::success("Uninstalling pcre2\n"),
        ));
        let out = autoremove(mock.as_ref()).unwrap();
        assert!(out.contains("pcre2"));
        mock.verify();
    }

    #[test]
    fn is_installed_distinguishes_kind() {
        let mock = Arc::new(
            MockCommandRunner::new()
                .expect(
                    "brew",
                    &["list", "--formula", "fzf"],
                    CommandOutput::success(""),
                )
                .expect(
                    "brew",
                    &["list", "--cask", "vlc"],
                    CommandOutput::failure(""),
                ),
        );
        assert!(is_installed(mock.as_ref(), "fzf", PkgKind::Formula).unwrap());
        assert!(!is_installed(mock.as_ref(), "vlc", PkgKind::Cask).unwrap());
        mock.verify();
    }

    #[test]
    fn dependents_splits_whitespace_output() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["uses", "--installed", "pcre2"],
            CommandOutput::success("ripgrep\nfd\nbat\n"),
        ));
        let deps = dependents(mock.as_ref(), "pcre2").unwrap();
        assert_eq!(deps, vec!["ripgrep", "fd", "bat"]);
        mock.verify();
    }

    #[test]
    fn dependents_empty_when_no_uses() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["uses", "--installed", "orphan"],
            CommandOutput::success(""),
        ));
        let deps = dependents(mock.as_ref(), "orphan").unwrap();
        assert!(deps.is_empty());
        mock.verify();
    }
}
