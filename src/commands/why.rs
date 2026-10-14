use anyhow::{Result, bail};
use console::style;
use serde::Deserialize;

use super::apply::{Target, output};

#[derive(Deserialize)]
struct Definition {
    file: String,
    names: Vec<String>,
}

const NAMES: &str = r#"defs: let
  unwrap = v: if builtins.isAttrs v && v ? _type && v ? content then unwrap v.content else v;
  name = p: if builtins.isString p then p else (p.pname or p.name or "");
in map (d: {
  file = d.file;
  names = let v = unwrap d.value; in
    if builtins.isList v then map name v
    else if builtins.isAttrs v then builtins.attrNames v
    else [ ];
}) defs"#;

pub fn run(target: &Target, query: &str) -> Result<()> {
    if query.contains('.') {
        return option(target, query);
    }
    package(target, query)
}

fn configs(target: &Target) -> Vec<(&'static str, String)> {
    let home = format!(
        "homeConfigurations.\"{}@{}\"",
        std::env::var("USER").unwrap_or_default(),
        target.host
    );
    if cfg!(target_os = "macos") {
        vec![
            (
                "nix-darwin",
                format!("darwinConfigurations.{}", target.host),
            ),
            ("home-manager", home),
        ]
    } else {
        vec![
            ("system-manager", format!("systemConfigs.{}", target.host)),
            ("home-manager", home),
        ]
    }
}

fn package_lists() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            "environment.systemPackages",
            "homebrew.brews",
            "homebrew.casks",
            "homebrew.masApps",
            "home.packages",
        ]
    } else {
        &[
            "pacman.packages",
            "aur.packages",
            "environment.systemPackages",
            "home.packages",
        ]
    }
}

/// A definition file from the flake's store copy, shown at its path in the local checkout.
pub fn local(file: &str, target: &Target) -> String {
    let rest = file
        .strip_prefix("/nix/store/")
        .and_then(|f| f.split_once('/'))
        .filter(|(store, _)| store.ends_with("-source"))
        .map(|(_, rest)| rest);
    match rest {
        Some(rest) if std::path::Path::new(&target.flake).join(rest).exists() => {
            format!("{}/{rest}", target.flake.trim_end_matches('/'))
        }
        _ => file.to_string(),
    }
}

/// Splits an option path on the dots that are not inside quotes.
fn segments(path: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut quoted = false;
    for c in path.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                parts.last_mut().unwrap().push(c);
            }
            '.' if !quoted => parts.push(String::new()),
            _ => parts.last_mut().unwrap().push(c),
        }
    }
    parts
}

fn option(target: &Target, path: &str) -> Result<()> {
    let parts = segments(path);
    for (scope, config) in configs(target) {
        for len in (1..=parts.len()).rev() {
            let declared = parts[..len].join(".");
            let attr = format!(
                "{}#{config}.options.{declared}.definitionsWithLocations",
                target.flake
            );
            let Ok(raw) = output(
                "nix",
                &["eval", "--json", &attr, "--apply", "map (d: d.file)"],
            ) else {
                continue;
            };
            let files: Vec<String> = serde_json::from_str(&raw)?;
            let value = output(
                "nix",
                &[
                    "eval",
                    "--json",
                    &format!("{}#{config}.config.{path}", target.flake),
                ],
            )
            .unwrap_or_else(|e| format!("{e:#}"));
            println!("{} {} {}", style(path).bold(), style("in").dim(), scope);
            if declared != path {
                println!(
                    "    {}",
                    style(format!("part of the option {declared}")).dim()
                );
            }
            println!("    {}", truncate(value.trim()));
            if files.is_empty() {
                println!("    {}", style("not set, the module default applies").dim());
            }
            for file in files {
                println!("    {}", style(local(&file, target)).dim());
            }
            return Ok(());
        }
    }
    bail!("{path} is not an option of this host's configuration")
}

fn package(target: &Target, name: &str) -> Result<()> {
    let mut found = false;
    for (scope, config) in configs(target) {
        for list in package_lists() {
            let attr = format!(
                "{}#{config}.options.{list}.definitionsWithLocations",
                target.flake
            );
            let Ok(raw) = output("nix", &["eval", "--json", &attr, "--apply", NAMES]) else {
                continue;
            };
            let defs: Vec<Definition> = serde_json::from_str(&raw)?;
            for def in defs {
                if def.names.iter().any(|n| n == name) {
                    found = true;
                    println!(
                        "{} {} {list} {} {scope}",
                        style(name).bold(),
                        style("in").dim(),
                        style("of").dim()
                    );
                    println!("    {}", style(local(&def.file, target)).dim());
                }
            }
        }
    }
    if !found {
        bail!("{name} is not declared in any package list of this host");
    }
    Ok(())
}

fn truncate(value: &str) -> String {
    const MAX: usize = 400;
    if value.chars().count() <= MAX {
        return value.to_string();
    }
    format!("{}...", value.chars().take(MAX).collect::<String>())
}
