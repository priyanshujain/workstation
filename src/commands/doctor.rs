use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use console::style;

use super::apply::{Target, output};

#[cfg(target_os = "linux")]
mod dconf;
#[cfg(target_os = "macos")]
mod defaults;
mod merges;

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
    let (attr, active) = if cfg!(target_os = "macos") {
        (
            format!(
                "{}#darwinConfigurations.{}.config.home-manager.users.{}.home.activationPackage",
                target.flake,
                target.host,
                user()
            ),
            home().join(".local/state/home-manager/gcroots/current-home"),
        )
    } else {
        (
            format!(
                "{}#homeConfigurations.\"{}@{}\".activationPackage",
                target.flake,
                user(),
                target.host
            ),
            home().join(".local/state/nix/profiles/home-manager"),
        )
    };
    Check::from_result(
        "home-manager",
        generation("home-manager", &attr, &active, "run wsctl apply"),
    )
}

fn built_from_local_checkout(binary: &Path) -> bool {
    let Ok(info) = output("go", &["version", "-m", &binary.display().to_string()]) else {
        return false;
    };
    info.lines()
        .find_map(|line| line.trim().strip_prefix("mod\t"))
        .and_then(|rest| rest.split('\t').next())
        .is_some_and(|module| home().join("go/src").join(module).is_dir())
}

fn stray_binaries() -> Check {
    let mut dirs = vec![
        home().join(".local/bin"),
        home().join("go/bin"),
        home().join(".cargo/bin"),
        home().join(".bun/bin"),
    ];
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let nix = dir.starts_with(home().join(".nix-profile"))
            || dir.starts_with(home().join(".local/state/nix"))
            || dir.starts_with("/nix")
            || dir.starts_with("/run");
        if dir.starts_with(home()) && !nix && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Ok(prefix) = output("npm", &["prefix", "-g"]) {
        let bin = PathBuf::from(prefix.trim()).join("bin");
        if !bin.starts_with("/nix/store") && !dirs.contains(&bin) {
            dirs.push(bin);
        }
    }
    let agent_dirs = agent_install_dirs(&dirs);
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if resolved(&path).is_some_and(|p| {
                p.starts_with("/nix/store")
                    || agent_dirs.iter().any(|dir| Path::new(&p).starts_with(dir))
            }) {
                continue;
            }
            let executable = std::fs::metadata(&path).is_ok_and(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && m.permissions().mode() & 0o111 != 0
            });
            if !executable || built_from_local_checkout(&path) {
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

fn agent_install_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let config = agents::config::load().unwrap_or_default();
    config
        .agents
        .iter()
        .flat_map(|agent| dirs.iter().map(move |dir| dir.join(&agent.command)))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| !path.starts_with("/nix/store"))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect()
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
    checks.push(Check::from_result("app settings", merges::merges()));
    checks.push(Check::from_result("declared state", merges::checks()));
    checks.push(stray_binaries());
    checks.push(agent_daemons());
    checks
}

fn agent_daemons() -> Check {
    let stale = super::stale_daemons::find(&agents::process::Table::snapshot());
    if stale.is_empty() {
        return Check::pass("agent daemons", "none older than the applied config");
    }
    Check::warn(
        "agent daemons",
        "started before the last apply, restart them",
        stale
            .iter()
            .map(|daemon| format!("{} {}", daemon.name, daemon.pid))
            .collect(),
    )
}

#[cfg(target_os = "linux")]
mod os {
    use std::collections::BTreeSet;
    use std::path::Path;

    use anyhow::Result;

    use super::{Check, Target, eval_names, generation, output, package_check, resolved};
    use crate::commands::apply::linux::{managed_files, masked_units, preset_units};

    const PROFILE: &str = "/nix/var/nix/profiles/system-manager-profiles/system-manager";

    /// userborn writes these from the declared `users` and `groups`.
    const USERBORN_FILES: &[&str] = &["/etc/passwd", "/etc/group", "/etc/shadow", "/etc/gshadow"];

