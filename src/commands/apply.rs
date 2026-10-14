use std::process::Command;

use anyhow::{Context, Result, bail};
use console::style;

pub struct Target {
    pub flake: String,
    pub host: String,
}

impl Target {
    pub fn parse(raw: &str) -> Result<Self> {
        let (flake, host) = match raw.split_once('#') {
            Some((flake, host)) => (flake.to_string(), host.to_string()),
            None => (raw.to_string(), hostname()?),
        };
        let flake = match flake.strip_prefix("~/") {
            Some(rest) => dirs::home_dir()
                .context("no home directory")?
                .join(rest)
                .display()
                .to_string(),
            None => flake,
        };
        Ok(Self { flake, host })
    }

    pub fn attr(&self) -> String {
        format!("{}#{}", self.flake, self.host)
    }
}

fn hostname() -> Result<String> {
    let out = Command::new("hostname")
        .arg("-s")
        .output()
        .context("could not run hostname")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn step(what: &str) {
    println!("{} {}", style("→").cyan(), style(what).bold());
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed with {status}", args.join(" "));
    }
    Ok(())
}

pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_os = "macos")]
pub fn run_apply(target: &Target, dry_run: bool) -> Result<()> {
    let attr = target.attr();
    if dry_run {
        step(&format!("darwin-rebuild build --flake {attr}"));
        return run("darwin-rebuild", &["build", "--flake", &attr]);
    }
    step(&format!("darwin-rebuild switch --flake {attr}"));
    let darwin_rebuild = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("darwin-rebuild"))
        .find(|p| p.is_file())
        .context("darwin-rebuild not found on PATH")?;
    run(
        "sudo",
        &[
            &darwin_rebuild.display().to_string(),
            "switch",
            "--flake",
            &attr,
        ],
    )
}

