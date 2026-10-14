use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use console::style;

use super::apply::{Target, output};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Pass,
    Warn,
    Fail,
}

struct Check {
    name: &'static str,
    level: Level,
    summary: String,
    details: Vec<String>,
}

impl Check {
    fn pass(name: &'static str, summary: impl Into<String>) -> Self {
        Self {
            name,
            level: Level::Pass,
            summary: summary.into(),
            details: Vec::new(),
        }
    }

    fn warn(name: &'static str, summary: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            name,
            level: Level::Warn,
            summary: summary.into(),
            details,
        }
    }

    fn fail(name: &'static str, summary: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            name,
            level: Level::Fail,
            summary: summary.into(),
            details,
        }
    }

    fn from_result(name: &'static str, result: Result<Check>) -> Self {
        result.unwrap_or_else(|e| Check::fail(name, format!("{e:#}"), Vec::new()))
    }
}

pub fn run(target: &Target) -> Result<bool> {
    let checks = platform_checks(target);
    for check in &checks {
        let mark = match check.level {
            Level::Pass => style("✓").green(),
            Level::Warn => style("!").yellow(),
            Level::Fail => style("×").red(),
        };
        println!("{mark} {:<14} {}", check.name, check.summary);
        for line in &check.details {
            let line = match check.level {
                Level::Pass => style(line.as_str()).dim(),
                Level::Warn => style(line.as_str()).yellow(),
                Level::Fail => style(line.as_str()).red(),
            };
            println!("    {line}");
        }
    }
    Ok(checks.iter().all(|c| c.level != Level::Fail))
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

fn user() -> String {
    std::env::var("USER").unwrap_or_default()
}

fn built(attr: &str) -> Result<String> {
    Ok(
        output("nix", &["build", "--no-link", "--print-out-paths", attr])?
            .trim()
            .to_string(),
    )
}

fn resolved(path: &Path) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()
        .map(|p| p.display().to_string())
}

fn generation(name: &'static str, attr: &str, active: &Path, apply_hint: &str) -> Result<Check> {
    let wanted = built(attr)?;
    match resolved(active) {
        None => Ok(Check::fail(
            name,
            "never activated",
            vec![apply_hint.to_string()],
        )),
        Some(current) if current == wanted => Ok(Check::pass(name, "matches the flake")),
        Some(current) => Ok(Check::warn(
            name,
            "differs from the flake, run wsctl apply",
            vec![format!("active {current}"), format!("flake  {wanted}")],
        )),
    }
}

fn home_manager(target: &Target) -> Check {
    let attr = format!(
        "{}#homeConfigurations.\"{}@{}\".activationPackage",
        target.flake,
        user(),
        target.host
    );
    let active = home().join(".local/state/nix/profiles/home-manager");
    Check::from_result(
        "home-manager",
        generation("home-manager", &attr, &active, "run wsctl apply"),
    )
}

fn stray_binaries() -> Check {
    let mut dirs = vec![
        home().join(".local/bin"),
        home().join("go/bin"),
        home().join(".cargo/bin"),
        home().join(".bun/bin"),
    ];
    if let Ok(prefix) = output("npm", &["prefix", "-g"]) {
        let bin = PathBuf::from(prefix.trim()).join("bin");
        if !bin.starts_with("/nix/store") {
            dirs.push(bin);
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if resolved(&path).is_some_and(|p| p.starts_with("/nix/store")) {
                continue;
            }
            found.push(path.display().to_string());
        }
    }
    found.sort();
    if found.is_empty() {
        Check::pass("stray binaries", "none outside a package manager")
    } else {
        Check::warn(
            "stray binaries",
            format!("{} installed outside any package manager", found.len()),
            found,
        )
    }
}

#[cfg(target_os = "linux")]
fn eval_names(attr: &str) -> Result<BTreeSet<String>> {
    let raw = output("nix", &["eval", "--json", attr])?;
    Ok(serde_json::from_str(&raw)?)
}