    /// What a fresh Arch install generates by itself, with no choice made: identities, keys,
    /// caches built from installed packages, and the backups shadow's tools keep.
    const ARCH_GENERATED: &[&str] = &[
        "/etc/.pwd.lock",
        "/etc/.updated",
        "/etc/adjtime",
        "/etc/ld.so.cache",
        "/etc/machine-id",
        "/etc/os-release",
        "/etc/passwd-",
        "/etc/group-",
        "/etc/shadow-",
        "/etc/gshadow-",
        "/etc/subuid-",
        "/etc/subgid-",
        "/etc/ssh/ssh_host_*",
        "/etc/pacman.d/gnupg/*",
        "/etc/ca-certificates/extracted/*",
        "/etc/ssl/certs/*",
        "/etc/fonts/conf.d/*",
        "/etc/mkinitcpio.d/*.preset",
    ];

    /// What the Nix installer writes before wsctl can run.
    const NIX_INSTALLER: &[&str] = &[
        "/etc/bash.bashrc",
        "/etc/bashrc",
        "/etc/zshrc",
        "/etc/zsh/zshrc",
        "/etc/profile.d/nix.sh",
        "/etc/nix/nix.conf",
        "/etc/nix/nix.custom.conf",
        "/etc/tmpfiles.d/nix-daemon.conf",
        "/etc/systemd/system/nix-daemon.service",
        "/etc/systemd/system/nix-daemon.socket",
        "/etc/systemd/system/determinate-nixd.socket",
        "/usr/local/bin/determinate-nixd",
    ];

