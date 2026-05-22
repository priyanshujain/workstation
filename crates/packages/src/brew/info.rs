//! Fetch installed Homebrew package metadata (formulae + casks).
//!
//! Calls `brew info --json=v2 --installed` once for all metadata, plus `du`
//! against the Cellar/Caskroom paths for on-disk size.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use wsctl_core::{CommandRunner, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PkgKind {
    Formula,
    Cask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub name: String,
    pub kind: PkgKind,
    pub version: Option<String>,
    /// On-disk size in bytes. `0` means unknown or not yet measured.
    pub size_bytes: u64,
    /// Unqualified names of direct dependencies.
    pub deps: Vec<String>,
    /// Unix timestamp (seconds) of installation, if known.
    pub installed_at: Option<u64>,
}

/// Run `brew info --json=v2 --installed` and parse the result.
pub fn fetch_installed(runner: &dyn CommandRunner) -> Result<Vec<InstalledPackage>> {
    let out = runner.run("brew", &["info", "--json=v2", "--installed"])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: "brew info --json=v2 --installed".into(),
            stderr: out.stderr,
        });
    }
    parse_installed(&out.stdout)
}

/// Run `brew --prefix` to discover where Homebrew is installed.
pub fn brew_prefix(runner: &dyn CommandRunner) -> Result<PathBuf> {
    let out = runner.run("brew", &["--prefix"])?;
    if !out.success {
        return Err(Error::CommandFailed {
            command: "brew --prefix".into(),
            stderr: out.stderr,
        });
    }
    Ok(PathBuf::from(out.stdout.trim()))
}

/// Compute on-disk size for each package in one `du -sk` invocation, in place.
///
/// Best-effort: packages whose directory is missing keep `size_bytes = 0`.
pub fn attach_sizes(prefix: &Path, packages: &mut [InstalledPackage]) {
    let mut indices_by_path: Vec<(PathBuf, usize)> = Vec::new();
    for (i, pkg) in packages.iter().enumerate() {
        let path = match pkg.kind {
            PkgKind::Formula => prefix.join("Cellar").join(&pkg.name),
            PkgKind::Cask => prefix.join("Caskroom").join(&pkg.name),
        };
        if path.exists() {
            indices_by_path.push((path, i));
        }
    }
    if indices_by_path.is_empty() {
        return;
    }
    let mut args: Vec<String> = vec!["-sk".into()];
    for (path, _) in &indices_by_path {
        args.push(path.to_string_lossy().into_owned());
    }
    let Ok(output) = Command::new("du").args(&args).output() else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(2, '\t');
        let Some(kb) = parts.next().and_then(|s| s.trim().parse::<u64>().ok()) else {
            continue;
        };
        let Some(path_str) = parts.next() else {
            continue;
        };
        if let Some((_, i)) = indices_by_path
            .iter()
            .find(|(p, _)| p.to_string_lossy() == path_str)
        {
            packages[*i].size_bytes = kb * 1024;
        }
    }
}

fn parse_installed(json: &str) -> Result<Vec<InstalledPackage>> {
    let info: BrewInfoV2 = serde_json::from_str(json)
        .map_err(|e| Error::Other(anyhow::anyhow!("brew info parse failed: {e}")))?;
    let mut pkgs = Vec::with_capacity(info.formulae.len() + info.casks.len());
    for f in info.formulae {
        let installed = f.installed.into_iter().next();
        let deps = installed
            .as_ref()
            .map(|i| {
                i.runtime_dependencies
                    .iter()
                    .map(|d| short_name(&d.full_name))
                    .collect()
            })
            .unwrap_or_default();
        pkgs.push(InstalledPackage {
            name: f.name,
            kind: PkgKind::Formula,
            version: installed.as_ref().map(|i| i.version.clone()),
            size_bytes: 0,
            deps,
            installed_at: installed.and_then(|i| i.time),
        });
    }
    for c in info.casks {
        let mut deps = c.depends_on.formula.clone();
        deps.extend(c.depends_on.cask.clone());
        pkgs.push(InstalledPackage {
            name: c.token,
            kind: PkgKind::Cask,
            version: c.installed,
            size_bytes: 0,
            deps,
            installed_at: c.installed_time,
        });
    }
    Ok(pkgs)
}

fn short_name(full: &str) -> String {
    full.rsplit('/').next().unwrap_or(full).to_string()
}

