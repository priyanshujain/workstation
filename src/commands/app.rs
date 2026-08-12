use anyhow::Result;
use console::style;

use apps::bundle::{self, AppBundle};
use apps::leftovers::{Confidence, Leftover, scan};
use apps::removal::{self, Mode};
use disk::util::format_size;
use wsctl_core::SystemCommandRunner;

pub fn remove(
    name: String,
    dry_run: bool,
    purge: bool,
    include_likely: bool,
    yes: bool,
) -> Result<()> {
    let app = bundle::find(&name)?;
    let mode = if purge { Mode::Purge } else { Mode::Trash };

    println!("{} {}", style("→").cyan(), style(&app.name).bold());
    println!("    {}", style(app.path.display()).dim());
    if let Some(id) = &app.bundle_id {
        println!("    {}", style(id).dim());
    }
    println!();

    let found = scan(&app);
    let (mut selected, likely): (Vec<Leftover>, Vec<Leftover>) = found
        .into_iter()
        .partition(|l| l.confidence == Confidence::Exact || include_likely);

    if selected.is_empty() {
        println!("{} Nothing found to remove.", style("✓").green());
        return Ok(());
    }

    let total: u64 = selected.iter().map(|l| l.size).sum();
    println!(
        "{} {} to remove, {}:",
        style("→").cyan(),
        style(format!("{} items", selected.len())).bold(),
        style(format_size(total)).bold()
    );
    for item in &selected {
        print_item(item);
    }

    if !likely.is_empty() {
        println!();
        println!(
            "{} Left alone, shared with the vendor's other apps. Pass {} to include:",
            style("!").yellow(),
            style("--include-likely").bold()
        );
        for item in &likely {
            print_item(item);
            println!("             {}", style(&item.reason).dim());
        }
    }

    let containers = selected
        .iter()
        .filter(|l| {
            l.target_path()
                .is_some_and(|p| p.to_string_lossy().contains("/Library/Containers/"))
        })
        .count();
    if containers > 0 && !removal::has_full_disk_access() {
        println!();
        let subject = if containers == 1 {
            "One of these is an app container".to_string()
        } else {
            format!("{containers} of these are app containers")
        };
        println!(
            "{} {subject}, which macOS will not let this terminal touch without Full Disk Access.",
            style("!").yellow()
        );
        println!(
            "    Grant it in System Settings > Privacy & Security > Full Disk Access, then run this again."
        );
    }

    if let Some(hint) = keychain_hint(&app) {
        println!();
        println!(
            "{} Keychain entries matched. Review with:",
            style("i").blue()
        );
        println!("    {}", style(hint).dim());
    }

    let cask = installed_cask(&app);
    if let Some(token) = &cask {
        println!();
        println!(
            "{} Homebrew installed this as the cask {}. It will be removed with {} so brew's records stay straight.",
            style("i").blue(),
            style(token).bold(),
            style("brew uninstall --cask --zap").bold()
        );
    }

    let privileged = removal::requires_root(&selected);
    if privileged {
        let count = selected.iter().filter(|l| l.needs_root).count();
        println!();
        println!(
            "{} {} of these need root, and are deleted outright rather than trashed.",
            style("!").yellow(),
            count
        );
    }

    if dry_run {
        println!();
        println!("{} Dry run, nothing was touched.", style("i").blue());
        return Ok(());
    }

    println!();
    let verb = if mode == Mode::Purge {
        "Permanently delete"
    } else {
        "Move to the Trash"
    };
    if !confirm(&format!("{verb} {} items?", selected.len()), yes)? {
        println!("{} Aborted.", style("×").red());
        return Ok(());
    }

    if privileged {
        removal::escalate().map_err(anyhow::Error::msg)?;
    }

    if removal::quit(&app) {
        println!("{} Quit {}", style("✓").green(), app.name);
    }

    if let Some(token) = &cask {
        match packages::brew::ops::uninstall_cask_zap(&SystemCommandRunner::new(), token) {
            Ok(()) => println!("{} brew zapped {token}", style("✓").green()),
            Err(e) => println!("{} brew zap failed: {e}", style("!").yellow()),
        }
        // The zap deletes the bundle; drop it so the report does not double count.
        selected.retain(|l| l.target_path() != Some(app.path.as_path()));
    }

    let report = removal::execute(&selected, mode);

    println!();
    println!(
        "{} Removed {} items, {} reclaimed.",
        style("✓").green(),
        report.removed.len(),
        style(format_size(report.freed)).bold()
    );
    if !report.forced_purge.is_empty() {
        println!(
            "{} {} root-owned items were deleted outright, not trashed.",
            style("i").blue(),
            report.forced_purge.len()
        );
    }
    for (item, err) in &report.failed {
        println!("{} {item}: {err}", style("×").red());
    }
    if report.failed.iter().any(|(_, e)| removal::is_tcc_denial(e)) {
        println!(
            "{} Those failures are macOS privacy protection, not file permissions. Give your terminal Full Disk Access and run this again.",
            style("i").blue()
        );
    }
    Ok(())
}

fn print_item(item: &Leftover) {
    let root = if item.needs_root {
        format!(" {}", style("root").yellow())
    } else {
        String::new()
    };
    println!(
        "    {:>9}  {}{}",
        format_size(item.size),
        item.display(),
        root
    );
}

fn installed_cask(app: &AppBundle) -> Option<String> {
    let out = std::process::Command::new("brew")
        .args(["list", "--cask"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let installed: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    apps::brew::match_cask(&app.name, &installed)
}

fn keychain_hint(app: &AppBundle) -> Option<String> {
    let service = app.name.clone();
    let found = std::process::Command::new("security")
        .args(["find-generic-password", "-s", &service])
        .output()
        .ok()?
        .status
        .success();
    found.then(|| format!("security delete-generic-password -s {service:?}"))
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