fn package_check(
    name: &'static str,
    present: &BTreeSet<String>,
    explicit: &BTreeSet<String>,
    declared: &BTreeSet<String>,
) -> Check {
    let missing: Vec<String> = declared.difference(present).cloned().collect();
    let undeclared: Vec<String> = explicit.difference(declared).cloned().collect();
    if !missing.is_empty() {
        let mut details: Vec<String> = missing.iter().map(|p| format!("missing {p}")).collect();
        details.extend(undeclared.iter().map(|p| format!("undeclared {p}")));
        return Check::fail(
            name,
            format!(
                "{} declared but not installed, {} undeclared",
                missing.len(),
                undeclared.len()
            ),
            details,
        );
    }
    if !undeclared.is_empty() {
        return Check::warn(
            name,
            format!("{} installed but not declared", undeclared.len()),
            undeclared,
        );
    }
    Check::pass(name, format!("{} declared, all installed", declared.len()))
}

fn platform_checks(target: &Target) -> Vec<Check> {
    let mut checks = os::checks(target);
    checks.push(home_manager(target));
    checks.push(stray_binaries());
    checks
}

#[cfg(target_os = "linux")]
mod os {
    use std::collections::BTreeSet;
    use std::path::Path;

    use anyhow::Result;

    use super::{Check, Target, eval_names, generation, output, package_check, resolved};
    use crate::commands::apply::linux::{managed_files, masked_units, preset_units};

    const PROFILE: &str = "/nix/var/nix/profiles/system-manager-profiles/system-manager";

    // Files the installer and the running system own; editing them is expected.
    const MACHINE_FILES: &[&str] = &[
        "/etc/fstab",
        "/etc/group",
        "/etc/gshadow",
        "/etc/hosts",
        "/etc/passwd",
        "/etc/resolv.conf",
        "/etc/shadow",
        "/etc/shells",
        "/etc/subgid",
        "/etc/subuid",
    ];

    // The Nix installer hooks the daemon profile into these.
    const NIX_INSTALLER_FILES: &[&str] = &["/etc/bash.bashrc", "/etc/zsh/zshrc"];

    pub fn checks(target: &Target) -> Vec<Check> {
        let managed = managed_files().unwrap_or_default();
        let attr = format!("{}#systemConfigs.{}", target.flake, target.host);
        vec![
            Check::from_result(
                "system-manager",
                generation(
                    "system-manager",
                    &attr,
                    Path::new(PROFILE),
                    "run wsctl apply",
                ),
            ),
            Check::from_result("/etc drift", etc_drift(&managed)),
            Check::from_result("modified /etc", modified_etc(&managed)),
            Check::from_result("pacnew", pacnew(&managed)),
            Check::from_result("failed units", failed_units()),
            Check::from_result("system units", system_units(&managed)),
            Check::from_result("user units", user_units()),
            Check::from_result("pacman", pacman(target)),
            Check::from_result("AUR", aur(target)),
            Check::from_result("users", users(target)),
            Check::from_result("groups", groups()),
        ]
    }

