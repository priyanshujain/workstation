use console::style;
use ws_cleanup::scan;

fn main() {
    println!();
    println!(
        "  {}",
        style("Workstation Disk Audit").bold().underlined().cyan()
    );
    println!();

    // Disk overview
    if let Some(overview) = scan::disk_overview() {
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
            style(scan::format_size(overview.used)).white(),
            style(scan::format_size(overview.total)).dim(),
            style(scan::format_size(overview.free)).green(),
        );
        println!();
    }

    // Category breakdown
    println!("  {}", style("Usage by Category").bold());
    println!("  {}", style("─".repeat(50)).dim());

    println!();
    println!("  Scanning...");

    let categories = scan::scan_categories();

    // Clear "Scanning..." line
    print!("\x1b[1A\x1b[2K");

    let mut total_accounted = 0u64;

    for cat in &categories {
        total_accounted += cat.total_size;

        println!(
            "  {:>10}  {}",
            style(scan::format_size(cat.total_size)).yellow().bold(),
            style(&cat.name).white().bold(),
        );

        for p in &cat.paths {
            println!(
                "  {:>10}    {}",
                style(scan::format_size(p.size)).dim(),
                style(&p.label).dim(),
            );
        }
    }

    println!();
    println!("  {}", style("─".repeat(50)).dim());
    println!(
        "  {:>10}  {}",
        style(scan::format_size(total_accounted)).green().bold(),
        style("total tracked").bold(),
    );
    println!();
}
