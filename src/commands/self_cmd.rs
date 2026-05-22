use anyhow::{Context, Result};
use console::style;
use std::path::Path;
use std::process::Command;

const INSTALLER_URL: &str =
    "https://raw.githubusercontent.com/priyanshujain/workstation/main/install.sh";

pub fn uninstall(yes: bool) -> Result<()> {
    let path = std::env::current_exe().context("could not determine current executable path")?;
    let path = path.canonicalize().unwrap_or(path);

    println!(
        "{} This will remove the {} binary at:",
        style("→").cyan(),
        style("wsctl").bold()
    );
    println!("    {}", style(path.display()).dim());
    println!();

    if !confirm("Continue with uninstall?", yes)? {
        println!("{} Aborted.", style("×").red());
        return Ok(());
    }

    delete_binary(&path)?;

    println!("{} Removed {}", style("✓").green(), path.display());
    println!(
        "{} If you added the install dir to PATH manually, you may want to remove that line too.",
        style("i").blue()
    );
    Ok(())
}

pub fn update(yes: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!(
        "{} Current version: {}",
        style("→").cyan(),
        style(current).bold()
    );
    println!(
        "{} Re-running installer from {}",
        style("→").cyan(),
        INSTALLER_URL
    );
    println!();

    if !confirm("Download and install the latest release?", yes)? {
        println!("{} Aborted.", style("×").red());
        return Ok(());
    }

    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("curl -fsSL {INSTALLER_URL} | sh"))
        .status()
        .context("failed to launch installer (is curl installed?)")?;

    if !status.success() {
        anyhow::bail!("installer exited with status {status}");
    }

    println!();
    println!(
        "{} Update complete. Run `wsctl --version` to confirm.",
        style("✓").green()
    );
    Ok(())
}

fn delete_binary(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
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
    fn delete_binary_errors_on_missing_file() {
        let path = std::env::temp_dir().join(format!("wsctl-missing-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);

        assert!(delete_binary(&path).is_err());
    }
}