    fn etc_drift(managed: &BTreeSet<String>) -> Result<Check> {
        if managed.is_empty() {
            return Ok(Check::fail(
                "/etc drift",
                "no system-manager state",
                vec!["run wsctl apply".into()],
            ));
        }
        let copies = copied_files();
        let mut drift = Vec::new();
        for file in managed {
            let path = Path::new(file);
            let meta = match path.symlink_metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    drift.push(format!("missing {file}"));
                    continue;
                }
                Err(_) => continue,
            };
            if let Some(source) = copies.get(file.as_str()) {
                let same = match (std::fs::read(path), std::fs::read(source)) {
                    (Ok(live), Ok(wanted)) => live == wanted,
                    _ => true,
                };
                if meta.file_type().is_symlink() || !same {
                    drift.push(format!("edited {file}"));
                }
            } else if !resolved(path).is_some_and(|p| p.starts_with("/nix/store")) {
                drift.push(format!("replaced {file}"));
            }
        }
        Ok(if drift.is_empty() {
            Check::pass(
                "/etc drift",
                format!("{} managed files in place", managed.len()),
            )
        } else {
            Check::fail(
                "/etc drift",
                format!("{} managed files changed", drift.len()),
                drift,
            )
        })
    }

    /// Entries the active generation copies into /etc rather than links, with their sources.
    fn copied_files() -> std::collections::BTreeMap<String, String> {
        let raw = std::fs::read_to_string(format!("{PROFILE}/etcFiles/etcFiles.json"))
            .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let mut copies = std::collections::BTreeMap::new();
        if let Some(entries) = json["entries"].as_object() {
            for entry in entries.values() {
                let (Some(mode), Some(target), Some(source)) = (
                    entry["mode"].as_str(),
                    entry["target"].as_str(),
                    entry["source"].as_str(),
                ) else {
                    continue;
                };
                if mode != "symlink" {
                    copies.insert(format!("/etc/{target}"), source.to_string());
                }
            }
        }
        copies
    }

    fn modified_etc(managed: &BTreeSet<String>) -> Result<Check> {
        let raw = output("pacman", &["-Qii"])?;
        let mut modified: Vec<String> = raw
            .lines()
            .filter_map(|l| l.trim().strip_suffix(" [modified]"))
            .map(|l| l.trim_start_matches("Backup Files    :").trim().to_string())
            .filter(|f| {
                !MACHINE_FILES.contains(&f.as_str())
                    && !NIX_INSTALLER_FILES.contains(&f.as_str())
                    && !managed.contains(f)
            })
            .collect();
        modified.sort();
        modified.dedup();
        Ok(if modified.is_empty() {
            Check::pass("modified /etc", "no package config edited by hand")
        } else {
            Check::warn(
                "modified /etc",
                format!(
                    "{} package config files edited outside the flake",
                    modified.len()
                ),
                modified,
            )
        })
    }

    fn pacnew(managed: &BTreeSet<String>) -> Result<Check> {
        let mut found = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("/etc")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path.display().to_string();
                let Some(base) = name
                    .strip_suffix(".pacnew")
                    .or_else(|| name.strip_suffix(".pacsave"))
                else {
                    continue;
                };
                if !managed.contains(base) {
                    found.push(name);
                }
            }
        }
        found.sort();
        Ok(if found.is_empty() {
            Check::pass("pacnew", "no unmerged .pacnew or .pacsave files")
        } else {
            Check::warn(
                "pacnew",
                format!("{} unmerged .pacnew or .pacsave files", found.len()),
                found,
            )
        })
    }

    fn failed_units() -> Result<Check> {
        let mut failed = Vec::new();
        for (scope, args) in [
            ("system", vec!["--failed", "--no-legend", "--plain"]),
            ("user", vec!["--user", "--failed", "--no-legend", "--plain"]),
        ] {
            for line in output("systemctl", &args)?.lines() {
                if let Some(unit) = line.split_whitespace().next() {
                    failed.push(format!("{scope} {unit}"));
                }
            }
        }
        Ok(if failed.is_empty() {
            Check::pass("failed units", "none")
        } else {
            Check::fail("failed units", format!("{} failed", failed.len()), failed)
        })
    }

    fn system_units(managed: &BTreeSet<String>) -> Result<Check> {
        let masked = masked_units().unwrap_or_default();
        let managed_units: BTreeSet<&str> = managed
            .iter()
            .filter_map(|f| f.rsplit('/').next())
            .chain(masked.iter().map(String::as_str))
            .collect();
        let mut presets = BTreeSet::new();
        for line in output("systemctl", &["list-unit-files", "--no-legend", "--plain"])?.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.get(2) == Some(&"enabled") {
                presets.insert(cols[0].to_string());
            }
        }
        let preset_enabled = |unit: &str| {
            let canonical = output("systemctl", &["show", "-P", "Id", unit])
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let template = match canonical.split_once('@') {
                Some((head, rest)) => format!("{head}@{}", &rest[rest.find('.').unwrap_or(0)..]),
                None => canonical.clone(),
            };
            presets.contains(&canonical) || presets.contains(&template)
        };
        let mut not_enabled = Vec::new();
        for unit in preset_units() {
            let state = output("systemctl", &["is-enabled", &unit]).unwrap_or_default();
            if state.trim() != "enabled" {
                not_enabled.push(format!("declared but {} {unit}", state.trim()));
            }
        }
        if !not_enabled.is_empty() {
            return Ok(Check::fail(
                "system units",
                format!(
                    "{} declared units not enabled, run wsctl apply",
                    not_enabled.len()
                ),
                not_enabled,
            ));
        }
        let mut undeclared = Vec::new();
        for state in ["enabled", "masked"] {
            let raw = output(
                "systemctl",
                &[
                    "list-unit-files",
                    "--state",
                    state,
                    "--no-legend",
                    "--plain",
                ],
            )?;
            for line in raw.lines() {
                let cols: Vec<&str> = line.split_whitespace().collect();
                let (Some(unit), Some(preset)) = (cols.first(), cols.get(2)) else {
                    continue;
                };
                if *preset == "enabled" || managed_units.contains(unit) || preset_enabled(unit) {
                    continue;
                }
                undeclared.push(format!("{state} {unit}"));
            }
        }
        Ok(if undeclared.is_empty() {
            Check::pass("system units", "every enabled unit is declared or a preset")
        } else {
            Check::warn(
                "system units",
                format!(
                    "{} enabled or masked by hand, not declared",
                    undeclared.len()
                ),
                undeclared,
            )
        })
    }

    fn user_units() -> Result<Check> {
        let dir = super::home().join(".config/systemd/user");
        let presets: BTreeSet<String> = output(
            "systemctl",
            &["--user", "list-unit-files", "--no-legend", "--plain"],
        )?
        .lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            (cols.get(2) == Some(&"enabled")).then(|| cols[0].to_string())
        })
        .collect();
        let mut manual = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for wants in entries.flatten() {
                let name = wants.file_name().to_string_lossy().into_owned();
                if !(name.ends_with(".wants") || name.ends_with(".requires")) {
                    continue;
                }
                let Ok(links) = std::fs::read_dir(wants.path()) else {
                    continue;
                };
                for link in links.flatten() {
                    let immediate = std::fs::read_link(link.path()).unwrap_or_default();
                    let path = immediate.display().to_string();
                    let hm = path.starts_with("/nix/store") || path.contains("home-manager-files");
                    let unit = link.file_name().to_string_lossy().into_owned();
                    if !hm && !presets.contains(&unit) {
                        manual.push(format!("{name}/{}", link.file_name().to_string_lossy()));
                    }
                }
            }
        }
        manual.sort();
        Ok(if manual.is_empty() {
            Check::pass("user units", "every enabled user unit is declared")
        } else {
            Check::warn(
                "user units",
                format!("{} enabled by hand, not declared", manual.len()),
                manual,
            )
        })
    }

    fn installed(args: &[&str]) -> Result<BTreeSet<String>> {
        Ok(output("pacman", args)?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn pacman(target: &Target) -> Result<Check> {
        let declared = eval_names(&format!(
            "{}#systemConfigs.{}.config.pacman.packages",
            target.flake, target.host
        ))?;
        Ok(package_check(
            "pacman",
            &installed(&["-Qq"])?,
            &installed(&["-Qqen"])?,
            &declared,
        ))
    }

    fn aur(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &[
                "eval",
                "--json",
                &format!(
                    "{}#systemConfigs.{}.config.aur.packages",
                    target.flake, target.host
                ),
                "--apply",
                "builtins.attrNames",
            ],
        )?;
        let declared: BTreeSet<String> = serde_json::from_str(&raw)?;
        let foreign = installed(&["-Qqm"])?;
        Ok(package_check(
            "AUR",
            &foreign,
            &installed(&["-Qqem"])?,
            &declared,
        ))
    }

    fn users(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &[
                "eval",
                "--json",
                &format!(
                    "{}#systemConfigs.{}.config.users.users",
                    target.flake, target.host
                ),
                "--apply",
                "builtins.attrNames",
            ],
        )?;
        let declared: BTreeSet<String> = serde_json::from_str(&raw)?;
        let passwd = std::fs::read_to_string("/etc/passwd")?;
        let undeclared: Vec<String> = passwd
            .lines()
            .filter_map(|l| {
                let cols: Vec<&str> = l.split(':').collect();
                let uid: u32 = cols.get(2)?.parse().ok()?;
                let shell = cols.get(6)?;
                let login = !(shell.ends_with("nologin") || shell.ends_with("false"));
                (uid >= 1000 && login && !declared.contains(cols[0]))
                    .then(|| format!("{} (uid {uid})", cols[0]))
            })
            .collect();
        Ok(if undeclared.is_empty() {
            Check::pass("users", "every login user is declared")
        } else {
            Check::warn(
                "users",
                format!("{} login users not declared", undeclared.len()),
                undeclared,
            )
        })
    }

    fn groups() -> Result<Check> {
        let out = std::process::Command::new("sudo")
            .args(["-n", "grpck", "-r"])
            .output()?;
        let text = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if text.contains("password is required") {
            return Ok(Check::warn(
                "groups",
                "not measured, grpck needs sudo without a password prompt",
                Vec::new(),
            ));
        }
        Ok(if out.status.success() {
            Check::pass("groups", "grpck clean")
        } else {
            Check::fail(
                "groups",
                "grpck found problems",
                text.lines().map(str::to_string).collect(),
            )
        })
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::collections::BTreeSet;
    use std::path::Path;

    use anyhow::Result;

    use super::{Check, Target, generation, output, package_check};

    pub fn checks(target: &Target) -> Vec<Check> {
        let attr = format!(
            "{}#darwinConfigurations.{}.system",
            target.flake, target.host
        );
        vec![
            Check::from_result(
                "nix-darwin",
                generation(
                    "nix-darwin",
                    &attr,
                    Path::new("/run/current-system"),
                    "run wsctl apply",
                ),
            ),
            Check::from_result("brews", brews(target)),
            Check::from_result("casks", casks(target)),
            Check::from_result("remote login", remote_login(target)),
        ]
    }

    fn option(target: &Target, path: &str) -> String {
        format!(
            "{}#darwinConfigurations.{}.config.{path}",
            target.flake, target.host
        )
    }

    fn brews(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &[
                "eval",
                "--json",
                &option(target, "homebrew.brews"),
                "--apply",
                "map (b: b.name)",
            ],
        )?;
        let declared: BTreeSet<String> = serde_json::from_str(&raw)?;
        let installed: BTreeSet<String> = output("brew", &["leaves", "--installed-on-request"])?
            .lines()
            .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
            .collect();
        let present: BTreeSet<String> = output("brew", &["list", "--formula", "-1"])?
            .lines()
            .map(str::to_string)
            .collect();
        Ok(package_check("brews", &present, &installed, &declared))
    }

    fn casks(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &[
                "eval",
                "--json",
                &option(target, "homebrew.casks"),
                "--apply",
                "map (c: c.name)",
            ],
        )?;
        let declared: BTreeSet<String> = serde_json::from_str(&raw)?;
        let installed: BTreeSet<String> = output("brew", &["list", "--cask", "-1"])?
            .lines()
            .map(str::to_string)
            .collect();
        Ok(package_check("casks", &installed, &installed, &declared))
    }

    fn remote_login(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &["eval", "--json", &option(target, "services.openssh.enable")],
        )?;
        let declared: Option<bool> = serde_json::from_str(&raw)?;
        let running = std::process::Command::new("launchctl")
            .args(["print", "system/com.openssh.sshd"])
            .output()?
            .status
            .success();
        let state = if running { "on" } else { "off" };
        Ok(match declared {
            None => Check::warn(
                "remote login",
                format!("{state}, not declared (services.openssh.enable)"),
                Vec::new(),
            ),
            Some(want) if want == running => {
                Check::pass("remote login", format!("{state} as declared"))
            }
            Some(_) => Check::fail(
                "remote login",
                format!("{state}, declared {}", if running { "off" } else { "on" }),
                Vec::new(),
            ),
        })
    }
}
