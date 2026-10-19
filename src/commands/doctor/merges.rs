use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Check, home};

#[derive(Deserialize)]
struct Merge {
    script: String,
    files: Vec<String>,
}

fn config(name: &str) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("wsctl")
        .join(name)
}

fn load<T: for<'de> Deserialize<'de>>(name: &str) -> Result<BTreeMap<String, T>> {
    let path = config(name);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).with_context(|| format!("could not parse {}", path.display()))
}

pub fn merges() -> Result<Check> {
    let merges: BTreeMap<String, Merge> = load("merges.json")?;
    let mut drift = Vec::new();
    for (name, merge) in &merges {
        for file in rerun(name, merge)? {
            drift.push(format!("{name}: apply would change ~/{file}"));
        }
    }
    Ok(if drift.is_empty() {
        Check::pass(
            "app settings",
            format!("{} merged settings match", merges.len()),
        )
    } else {
        Check::fail(
            "app settings",
            "merged settings drifted, run wsctl apply",
            drift,
        )
    })
}

fn rerun(name: &str, merge: &Merge) -> Result<Vec<String>> {
    let home = home();
    let scratch = std::env::temp_dir().join(format!("wsctl-merge-{name}-{}", std::process::id()));
    let result = (|| {
        let live = expand(&home, &merge.files)?;
        for file in &live {
            let copy = scratch.join(file);
            std::fs::create_dir_all(copy.parent().unwrap_or(&scratch))?;
            std::fs::copy(home.join(file), &copy)?;
        }
        std::fs::create_dir_all(&scratch)?;
        let status = Command::new(&merge.script)
            .env("HOME", &scratch)
            .status()
            .with_context(|| format!("could not run {}", merge.script))?;
        if !status.success() {
            bail!(
                "{name}: {} failed on a copy of the live files",
                merge.script
            );
        }
        let mut changed = Vec::new();
        for file in expand(&scratch, &merge.files)? {
            let after = std::fs::read(scratch.join(&file))?;
            let before = std::fs::read(home.join(&file)).ok();
            if !before.is_some_and(|before| same_content(&before, &after)) {
                changed.push(file);
            }
        }
        Ok(changed)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn same_content(before: &[u8], after: &[u8]) -> bool {
    if before == after {
        return true;
    }
    match (
        serde_json::from_slice::<serde_json::Value>(before),
        serde_json::from_slice::<serde_json::Value>(after),
    ) {
        (Ok(before), Ok(after)) => before == after,
        _ => false,
    }
}

fn expand(root: &Path, patterns: &[String]) -> Result<Vec<String>> {
    let out = Command::new("bash")
        .args([
            "-c",
            r#"cd "$1" && shift && IFS= && shopt -s nullglob && for p in "$@"; do for f in $p; do [ -f "$f" ] && printf '%s\0' "$f"; done; done; true"#,
            "expand",
        ])
        .arg(root)
        .args(patterns)
        .output()?;
    if !out.status.success() {
        bail!("could not expand {}", patterns.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn checks() -> Result<Check> {
    let checks: BTreeMap<String, String> = load("checks.json")?;
    let mut failed = Vec::new();
    for (name, command) in &checks {
        let ok = Command::new("bash")
            .args(["-c", command])
            .output()
            .with_context(|| format!("could not run the {name} check"))?
            .status
            .success();
        if !ok {
            failed.push(format!("{name}: {command}"));
        }
    }
    Ok(if failed.is_empty() {
        Check::pass("declared state", format!("{} checks pass", checks.len()))
    } else {
        Check::fail(
            "declared state",
            format!("{} of {} checks fail", failed.len(), checks.len()),
            failed,
        )
    })
}