#[derive(Deserialize)]
struct BrewInfoV2 {
    #[serde(default)]
    formulae: Vec<FormulaInfo>,
    #[serde(default)]
    casks: Vec<CaskInfo>,
}

#[derive(Deserialize)]
struct FormulaInfo {
    name: String,
    #[serde(default)]
    installed: Vec<FormulaInstalled>,
}

#[derive(Deserialize)]
struct FormulaInstalled {
    version: String,
    #[serde(default)]
    time: Option<u64>,
    #[serde(default)]
    runtime_dependencies: Vec<RuntimeDep>,
}

#[derive(Deserialize)]
struct RuntimeDep {
    full_name: String,
}

#[derive(Deserialize)]
struct CaskInfo {
    token: String,
    #[serde(default)]
    installed: Option<String>,
    #[serde(default)]
    installed_time: Option<u64>,
    #[serde(default)]
    depends_on: CaskDeps,
}

#[derive(Deserialize, Default)]
struct CaskDeps {
    #[serde(default)]
    formula: Vec<String>,
    #[serde(default)]
    cask: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsctl_core::testing::MockCommandRunner;
    use wsctl_core::CommandOutput;
    use std::sync::Arc;

    const SAMPLE_JSON: &str = r#"{
        "formulae": [
            {
                "name": "ripgrep",
                "installed": [
                    {
                        "version": "14.0.3",
                        "time": 1700000000,
                        "runtime_dependencies": [
                            {"full_name": "pcre2", "version": "10.42"}
                        ]
                    }
                ]
            },
            {
                "name": "maestro",
                "installed": [
                    {
                        "version": "1.0",
                        "time": 1710000000,
                        "runtime_dependencies": [
                            {"full_name": "openjdk@17", "version": "17"},
                            {"full_name": "mobile-dev-inc/tap/idb-companion", "version": "1.0"}
                        ]
                    }
                ]
            }
        ],
        "casks": [
            {
                "token": "ghostty",
                "installed": "1.0.1",
                "installed_time": 1720000000,
                "depends_on": {"formula": ["libssh"]}
            }
        ]
    }"#;

    #[test]
    fn parses_formulae_and_casks() {
        let pkgs = parse_installed(SAMPLE_JSON).unwrap();
        assert_eq!(pkgs.len(), 3);

        let rg = &pkgs[0];
        assert_eq!(rg.name, "ripgrep");
        assert_eq!(rg.kind, PkgKind::Formula);
        assert_eq!(rg.version.as_deref(), Some("14.0.3"));
        assert_eq!(rg.installed_at, Some(1700000000));
        assert_eq!(rg.deps, vec!["pcre2"]);

        let maestro = &pkgs[1];
        assert_eq!(maestro.name, "maestro");
        assert_eq!(maestro.deps, vec!["openjdk@17", "idb-companion"]);

        let ghostty = &pkgs[2];
        assert_eq!(ghostty.name, "ghostty");
        assert_eq!(ghostty.kind, PkgKind::Cask);
        assert_eq!(ghostty.version.as_deref(), Some("1.0.1"));
        assert_eq!(ghostty.deps, vec!["libssh"]);
    }

    #[test]
    fn parse_tolerates_missing_optional_fields() {
        let json = r#"{"formulae": [{"name": "x", "installed": [{"version": "1"}]}], "casks": []}"#;
        let pkgs = parse_installed(json).unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].deps.len(), 0);
        assert!(pkgs[0].installed_at.is_none());
    }

    #[test]
    fn parse_handles_empty_lists() {
        let pkgs = parse_installed(r#"{"formulae": [], "casks": []}"#).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn fetch_installed_invokes_brew() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["info", "--json=v2", "--installed"],
            CommandOutput::success(SAMPLE_JSON),
        ));
        let pkgs = fetch_installed(mock.as_ref()).unwrap();
        assert_eq!(pkgs.len(), 3);
        mock.verify();
    }

    #[test]
    fn brew_prefix_trims_output() {
        let mock = Arc::new(MockCommandRunner::new().expect(
            "brew",
            &["--prefix"],
            CommandOutput::success("/opt/homebrew\n"),
        ));
        let prefix = brew_prefix(mock.as_ref()).unwrap();
        assert_eq!(prefix, PathBuf::from("/opt/homebrew"));
        mock.verify();
    }
}
