use std::path::Path;

use console::style;
use disk::audit::Audit;
use disk::overview::{DiskOverview, disk_overview};
use disk::report::{self, Report, Source};
use disk::sweep::Denial;
use disk::util::format_size;

const RULE: usize = 58;

/// Roots smaller than this are macOS bookkeeping directories that only add
/// rows. They still count towards the totals, they just do not get a line.
const ROOT_FLOOR: u64 = 16 * 1024 * 1024;

const NAME_WIDTH: usize = 18;

pub fn run_report(no_cache: bool) -> anyhow::Result<()> {
    println!();
    println!(
        "  {}",
        style("Workstation Disk Audit").bold().underlined().cyan()
    );
    println!();

    let overview = disk_overview();
    if let Some(overview) = &overview {
        print_overview(overview);
    }

    if no_cache {
        println!(
            "  {}",
            style("Walking the volume, this takes a few minutes...").dim()
        );
    }

    let (report, source) = report::load_or_refresh(
        &disk::projects::default_roots(),
        disk::projects::MAX_DEPTH,
        no_cache,
    );

    if no_cache {
        print!("\x1b[1A\x1b[2K");
    }

    let audit = report.to_audit();
    let as_of = match source {
        Source::Fresh => "fresh scan".to_string(),
        Source::Cache { age } => report::humanize_age(age),
    };

    print_roots(&audit, overview.as_ref(), &as_of);
    print_categories(&audit);
    print_projects(&report);
    print_staleness(&report);

    Ok(())
}

fn print_overview(overview: &DiskOverview) {
    let pct = overview.usage_percent();
    let pct_style = if pct > 90.0 {
        style(format!("{pct:.0}%")).red().bold()
    } else if pct > 70.0 {
        style(format!("{pct:.0}%")).yellow().bold()
    } else {
        style(format!("{pct:.0}%")).green().bold()
    };

    let width = 30;
    let filled = ((pct / 100.0) * width as f64) as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));

    println!("  {}  {}", style("Disk").bold(), pct_style);
    println!(
        "  {}  {} used / {} total / {} free",
        style(bar).dim(),
        style(format_size(overview.used())).white(),
        style(format_size(overview.total)).dim(),
        style(format_size(overview.free)).green(),
    );
    println!();
}

/// The partition. Every byte on the disk is in exactly one of these rows,
/// which is the property the old "total tracked" line never had.
fn print_roots(audit: &Audit, overview: Option<&DiskOverview>, as_of: &str) {
    println!(
        "  {}  {}",
        style("Where the space is").bold(),
        style(format!("({as_of})")).dim()
    );
    println!("  {}", style("─".repeat(RULE)).dim());

    // A root that could not be opened still gets a row: "unknown" and "empty"
    // are different answers and the old audit only ever gave the second one.
    for root in audit
        .roots
        .iter()
        .filter(|r| r.total >= ROOT_FLOOR || r.unreadable_count > 0)
    {
        let size = if root.total == 0 && root.unreadable_count > 0 {
            style("unknown".to_string()).red().to_string()
        } else {
            style(format_size(root.total)).yellow().bold().to_string()
        };
        println!(
            "  {:>10}  {:<NAME_WIDTH$}  {}",
            size,
            style(clip(&root.name, NAME_WIDTH)).white().bold(),
            style(tilde(&root.path)).dim(),
        );
    }

    if let Some(overview) = overview
        && overview.other_volumes > 0
    {
        println!(
            "  {:>10}  {:<NAME_WIDTH$}  {}",
            style(format_size(overview.other_volumes)).yellow(),
            style("Other volumes").white(),
            style("System, Preboot, Recovery, VM").dim(),
        );
    }

    println!("  {}", style("─".repeat(RULE)).dim());

    let accounted = audit.measured() + overview.map_or(0, |o| o.other_volumes);
    println!(
        "  {:>10}  {}",
        style(format_size(accounted)).green().bold(),
        style("accounted for").bold(),
    );

    if let Some(overview) = overview {
        let shortfall = overview.used().saturating_sub(accounted);
        if shortfall > overview.used() / 100 {
            println!(
                "  {:>10}  {}",
                style(format_size(shortfall)).red(),
                style("not reachable by this scan").dim(),
            );
        }
    }

    let unreadable = audit.unreadable_count();
    if unreadable > 0 {
        println!();
        println!(
            "  {}",
            style(format!(
                "{unreadable} director{} could not be opened, so what they hold is \
                 unknown rather than zero.",
                if unreadable == 1 { "y" } else { "ies" }
            ))
            .dim()
        );
        for entry in audit.unreadable_examples(Denial::Protected, 4) {
            println!("  {}", style(format!("  {}", tilde(&entry.path))).dim());
        }
        println!(
            "  {}",
            style("Give Terminal Full Disk Access in System Settings to measure them.").dim()
        );
    }
    println!();
}

