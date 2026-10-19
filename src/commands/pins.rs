use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use console::style;

use super::apply::{Target, output};

pub enum Pins {
    Tracked,
    Upstream,
}

pub fn pull(target: &Target) -> Result<()> {
    if !is_checkout(target) {
        return Ok(());
    }
    git(target, &["pull", "--ff-only", "--quiet"])
}

pub fn select(target: &Target, pins: Pins, named: &[String]) -> Result<Vec<String>> {
    if !named.is_empty() {
        return Ok(named.to_vec());
    }
    let tracked = tracked(target)?;
    Ok(match pins {
        Pins::Tracked => tracked.into_iter().collect(),
        Pins::Upstream => all_inputs(target)?
            .into_iter()
            .filter(|input| !tracked.contains(input))
            .collect(),
    })
}

pub fn update(target: &Target, inputs: &[String]) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let mut args = vec!["flake", "update", "--flake", target.flake.as_str()];
    args.extend(inputs.iter().map(String::as_str));
    println!("{} nix {}", style("→").cyan(), args.join(" "));
    let mut nix = Command::new("nix");
    if let Some(token) = github_token() {
        let inherited = std::env::var("NIX_CONFIG").unwrap_or_default();
        nix.env(
            "NIX_CONFIG",
            format!("{inherited}\naccess-tokens = github.com={token}"),
        );
    }
    let status = nix.args(&args).status()?;
    if !status.success() {
        bail!("nix flake update failed with {status}");
    }
    Ok(())
}

fn github_token() -> Option<String> {
    let token = output("gh", &["auth", "token"]).ok()?;
    Some(token.trim().to_string()).filter(|token| !token.is_empty())
}

pub fn commit_lock(target: &Target, inputs: &[String]) -> Result<()> {
    if !is_checkout(target) || lock_unchanged(target)? {
        return Ok(());
    }
    let message = format!("chore(nix): bump {}", inputs.join(" "));
    git(target, &["commit", "--quiet", "-m", &message, "flake.lock"])?;
    git(target, &["push", "--quiet"])?;
    println!("{} {message}, pushed", style("✓").green());
    Ok(())
}

fn tracked(target: &Target) -> Result<BTreeSet<String>> {
    let user = std::env::var("USER").unwrap_or_default();
    let home = if cfg!(target_os = "macos") {
        format!(
            "darwinConfigurations.{}.config.home-manager.users.{user}",
            target.host
        )
    } else {
        format!("homeConfigurations.\"{user}@{}\".config", target.host)
    };
    let attr = format!("{}#{home}.services.wsctl.trackInputs", target.flake);
    Ok(serde_json::from_str(&output(
        "nix",
        &["eval", "--json", &attr],
    )?)?)
}

fn all_inputs(target: &Target) -> Result<BTreeSet<String>> {
    let metadata: serde_json::Value = serde_json::from_str(&output(
        "nix",
        &["flake", "metadata", "--json", &target.flake],
    )?)?;
    let inputs = metadata["locks"]["nodes"]["root"]["inputs"]
        .as_object()
        .context("flake.lock has no root inputs")?;
    Ok(inputs.keys().cloned().collect())
}

fn is_checkout(target: &Target) -> bool {
    Path::new(&target.flake).join(".git").exists()
}

fn lock_unchanged(target: &Target) -> Result<bool> {
    let status = Command::new("git")
        .args(["-C", &target.flake, "diff", "--quiet", "flake.lock"])
        .status()?;
    Ok(status.success())
}

fn git(target: &Target, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(&target.flake)
        .args(args)
        .status()
        .context("could not run git")?;
    if !status.success() {
        bail!("git {} failed in {}", args.join(" "), target.flake);
    }
    Ok(())
}
