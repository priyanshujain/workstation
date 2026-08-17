use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;
use console::style;
use disk::cleanup::{CleanAction, Target};
use disk::liveness::Liveness;
use disk::projects::{MAX_DEPTH, Project, default_roots};
use disk::report::{self, Source};
use disk::util::format_size;

pub struct Options {
    pub roots: Vec<PathBuf>,
    pub idle_days: u64,
    pub min_size_mb: u64,
    pub clean: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub no_cache: bool,
}

pub fn run(opts: Options) -> Result<()> {
    let roots = if opts.roots.is_empty() {
        default_roots()
    } else {
        opts.roots.clone()
    };

    if roots.is_empty() {
        println!("  No project roots to scan.");
        return Ok(());
    }

    println!();
    println!(
        "  {}",
        style("Project Build Artifacts").bold().underlined().cyan()
    );
    for root in &roots {
        println!("  {}", style(root.display()).dim());
    }
    println!();

    // Custom roots are never served from cache: the cached report only ever
    // covers the default roots.
    let custom_roots = !opts.roots.is_empty();
    let scanning = opts.no_cache || custom_roots;
    if scanning {
        println!("  Scanning...");
    }

    let (projects, source) = if custom_roots {
        (disk::projects::discover(&roots, MAX_DEPTH), Source::Fresh)
    } else {
        let (report, source) = report::load_or_refresh(&roots, MAX_DEPTH, opts.no_cache);
        (report.to_projects(), source)
    };

    // Always measured now, never cached: a stale "nothing is using this" is
    // exactly how live work gets deleted.
    let live = Liveness::snapshot();
    let now = SystemTime::now();

    if scanning {
        print!("\x1b[1A\x1b[2K");
    }

    let as_of = match source {
        Source::Fresh => "fresh scan".to_string(),
        Source::Cache { age } => report::humanize_age(age),
    };

    // Decimal, so --min-size-mb 50 hides exactly what the SIZE column calls
    // less than 50 MB.
    let min_bytes = opts.min_size_mb * 1_000_000;
    let shown: Vec<&Project> = projects
        .iter()
        .filter(|p| p.artifact_size() >= min_bytes)
        .collect();

    if shown.is_empty() {
        println!("  Nothing above {}.", format_size(min_bytes.max(1)));
        println!();
        return Ok(());
    }

    let hidden = projects.len() - shown.len();
    let mut total = 0u64;
    let mut eligible: Vec<(&Project, u64)> = Vec::new();
    let mut blocked: Vec<(&Project, String)> = Vec::new();

    println!(
        "  {}  {}",
        style("sizes").dim(),
        style(format!("({as_of}), usage checked live")).dim()
    );
    println!();
    println!(
        "  {:>10}  {:>6}  {:<7}  {}",
        style("SIZE").bold(),
        style("IDLE").bold(),
        style("KIND").bold(),
        style("PROJECT").bold()
    );
    println!("  {}", style("─".repeat(72)).dim());

    for project in &shown {
        let size = project.artifact_size();
        total += size;

        let busy: Vec<String> = project
            .artifacts
            .iter()
            .flat_map(|a| live.check(&a.path))
            .chain(live.check(&project.root))
            .map(|b| b.to_string())
            .collect();

        let idle = project.idle_days(now);
        let idle_text = match idle {
            Some(d) => format!("{d}d"),
            None => "?".to_string(),
        };

        let idle_styled = match idle {
            Some(d) if d >= opts.idle_days => style(idle_text.clone()).green(),
            _ => style(idle_text.clone()).dim(),
        };

        println!(
            "  {:>10}  {:>6}  {:<7}  {}",
            style(format_size(size)).yellow().bold(),
            idle_styled,
            style(project.kind.label()).cyan(),
            style(project.root.display()).white()
        );

        if let Some(reason) = busy.first().cloned() {
            println!("  {:>10}  {}", "", style(format!("in use: {reason}")).red());
            blocked.push((project, reason));
        } else if project.is_idle_for(opts.idle_days, now) {
            eligible.push((project, size));
        }
    }

    println!("  {}", style("─".repeat(72)).dim());
    println!(
        "  {:>10}  across {} projects{}",
        style(format_size(total)).green().bold(),
        shown.len(),
        if hidden > 0 {
            format!(", {hidden} smaller hidden")
        } else {
            String::new()
        }
    );

    let reclaimable: u64 = eligible.iter().map(|(_, s)| s).sum();
    println!(
        "  {:>10}  idle {}+ days and not in use",
        style(format_size(reclaimable)).green().bold(),
        opts.idle_days
    );
    if !blocked.is_empty() {
        println!(
            "  {:>10}  {} project(s) skipped, something is using them",
            "",
            blocked.len()
        );
    }
    println!();

    if !opts.clean {
        println!(
            "  {}",
            style("Read-only. Pass --clean to remove the eligible artifacts.").dim()
        );
        println!();
        return Ok(());
    }

    if eligible.is_empty() {
        println!("  Nothing eligible to clean.");
        println!();
        return Ok(());
    }

    println!("  {}", style("Would remove:").bold());
    for (project, size) in &eligible {
        for artifact in &project.artifacts {
            println!(
                "    {:>10}  {}",
                style(format_size(artifact.size)).dim(),
                artifact.path.display()
            );
        }
        let _ = size;
        println!(
            "    {:>10}  {}",
            "",
            style(format!("rebuild with: {}", project.kind.rebuild_hint())).dim()
        );
    }
    println!();

    if opts.dry_run {
        println!(
            "  {}",
            style(format!(
                "Dry run, nothing removed ({})",
                format_size(reclaimable)
            ))
            .dim()
        );
        println!();
        return Ok(());
    }

    if !confirm(
        &format!("Remove {} of build artifacts?", format_size(reclaimable)),
        opts.yes,
    )? {
        println!("  Cancelled.");
        println!();
        return Ok(());
    }

    // Re-check liveness immediately before deleting: the scan above may have
    // taken minutes, and a build started since then must still block.
    let live = Liveness::snapshot();
    let mut freed = 0u64;
    let mut skipped = 0usize;

    for (project, _) in &eligible {
        for artifact in &project.artifacts {
            let busy = live.check(&artifact.path);
            if !busy.is_empty() {
                println!(
                    "  {} {} ({})",
                    style("skip").yellow(),
                    artifact.path.display(),
                    busy[0]
                );
                skipped += 1;
                continue;
            }

            let target = Target::new(
                project.kind.label(),
                artifact.path.display().to_string(),
                artifact.size,
                CleanAction::RemoveDir(artifact.path.clone()),
            );
            match target.clean() {
                Ok(bytes) => {
                    freed += bytes;
                    println!("  {} {}", style("removed").green(), artifact.path.display());
                }
                Err(e) => {
                    println!(
                        "  {} {}: {e}",
                        style("failed").red(),
                        artifact.path.display()
                    );
                }
            }
        }
    }

    println!();
    println!("  Freed {}", style(format_size(freed)).green().bold());
    if skipped > 0 {
        println!("  {skipped} skipped, became busy during the run");
    }
    println!();

    Ok(())
}

fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    print!("  {prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}