fn print_categories(audit: &Audit) {
    println!("  {}", style("What it is").bold());
    println!("  {}", style("─".repeat(RULE)).dim());

    for cat in &audit.categories {
        println!(
            "  {:>10}  {}",
            style(format_size(cat.total_size)).yellow().bold(),
            style(&cat.name).white().bold(),
        );
        for path in &cat.paths {
            println!(
                "  {:>10}    {}",
                style(format_size(path.size)).dim(),
                style(&path.label).dim(),
            );
        }
    }

    println!("  {}", style("─".repeat(RULE)).dim());
    println!(
        "  {:>10}  {}",
        style(format_size(audit.attributed())).green().bold(),
        style("named").bold(),
    );

    let unattributed = audit.unattributed();
    if unattributed == 0 {
        println!();
        return;
    }

    println!(
        "  {:>10}  {}",
        style(format_size(unattributed)).cyan().bold(),
        style("unnamed, largest first").bold(),
    );
    for (path, size) in audit.largest_unattributed(8) {
        println!(
            "  {:>10}    {}",
            style(format_size(size)).dim(),
            style(tilde(path)).dim(),
        );
    }
    println!();
}

/// Build artifacts live inside the roots above, so this is a view of bytes
/// already counted, never an addition to them.
fn print_projects(report: &Report) {
    let projects = report.to_projects();
    let total: u64 = projects.iter().map(|p| p.artifact_size()).sum();
    if total == 0 {
        return;
    }
    println!(
        "  {}  {} across {} projects, already counted above",
        style("Build artifacts").bold(),
        style(format_size(total)).yellow().bold(),
        projects.len(),
    );
    println!("  {}", style("wsctl disk projects lists them").dim());
    println!();
}

fn print_staleness(report: &Report) {
    let stale = report.stale_paths();
    if stale.is_empty() {
        return;
    }
    println!(
        "  {}",
        style(format!(
            "{} path(s) changed since the scan, run with --no-cache for exact numbers",
            stale.len()
        ))
        .dim()
    );
    println!();
}

/// Keep the column. A root name wider than its column pushes every path on
/// that row out of line, and the paths are the part worth reading.
fn clip(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_string();
    }
    let kept: String = name.chars().take(width - 1).collect();
    format!("{kept}\u{2026}")
}

fn tilde(path: &Path) -> String {
    let display = path.to_string_lossy();
    match dirs::home_dir() {
        Some(home) => match display.strip_prefix(home.to_string_lossy().as_ref()) {
            Some("") => "~".to_string(),
            Some(rest) => format!("~{rest}"),
            None => display.into_owned(),
        },
        None => display.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_the_column_width() {
        assert_eq!(clip("Home", 18), "Home");
        assert_eq!(clip(".PreviousSystemInformation", 18).chars().count(), 18);
        assert!(clip(".PreviousSystemInformation", 18).ends_with('\u{2026}'));
    }

    #[test]
    fn tilde_shortens_paths_inside_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(&home.join("Downloads")), "~/Downloads");
        assert_eq!(tilde(Path::new("/Applications")), "/Applications");
    }
}
