use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use console::style;
use serde::Deserialize;

use super::apply::output;

#[derive(Deserialize)]
struct Config {
    version: String,
    xcodes: String,
    #[serde(rename = "appleId")]
    apple_id: Option<String>,
}

fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("wsctl/xcode.json")
}

fn load() -> Result<Option<Config>> {
    let path = config_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let config = serde_json::from_str(&raw)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(config))
}

enum State {
    Selected,
    Installed,
    Missing,
}

fn state(config: &Config) -> Result<State> {
    let installed = output(&config.xcodes, &["installed"])?;
    let prefix = format!("{} ", config.version);
    Ok(
        match installed.lines().find(|line| line.starts_with(&prefix)) {
            Some(line) if line.contains("(Selected)") => State::Selected,
            Some(_) => State::Installed,
            None => State::Missing,
        },
    )
}

fn rbw(args: &[&str]) -> Result<String> {
    Ok(output("rbw", args)?.trim().to_string())
}

fn install(config: &Config) -> Result<()> {
    let mut command = Command::new(&config.xcodes);
    command.args([
        "install",
        &config.version,
        "--select",
        "--experimental-unxip",
        "--empty-trash",
    ]);
    if let Some(entry) = &config.apple_id {
        command
            .env(
                "XCODES_USERNAME",
                rbw(&["get", "--field", "username", entry])?,
            )
            .env("XCODES_PASSWORD", rbw(&["get", entry])?);
    }
    let status = command.status().context("could not run xcodes")?;
    if !status.success() {
        bail!("xcodes install {} failed with {status}", config.version);
    }
    Ok(())
}

pub fn ensure(dry_run: bool) -> Result<()> {
    let Some(config) = load()? else {
        return Ok(());
    };
    let version = &config.version;
    match state(&config)? {
        State::Selected => Ok(()),
        State::Installed => {
            println!(
                "{} {}",
                style("→").cyan(),
                style(format!("xcodes select {version}")).bold()
            );
            if dry_run {
                return Ok(());
            }
            let status = Command::new("sudo")
                .args([config.xcodes.as_str(), "select", version])
                .status()
                .context("could not run xcodes")?;
            if !status.success() {
                bail!("xcodes select {version} failed with {status}");
            }
            Ok(())
        }
        State::Missing if dry_run || !std::io::stdin().is_terminal() => {
            println!(
                "{} Xcode {version} is not installed; run wsctl apply in a terminal to install it",
                style("!").yellow()
            );
            Ok(())
        }
        State::Missing => {
            println!(
                "{} {}",
                style("→").cyan(),
                style(format!("xcodes install {version}")).bold()
            );
            install(&config)
        }
    }
}