    fn matches(patterns: &[&str], path: &str) -> bool {
        patterns.iter().any(|p| match p.split_once('*') {
            Some((head, tail)) => path.starts_with(head) && path.ends_with(tail),
            None => *p == path,
        })
    }

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
            Check::from_result("unowned /etc", unowned(&managed)),
            Check::from_result("failed units", failed_units()),
            Check::from_result("system units", system_units(&managed)),
            Check::from_result("user units", user_units()),
            Check::from_result("pacman", pacman(target)),
            Check::from_result("AUR", aur(target)),
            Check::from_result("users", users(target)),
            Check::from_result("groups", groups()),
            Check::from_result("settings", super::dconf::check(target)),
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
        let static_env = static_env();
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
            } else {
                let expected = file
                    .strip_prefix("/etc/")
                    .and_then(|rel| resolved(&static_env.join(rel)));
                let live = resolved(path);
                let ok = match expected {
                    Some(expected) => live.as_deref() == Some(expected.as_str()),
                    None => live.is_some_and(|p| p.starts_with("/nix/store")),
                };
                if !ok {
                    drift.push(format!("replaced {file}"));
                }
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

    /// The tree the active generation links /etc entries from.
    fn static_env() -> std::path::PathBuf {
        let raw = std::fs::read_to_string(format!("{PROFILE}/etcFiles/etcFiles.json"))
            .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        std::path::PathBuf::from(json["staticEnv"].as_str().unwrap_or_default())
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
                !USERBORN_FILES.contains(&f.as_str())
                    && !matches(NIX_INSTALLER, f)
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

    fn unowned(managed: &BTreeSet<String>) -> Result<Check> {
        let owned: BTreeSet<String> = output("pacman", &["-Qlq"])?
            .lines()
            .filter(|l| l.starts_with("/etc/") || l.starts_with("/usr/local/"))
            .map(|l| l.trim_end_matches('/').to_string())
            .collect();
        let mut found = Vec::new();
        let mut unreadable = Vec::new();
        let mut stack = vec![
            std::path::PathBuf::from("/etc"),
            std::path::PathBuf::from("/usr/local"),
        ];
        while let Some(dir) = stack.pop() {
            let mut files = Vec::new();
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let Ok(kind) = entry.file_type() else {
                            continue;
                        };
                        if kind.is_dir() {
                            stack.push(entry.path());
                        } else {
                            files.push((entry.path(), kind.is_symlink()));
                        }
                    }
                }
                Err(_) => {
                    let dir_name = dir.display().to_string();
                    let listed = std::process::Command::new("sudo")
                        .args(["-n", "find", &dir_name, "-mindepth", "1"])
                        .args(["(", "-type", "f", "-o", "-type", "l", ")"])
                        .output();
                    match listed {
                        Ok(out) if out.status.success() => {
                            for line in String::from_utf8_lossy(&out.stdout).lines() {
                                files.push((std::path::PathBuf::from(line), false));
                            }
                        }
                        _ => unreadable.push(format!("not readable {dir_name}")),
                    }
                }
            }
            for (path, is_symlink) in files {
                let name = path.display().to_string();
                let unit_link = is_symlink
                    && (name.starts_with("/etc/systemd/system/")
                        || name.starts_with("/etc/systemd/user/"))
                    && std::fs::read_link(&path).is_ok_and(|t| {
                        let t = t.display().to_string();
                        t.starts_with("/usr/lib/systemd/")
                            || t == "/dev/null"
                            || t.starts_with("/etc/systemd/system/")
                    });
                let backup = name
                    .strip_suffix(".system-manager-backup")
                    .is_some_and(|b| managed.contains(b));
                if owned.contains(&name)
                    || managed.contains(&name)
                    || backup
                    || unit_link
                    || USERBORN_FILES.contains(&name.as_str())
                    || matches(ARCH_GENERATED, &name)
                    || matches(NIX_INSTALLER, &name)
                {
                    continue;
                }
                found.push(name);
            }
        }
        found.sort();
        found.extend(unreadable);
        Ok(if found.is_empty() {
            Check::pass(
                "unowned /etc",
                "every file is packaged, declared or generated",
            )
        } else {
            Check::warn(
                "unowned /etc",
                format!(
                    "{} files no package or declaration accounts for",
                    found.len()
                ),
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
        let declared = preset_units();
        let also: BTreeSet<String> = declared
            .iter()
            .flat_map(|unit| {
                output("systemctl", &["cat", unit])
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| l.trim().strip_prefix("Also="))
                    .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut not_enabled = Vec::new();
        for unit in declared.iter() {
            let state = output("systemctl", &["is-enabled", unit]).unwrap_or_default();
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
                if *preset == "enabled"
                    || managed_units.contains(unit)
                    || also.contains(*unit)
                    || preset_enabled(unit)
                {
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
        let snapshot: Option<String> = serde_json::from_str(&output(
            "nix",
            &[
                "eval",
                "--json",
                &format!(
                    "{}#systemConfigs.{}.config.pacman.snapshot",
                    target.flake, target.host
                ),
            ],
        )?)?;
        let mut check = package_check(
            "pacman",
            &installed(&["-Qq"])?,
            &installed(&["-Qqen"])?,
            &declared,
        );
        let drifted: Vec<String> = output("pacman", &["-Sl"])?
            .lines()
            .filter_map(|l| {
                let (head, installed) = l.split_once(" [installed: ")?;
                let mut cols = head.split_whitespace();
                let (_, name, version) = (cols.next()?, cols.next()?, cols.next()?);
                Some(format!(
                    "{name} {} installed, {version} pinned",
                    installed.trim_end_matches(']')
                ))
            })
            .collect();
        if !drifted.is_empty() {
            let mut details = drifted;
            details.extend(check.details);
            return Ok(Check::fail(
                "pacman",
                format!(
                    "{} packages differ from the pinned repositories, run wsctl apply",
                    details.len()
                ),
                details,
            ));
        }
        if snapshot.is_none() && check.level == super::Level::Pass {
            check = Check::warn(
                "pacman",
                "not pinned: pacman.snapshot is unset, so the live mirrors decide versions",
                Vec::new(),
            );
        } else if snapshot.is_none() {
            check
                .details
                .push("not pinned: pacman.snapshot is unset".to_string());
        }
        Ok(check)
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
        let check = package_check("AUR", &foreign, &installed(&["-Qqem"])?, &declared);
        let sources: std::collections::BTreeMap<String, String> = serde_json::from_str(&output(
            "nix",
            &[
                "eval",
                "--json",
                &format!(
                    "{}#systemConfigs.{}.config.aur.packages",
                    target.flake, target.host
                ),
            ],
        )?)?;
        let mut drifted = Vec::new();
        for (name, src) in &sources {
            let pinned = crate::commands::apply::linux::srcinfo_version(Path::new(src))?;
            let live = output("pacman", &["-Q", name])
                .ok()
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string));
            if let Some(live) = live.filter(|v| *v != pinned) {
                drifted.push(format!("{name} {live} installed, {pinned} pinned"));
            }
        }
        if drifted.is_empty() {
            return Ok(check);
        }
        drifted.extend(check.details);
        Ok(Check::fail(
            "AUR",
            "installed versions differ from the pinned sources, run wsctl apply",
            drifted,
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
            Check::from_result("settings", super::defaults::check(target)),
            Check::from_result("power", power(target)),
            Check::from_result("firewall", firewall(target)),
            Check::from_result("default apps", default_apps()),
        ]
    }

    fn default_apps() -> Result<Check> {
        use crate::commands::default_apps::{describe, report};
        let Some(report) = report()? else {
            return Ok(Check::pass("default apps", "none declared"));
        };
        let unmapped = report.unmapped.iter().cloned();
        if report.mismatches.is_empty() {
            return Ok(Check {
                details: unmapped.collect(),
                ..Check::pass(
                    "default apps",
                    format!("{} types open in their declared app", report.managed),
                )
            });
        }
        Ok(Check::fail(
            "default apps",
            "types open in the wrong app, run wsctl default-apps",
            report
                .mismatches
                .iter()
                .map(describe)
                .chain(unmapped)
                .collect(),
        ))
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

    fn power(target: &Target) -> Result<Check> {
        let script = output(
            "nix",
            &[
                "eval",
                "--raw",
                &option(target, "system.activationScripts.postActivation.text"),
            ],
        )?;
        let live = output("pmset", &["-g", "custom"])?;
        let mut sources: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
        for line in live.lines() {
            match line.strip_suffix(':') {
                Some(source) => sources.push((source, Vec::new())),
                None => {
                    if let (Some((_, pairs)), Some((key, value))) =
                        (sources.last_mut(), line.trim().rsplit_once(' '))
                    {
                        pairs.push((key.trim(), value.trim()));
                    }
                }
            }
        }

        let mut total = 0;
        let mut drift = Vec::new();
        for words in super::defaults::statements(&script) {
            let Some(start) = words.iter().position(|w| w == "pmset") else {
                continue;
            };
            let mut words = words[start + 1..].iter().map(String::as_str);
            let wanted_sources: &[&str] = match words.next() {
                Some("-b") => &["Battery Power"],
                Some("-c") => &["AC Power"],
                Some("-a") => &["Battery Power", "AC Power"],
                other => anyhow::bail!("unsupported pmset call {other:?}"),
            };
            let words: Vec<&str> = words.collect();
            for pair in words.chunks(2) {
                let [key, want] = pair else {
                    anyhow::bail!("pmset {} has no value", pair[0]);
                };
                for source in wanted_sources {
                    total += 1;
                    let have = sources
                        .iter()
                        .find(|(name, _)| name == source)
                        .and_then(|(_, pairs)| pairs.iter().find(|(k, _)| k == key))
                        .map(|(_, v)| *v);
                    if have != Some(*want) {
                        drift.push(format!(
                            "{source} {key}: want {want}, have {}",
                            have.unwrap_or("unset")
                        ));
                    }
                }
            }
        }
        Ok(if drift.is_empty() {
            Check::pass("power", format!("{total} declared pmset values match"))
        } else {
            Check::fail(
                "power",
                format!("{} of {total} declared pmset values differ", drift.len()),
                drift,
            )
        })
    }

    fn firewall(target: &Target) -> Result<Check> {
        let raw = output(
            "nix",
            &[
                "eval",
                "--json",
                &option(target, "networking.applicationFirewall"),
                "--apply",
                "f: { inherit (f) enable enableStealthMode; }",
            ],
        )?;
        let declared: serde_json::Value = serde_json::from_str(&raw)?;
        let live = output(
            "/usr/libexec/ApplicationFirewall/socketfilterfw",
            &["--getglobalstate", "--getstealthmode"],
        )?;
        let enabled = live.contains("Firewall is enabled");
        let stealth = live.contains("stealth mode is on");
        let mut drift = Vec::new();
        for (name, key, have) in [
            ("firewall", "enable", enabled),
            ("stealth mode", "enableStealthMode", stealth),
        ] {
            if let Some(want) = declared[key].as_bool()
                && want != have
            {
                drift.push(format!("{name}: want {}, have {}", on(want), on(have)));
            }
        }
        Ok(if drift.is_empty() {
            Check::pass("firewall", "matches networking.applicationFirewall")
        } else {
            Check::fail(
                "firewall",
                "differs from networking.applicationFirewall",
                drift,
            )
        })
    }

    fn on(value: bool) -> &'static str {
        if value { "on" } else { "off" }
    }
}
