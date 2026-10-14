use std::path::Path;

use anyhow::{Context, Result};
use console::style;
use serde_json::Value;

use super::apply::{Target, output};

struct Input {
    name: String,
    url: String,
    reference: String,
    locked: String,
}

pub fn run(target: &Target) -> Result<()> {
    inputs(target)?;
    os::packages(target)
}

fn lock(target: &Target) -> Result<Value> {
    let path = Path::new(&target.flake).join("flake.lock");
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {}; outdated needs a local flake",
            path.display()
        )
    })?;
    Ok(serde_json::from_str(&raw)?)
}

fn direct_inputs(lock: &Value) -> Vec<Input> {
    let nodes = &lock["nodes"];
    let root = lock["root"].as_str().unwrap_or("root");
    let Some(inputs) = nodes[root]["inputs"].as_object() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (name, node) in inputs {
        let Some(node) = node.as_str() else {
            continue;
        };
        let original = &nodes[node]["original"];
        let locked = &nodes[node]["locked"];
        if original["rev"].is_string() {
            continue;
        }
        let url = match original["type"].as_str() {
            Some("github") => format!(
                "https://github.com/{}/{}",
                original["owner"].as_str().unwrap_or_default(),
                original["repo"].as_str().unwrap_or_default()
            ),
            Some("git") => original["url"].as_str().unwrap_or_default().to_string(),
            _ => continue,
        };
        found.push(Input {
            name: name.clone(),
            url,
            reference: original["ref"].as_str().unwrap_or("HEAD").to_string(),
            locked: locked["rev"].as_str().unwrap_or_default().to_string(),
        });
    }
    found
}

fn head(input: &Input) -> Result<String> {
    let peeled_ref = format!("{}^{{}}", input.reference);
    let raw = output(
        "git",
        &["ls-remote", &input.url, &input.reference, &peeled_ref],
    )?;
    let lines: Vec<(&str, &str)> = raw
        .lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .map(|(sha, r)| (sha, r.trim()))
        .collect();
    let peeled = lines.iter().find(|(_, r)| r.ends_with("^{}"));
    peeled
        .or_else(|| lines.first())
        .map(|(sha, _)| sha.to_string())
        .context("no such ref upstream")
}

fn inputs(target: &Target) -> Result<()> {
    let inputs = direct_inputs(&lock(target)?);
    let heads: Vec<(Input, Result<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = inputs
            .into_iter()
            .map(|input| {
                s.spawn(move || {
                    let head = head(&input);
                    (input, head)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    println!("{}", style("Flake inputs").bold());
    let mut behind = 0;
    for (input, head) in &heads {
        match head {
            Ok(head) if *head == input.locked => {
                println!(
                    "{} {:<20} {}",
                    style("✓").green(),
                    input.name,
                    style(&input.reference).dim()
                );
            }
            Ok(head) => {
                behind += 1;
                println!(
                    "{} {:<20} {} {} → {}",
                    style("!").yellow(),
                    input.name,
                    style(&input.reference).dim(),
                    &input.locked[..input.locked.len().min(7)],
                    &head[..head.len().min(7)]
                );
            }
            Err(e) => println!(
                "{} {:<20} {}",
                style("×").red(),
                input.name,
                style(format!("{e:#}")).red()
            ),
        }
    }
    println!(
        "    {}",
        style(format!("{behind} behind upstream; wsctl update moves them")).dim()
    );
    println!();
    Ok(())
}

#[cfg(target_os = "linux")]
mod os {
    use anyhow::Result;
    use console::style;

    use super::{Target, output};

    pub fn packages(target: &Target) -> Result<()> {
        println!("{}", style("pacman").bold());
        let attr = format!(
            "{}#systemConfigs.{}.config.pacman.snapshot",
            target.flake, target.host
        );
        let snapshot: Option<String> =
            serde_json::from_str(&output("nix", &["eval", "--json", &attr])?)?;
        if let Some(date) = snapshot {
            let today = output("date", &["+%F"])?.trim().to_string();
            println!(
                "{} pinned to the Arch Linux Archive of {date}, today is {today}",
                style("!").yellow()
            );
            println!(
                "    {}",
                style("wsctl update --snapshot moves it to today").dim()
            );
            return Ok(());
        }
        let out = std::process::Command::new("checkupdates").output()?;
        let raw = String::from_utf8_lossy(&out.stdout);
        let updates: Vec<&str> = raw.lines().collect();
        if updates.is_empty() {
            println!("{} up to date with the mirrors", style("✓").green());
        } else {
            println!(
                "{} {} updates on the mirrors, not pinned (pacman.snapshot is unset)",
                style("!").yellow(),
                updates.len()
            );
            for line in updates {
                println!("    {}", style(line).yellow());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod os {
    use anyhow::Result;
    use console::style;

    use super::{Target, output};

    pub fn packages(_target: &Target) -> Result<()> {
        println!("{}", style("Homebrew").bold());
        let raw = output("brew", &["outdated", "--verbose"])?;
        if raw.trim().is_empty() {
            println!("{} nothing outdated", style("✓").green());
        } else {
            for line in raw.lines() {
                println!("{} {}", style("!").yellow(), line);
            }
        }
        Ok(())
    }
}
