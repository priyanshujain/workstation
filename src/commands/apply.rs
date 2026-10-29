use std::process::Command;

use anyhow::{Context, Result, bail};
use console::style;

use super::pins::{self, Pins};
use super::stale_daemons;

pub struct Target {
    pub flake: String,
    pub host: String,
}

impl Target {
    pub fn parse(raw: &str) -> Result<Self> {
        let (flake, host) = match raw.split_once('#') {
            Some((flake, host)) => (flake, Some(host.to_string())),
            None => (raw, None),
        };
        let flake = match flake.strip_prefix("~/") {
            Some(rest) => dirs::home_dir()
                .context("no home directory")?
                .join(rest)
                .display()
                .to_string(),
            None => flake.to_string(),
        };
        let flake = match std::fs::canonicalize(&flake) {
            Ok(path) if !flake.contains(':') => path.display().to_string(),
            _ => flake,
        };
        let host = match host {
            Some(host) => host,
            None => default_host(&flake)?,
        };
        Ok(Self { flake, host })
    }

    pub fn attr(&self) -> String {
        format!("{}#{}", self.flake, self.host)
    }
}

fn default_host(flake: &str) -> Result<String> {
    let hostname = hostname()?;
    let configurations = if cfg!(target_os = "macos") {
        "darwinConfigurations"
    } else {
        "systemConfigs"
    };
    let attr = format!("{flake}#{configurations}");
    let Ok(raw) = output(
        "nix",
        &["eval", "--json", &attr, "--apply", "builtins.attrNames"],
    ) else {
        return Ok(hostname);
    };
    let hosts: Vec<String> = serde_json::from_str(&raw)?;
    if hosts.contains(&hostname) {
        return Ok(hostname);
    }
    match hosts.as_slice() {
        [only] => Ok(only.clone()),
        _ => bail!(
            "{flake} has no {configurations}.{hostname}; pass --flake {flake}#<host>, one of: {}",
            hosts.join(", ")
        ),
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

/// Runs from a scratch directory, because the switch and build tools leave a `result` link in
/// whatever directory they start in.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let scratch = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("wsctl");
    std::fs::create_dir_all(&scratch)?;
    let status = Command::new(program)
        .args(args)
        .current_dir(&scratch)
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

pub fn sync(target: &Target, pins: Option<Pins>, named: &[String], dry_run: bool) -> Result<()> {
    if dry_run {
        return run_apply(target, true);
    }
    pins::pull(target)?;
    let Some(pins) = pins else {
        run_apply(target, false)?;
        stale_daemons::restart_unused();
        return Ok(());
    };
    let inputs = pins::select(target, pins, named)?;
    pins::update(target, &inputs)?;
    run_apply(target, false)?;
    stale_daemons::restart_unused();
    pins::commit_lock(target, &inputs)
}

#[cfg(target_os = "macos")]
pub fn run_apply(target: &Target, dry_run: bool) -> Result<()> {
    let attr = target.attr();
    if dry_run {
        step(&format!("darwin-rebuild build --flake {attr}"));
        return run("darwin-rebuild", &["build", "--flake", &attr]);
    }
    step(&format!("darwin-rebuild switch --flake {attr}"));
    with_sudo_kept_alive(|| {
        let system = build_darwin_system(target)?;
        run(
            "sudo",
            &[
                "-H",
                "nix-env",
                "-p",
                "/nix/var/nix/profiles/system",
                "--set",
                &system,
            ],
        )?;
        run("sudo", &["-H", &format!("{system}/activate")])
    })
}

/// Builds as the user, so private inputs fetch with the user's SSH key; root only activates.
#[cfg(target_os = "macos")]
fn build_darwin_system(target: &Target) -> Result<String> {
    let attr = format!(
        "{}#darwinConfigurations.{}.system",
        target.flake, target.host
    );
    let out = Command::new("nix")
        .args(["build", "--no-link", "--print-out-paths", &attr])
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("could not run nix")?;
    if !out.status.success() {
        bail!("nix build {attr} failed with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(target_os = "macos")]
fn with_sudo_kept_alive(work: impl FnOnce() -> Result<()>) -> Result<()> {
    use std::process::Stdio;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::Duration;

    run("sudo", &["-v"])?;
    let (stop, stopped) = channel::<()>();
    let refresher = std::thread::spawn(move || {
        while stopped.recv_timeout(Duration::from_secs(60)) == Err(RecvTimeoutError::Timeout) {
            let _ = Command::new("sudo")
                .args(["-n", "-v"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    });
    let result = work();
    drop(stop);
    let _ = refresher.join();
    result
}

#[cfg(target_os = "linux")]
pub fn run_apply(target: &Target, dry_run: bool) -> Result<()> {
    linux::run_apply(target, dry_run)
}

#[cfg(target_os = "linux")]
pub mod linux {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use anyhow::{Context, Result, bail};
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
            if managed_files()?.is_empty() {
                bail!(
                    "system-manager switch wrote no /etc files; its activation failed, see the output above"
                );
            }
        }

        let presets = preset_units();
        if !presets.is_empty() {
            step("systemctl preset");
            println!("    {}", style(presets.join(" ")).dim());
            if !dry_run {
                let mut args = vec!["systemctl", "preset"];
                args.extend(presets.iter().map(String::as_str));
                run("sudo", &args)?;
            }
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

    const STATE: &str = "/var/lib/system-manager/state/system-manager-state.json";

    #[derive(Deserialize)]
    struct State {
        #[serde(rename = "fileTree")]
        file_tree: FileTree,
        services: BTreeMap<String, Service>,
    }

    #[derive(Deserialize)]
    struct FileTree {
        files: Vec<String>,
        #[serde(rename = "backedUpFiles", default)]
        backed_up_files: Vec<String>,
    }

    #[derive(Deserialize)]
    struct Service {
        masked: bool,
    }

    fn state() -> Result<State> {
        Ok(serde_json::from_str(&std::fs::read_to_string(STATE)?)?)
    }

    /// Every /etc path system-manager owns, including the ones it replaced.
    pub fn managed_files() -> Result<BTreeSet<String>> {
        let tree = state()?.file_tree;
        Ok(tree.files.into_iter().chain(tree.backed_up_files).collect())
    }

    pub fn masked_units() -> Result<BTreeSet<String>> {
        Ok(state()?
            .services
            .into_iter()
            .filter(|(_, s)| s.masked)
            .map(|(name, _)| name)
            .collect())
    }

    /// Units enabled by the preset files system-manager manages in /etc/systemd/system-preset.
    pub fn preset_units() -> Vec<String> {
        let mut units = Vec::new();
        for file in managed_files().unwrap_or_default() {
            if !(file.starts_with("/etc/systemd/system-preset/") && file.ends_with(".preset")) {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in raw.lines() {
                let mut words = line.split_whitespace();
                if let (Some("enable"), Some(unit)) = (words.next(), words.next()) {
                    units.push(unit.to_string());
                }
            }
        }
        units.sort();
        units.dedup();
        units
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
            println!(
                "    {}",
                style(format!("sync to the Arch Linux Archive of {date}")).dim()
            );
            if dry_run {
                return Ok(());
            }
            let mut args = vec!["pacman", "-Syyuu", "--needed", "--noconfirm"];
            args.extend(pacman.packages.iter().map(String::as_str));
            run("sudo", &args)?;
        } else if !missing.is_empty() {
            println!(
                "    {}",
                style(format!("install {}", missing.join(" "))).dim()
            );
            if !dry_run {
                let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
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

    pub fn srcinfo_version(src: &Path) -> Result<String> {
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
        let mut args = vec!["pacman", flag, "--noconfirm"];
        args.extend(&undeclared);
        run("sudo", &args)
    }
}
