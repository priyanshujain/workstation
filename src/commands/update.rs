use anyhow::{Result, bail};
use console::style;

use super::apply::{Target, output};

pub fn run(target: &Target, inputs: &[String], snapshot: bool) -> Result<()> {
    if snapshot {
        move_snapshot(target)?;
    } else {
        let mut args = vec!["flake", "update", "--flake", target.flake.as_str()];
        args.extend(inputs.iter().map(String::as_str));
        println!("{} nix {}", style("→").cyan(), args.join(" "));
        let status = std::process::Command::new("nix").args(&args).status()?;
        if !status.success() {
            bail!("nix flake update failed with {status}");
        }
    }
    println!(
        "    {}",
        style("review the diff, then run wsctl apply").dim()
    );
    Ok(())
}

fn move_snapshot(target: &Target) -> Result<()> {
    let attr = format!(
        "{}#systemConfigs.{}.options.pacman.snapshot.definitionsWithLocations",
        target.flake, target.host
    );
    let files: Vec<String> = serde_json::from_str(&output(
        "nix",
        &["eval", "--json", &attr, "--apply", "map (d: d.file)"],
    )?)?;
    let local: Vec<String> = files
        .iter()
        .map(|f| super::why::local(f, target))
        .filter(|f| !f.starts_with("/nix/store/"))
        .collect();
    let [path] = local.as_slice() else {
        bail!(
            "pacman.snapshot is not set in {}; declare it in the host config first",
            target.flake
        );
    };
    let today = output("date", &["+%F"])?.trim().to_string();
    let raw = std::fs::read_to_string(path)?;
    let mut changed = 0;
    let updated: Vec<String> = raw
        .lines()
        .map(|line| {
            let Some(start) = line.find("pacman.snapshot") else {
                return line.to_string();
            };
            let Some(open) = line[start..].find('"').map(|i| start + i + 1) else {
                return line.to_string();
            };
            let Some(close) = line[open..].find('"').map(|i| open + i) else {
                return line.to_string();
            };
            changed += 1;
            format!("{}{today}{}", &line[..open], &line[close..])
        })
        .collect();
    if changed != 1 {
        bail!("expected one `pacman.snapshot = \"...\"` line in {path}, found {changed}");
    }
    let mut out = updated.join("\n");
    if raw.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out)?;
    println!(
        "{} pacman.snapshot = \"{today}\" in {path}",
        style("✓").green()
    );
    Ok(())
}
