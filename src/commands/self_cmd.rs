use anyhow::{Context, Result};
use console::style;
use std::path::{Path, PathBuf};

pub fn uninstall(yes: bool, purge: bool) -> Result<()> {
    let path = std::env::current_exe().context("could not determine current executable path")?;
    let path = path.canonicalize().unwrap_or(path);

    println!(
        "{} This will remove the {} binary:",
        style("→").cyan(),
        style("wsctl").bold()
    );
    println!("    {}", style(path.display()).dim());

    let data: Vec<PathBuf> = purge
        .then(purge_paths)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    if purge {
        println!("  and, with --purge, its logs, caches and config:");
        for p in &data {
            println!("    {}", style(p.display()).dim());
        }
    }
    println!();

    if !confirm("Continue with uninstall?", yes)? {
        println!("{} Aborted.", style("×").red());
        return Ok(());
    }

    if purge {
        remove_paths(&data)?;
    }
    delete_binary(&path)?;

    println!("{} Removed {}", style("✓").green(), path.display());
    println!(
        "{} If you added the install dir to PATH manually, you may want to remove that line too.",
        style("i").blue()
    );
    Ok(())
}

fn delete_binary(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
}

/// The data wsctl leaves behind once the binary is gone: the job logs,
/// the disk report in Application Support, and the display pin in ~/.config.
fn purge_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "macos")]
    {
        paths.push(display::agent::log_path());
        if let Some(dir) = display::config::config_path().parent() {
            paths.push(dir.to_path_buf());
        }
    }
    if let Some(data) = dirs::data_dir() {
        paths.push(data.join("wsctl"));
    }
    paths
}

fn remove_paths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        let result = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
            Ok(_) => std::fs::remove_file(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => Err(e),
        };
        result.with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_binary_removes_file() {
        let path = std::env::temp_dir().join(format!("wsctl-test-{}", std::process::id()));
        std::fs::write(&path, b"placeholder").unwrap();
        assert!(path.exists());

        delete_binary(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn purge_covers_logs_config_and_data() {
        let paths = purge_paths();
        #[cfg(target_os = "macos")]
        {
            let has = |suffix: &str| paths.iter().any(|p| p.ends_with(suffix));
            assert!(has("Library/Logs/wsctl-display.log"));
            assert!(has(".config/wsctl"));
        }
        assert!(paths.contains(&dirs::data_dir().unwrap().join("wsctl")));
    }

    #[test]
    fn remove_paths_takes_files_and_directories_and_skips_what_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("wsctl.log");
        let tree = dir.path().join("wsctl");
        std::fs::write(&file, b"x").unwrap();
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("nested/report.json"), b"{}").unwrap();

        remove_paths(&[file.clone(), tree.clone(), dir.path().join("missing")]).unwrap();

        assert!(!file.exists());
        assert!(!tree.exists());
    }

    #[test]
    fn delete_binary_errors_on_missing_file() {
        let path = std::env::temp_dir().join(format!("wsctl-missing-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);

        assert!(delete_binary(&path).is_err());
    }
}
