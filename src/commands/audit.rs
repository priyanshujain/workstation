use console::style;
use disk::overview::disk_overview;
use disk::report::{self, Source};
use disk::util::format_size;

pub fn run_report(no_cache: bool) -> anyhow::Result<()> {
    println!();
    println!(
        "  {}",
        style("Workstation Disk Audit").bold().underlined().cyan()
    );
    println!();

    if let Some(overview) = disk_overview() {
        let pct = overview.usage_percent();
        let pct_style = if pct > 90.0 {
            style(format!("{pct:.0}%")).red().bold()
        } else if pct > 70.0 {
            style(format!("{pct:.0}%")).yellow().bold()
        } else {
            style(format!("{pct:.0}%")).green().bold()
        };

        let bar_width = 30;
        let filled = ((pct / 100.0) * bar_width as f64) as usize;
        let empty = bar_width - filled;
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));

        println!("  {}  {}", style("Disk").bold(), pct_style);
        println!(
            "  {}  {} used / {} total / {} free",
            style(bar).dim(),
            style(format_size(overview.used)).white(),
            style(format_size(overview.total)).dim(),
            style(format_size(overview.free)).green(),
        );
        println!();
    }

    if no_cache {
        println!("  Scanning...");
    }

    let (report, source) = report::load_or_refresh(
        &disk::projects::default_roots(),
        disk::projects::MAX_DEPTH,
        no_cache,
    );
    let categories = report.to_categories();

    if no_cache {
        // Clear "Scanning..." line
        print!("\x1b[1A\x1b[2K");
    }

    let as_of = match source {
        Source::Fresh => "fresh scan".to_string(),
        Source::Cache { age } => report::humanize_age(age),
    };
    println!(
        "  {}  {}",
        style("Usage by Category").bold(),
        style(format!("({as_of})")).dim()
    );
    println!("  {}", style("─".repeat(50)).dim());
    println!();

    let mut total_accounted = 0u64;

    for cat in &categories {
        total_accounted += cat.total_size;

        println!(
            "  {:>10}  {}",
            style(format_size(cat.total_size)).yellow().bold(),
            style(&cat.name).white().bold(),
        );

        for p in &cat.paths {
            println!(
                "  {:>10}    {}",
                style(format_size(p.size)).dim(),
                style(&p.label).dim(),
            );
        }
    }

    println!();
    println!("  {}", style("─".repeat(50)).dim());
    println!(
        "  {:>10}  {}",
        style(format_size(total_accounted)).green().bold(),
        style("total tracked").bold(),
    );

    let stale = report.stale_paths();
    if !stale.is_empty() {
        println!(
            "  {}",
            style(format!(
                "{} path(s) changed since the scan, run with --no-cache for exact numbers",
                stale.len()
            ))
            .dim()
        );
    }
    println!();

    Ok(())
}