#[cfg(target_os = "linux")]
pub fn run_apply(target: &Target, dry_run: bool) -> Result<()> {
    linux::run_apply(target, dry_run)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use anyhow::{Context, Result};
    use console::style;
    use serde::Deserialize;

    use super::{Target, output, run, step};

    #[derive(Deserialize)]
    struct OnActivation {
        cleanup: String,
    }

    #[derive(Deserialize)]
    struct Pacman {
        packages: Vec<String>,
        snapshot: Option<String>,
        #[serde(rename = "onActivation")]
        on_activation: OnActivation,
    }

    #[derive(Deserialize)]
    struct Aur {
        packages: BTreeMap<String, String>,
        #[serde(rename = "onActivation")]
        on_activation: OnActivation,
    }

    pub fn run_apply(target: &Target, dry_run: bool) -> Result<()> {
        let system = target.attr();
        if dry_run {
            step(&format!("system-manager build --flake {system}"));
            run("system-manager", &["build", "--flake", &system])?;
        } else {
            step(&format!("system-manager switch --flake {system}"));
            run("system-manager", &["switch", "--flake", &system, "--sudo"])?;
        }

        let pacman: Pacman = eval(target, "pacman")?;
        let aur: Aur = eval(target, "aur")?;

        step("pacman");
        apply_pacman(&pacman, dry_run)?;

        step("AUR");
        apply_aur(&aur, dry_run)?;

        cleanup(
            "pacman",
            &list(&["-Qqen"])?,
            &pacman.packages.iter().cloned().collect(),
            &pacman.on_activation.cleanup,
            dry_run,
        )?;
        cleanup(
            "AUR",
            &list(&["-Qqem"])?,
            &aur.packages.keys().cloned().collect(),
            &aur.on_activation.cleanup,
            dry_run,
        )?;

        if !dry_run {
            step("grpconv");
            run("sudo", &["grpconv"])?;
        }

        let home = if dry_run { "build" } else { "switch" };
        step(&format!("home-manager {home} --flake {}", target.flake));
        run("home-manager", &[home, "--flake", &target.flake])
    }

    fn eval<T: serde::de::DeserializeOwned>(target: &Target, option: &str) -> Result<T> {
        let attr = format!(
            "{}#systemConfigs.{}.config.{option}",
            target.flake, target.host
        );
        let raw = output("nix", &["eval", "--json", &attr])?;
        serde_json::from_str(&raw).with_context(|| format!("could not read {attr}"))
    }

    fn list(args: &[&str]) -> Result<BTreeSet<String>> {
        Ok(output("pacman", args)?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn apply_pacman(pacman: &Pacman, dry_run: bool) -> Result<()> {
        let installed = list(&["-Qq"])?;
        let missing: Vec<&str> = pacman
            .packages
            .iter()
            .filter(|p| !installed.contains(*p))
            .map(String::as_str)
            .collect();

        if let Some(date) = &pacman.snapshot {
            println!("    {}", style(format!("snapshot {date}")).dim());
            if dry_run {
                return Ok(());
            }
            let conf = snapshot_conf(date)?;
            let conf_path = std::env::temp_dir().join("wsctl-pacman.conf");
            std::fs::write(&conf_path, conf)?;
            let conf_path = conf_path.display().to_string();
            let mut args = vec!["pacman", "--config", &conf_path, "-Syuu", "--needed"];
            args.extend(pacman.packages.iter().map(String::as_str));
            run("sudo", &args)?;
        } else if !missing.is_empty() {
            println!(
                "    {}",
                style(format!("install {}", missing.join(" "))).dim()
            );
            if !dry_run {
                let mut args = vec!["pacman", "-S", "--needed"];
                args.extend(&missing);
                run("sudo", &args)?;
            }
        }

        let deps = list(&["-Qqd"])?;
        let implicit: Vec<&str> = pacman
            .packages
            .iter()
            .filter(|p| deps.contains(*p))
            .map(String::as_str)
            .collect();
        if !implicit.is_empty() {
            println!(
                "    {}",
                style(format!("mark explicit {}", implicit.join(" "))).dim()
            );
            if !dry_run {
                let mut args = vec!["pacman", "-D", "--asexplicit"];
                args.extend(&implicit);
                run("sudo", &args)?;
            }
        }
        Ok(())
    }

    fn snapshot_conf(date: &str) -> Result<String> {
        let (y, rest) = date.split_once('-').context("snapshot is not YYYY-MM-DD")?;
        let (m, d) = rest.split_once('-').context("snapshot is not YYYY-MM-DD")?;
        let server =
            format!("Server = https://archive.archlinux.org/repos/{y}/{m}/{d}/$repo/os/$arch");
        let conf = std::fs::read_to_string("/etc/pacman.conf")?;
        Ok(conf
            .lines()
            .map(|l| {
                if l.trim() == "Include = /etc/pacman.d/mirrorlist" {
                    server.clone()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn apply_aur(aur: &Aur, dry_run: bool) -> Result<()> {
        for (name, src) in &aur.packages {
            let wanted = srcinfo_version(Path::new(src))?;
            let installed = output("pacman", &["-Q", name])
                .ok()
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string));
            if installed.as_deref() == Some(wanted.as_str()) {
                continue;
            }
            println!(
                "    {}",
                style(format!(
                    "build {name} {wanted} (installed {})",
                    installed.as_deref().unwrap_or("none")
                ))
                .dim()
            );
            if dry_run {
                continue;
            }
            let dir = std::env::temp_dir().join(format!("wsctl-aur-{name}"));
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            run("cp", &["-r", src, &dir.display().to_string()])?;
            run("chmod", &["-R", "u+w", &dir.display().to_string()])?;
            let status = std::process::Command::new("makepkg")
                .args(["-si", "--noconfirm", "--needed"])
                .current_dir(&dir)
                .status()
                .context("could not run makepkg")?;
            std::fs::remove_dir_all(&dir)?;
            if !status.success() {
                anyhow::bail!("makepkg for {name} failed with {status}");
            }
        }
        Ok(())
    }

    fn srcinfo_version(src: &Path) -> Result<String> {
        let path = src.join(".SRCINFO");
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let field = |key: &str| {
            raw.lines()
                .filter_map(|l| l.trim().split_once(" = "))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        };
        let ver = field("pkgver").context("no pkgver in .SRCINFO")?;
        let rel = field("pkgrel").context("no pkgrel in .SRCINFO")?;
        Ok(match field("epoch") {
            Some(epoch) => format!("{epoch}:{ver}-{rel}"),
            None => format!("{ver}-{rel}"),
        })
    }

    fn cleanup(
        backend: &str,
        installed: &BTreeSet<String>,
        declared: &BTreeSet<String>,
        mode: &str,
        dry_run: bool,
    ) -> Result<()> {
        let undeclared: Vec<&str> = installed.difference(declared).map(String::as_str).collect();
        if undeclared.is_empty() {
            return Ok(());
        }
        let flag = match mode {
            "uninstall" => "-Rs",
            "zap" => "-Rns",
            _ => {
                println!(
                    "{} {} {backend} packages installed but not declared:",
                    style("!").yellow(),
                    undeclared.len()
                );
                for p in &undeclared {
                    println!("    {}", style(p).yellow());
                }
                return Ok(());
            }
        };
        step(&format!("remove undeclared {backend} packages"));
        println!("    {}", style(undeclared.join(" ")).dim());
        if dry_run {
            return Ok(());
        }
        let mut args = vec!["pacman", flag];
        args.extend(&undeclared);
        run("sudo", &args)
    }
}
