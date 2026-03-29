use crate::builder::Workstation;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use wsctl_core::{Change, Context, Executor};

pub fn run(
    workstation: &Workstation,
    profile: &str,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    let graph = workstation.build_graph(profile)?;

    if graph.is_empty() {
        println!(
            "{} No resources defined for profile '{}'",
            style("!").yellow(),
            profile
        );
        return Ok(());
    }

    let ctx = Context::new(profile).with_dry_run(dry_run);
    let executor = Executor::new();
    let plan = executor.plan(&graph, &ctx)?;

    if plan.is_empty() {
        println!(
            "{} Everything is up to date for profile '{}'",
            style("✓").green(),
            profile
        );
        return Ok(());
    }

    println!(
        "\n{} Execution plan for profile '{}':\n",
        style("→").cyan(),
        style(profile).bold()
    );

    for resource in &plan.resources {
        let symbol = match &resource.change {
            Change::Create => style("+").green(),
            Change::Update(_) => style("~").yellow(),
            Change::Remove => style("-").red(),
            Change::NoOp => style("=").dim(),
        };
        println!(
            "  {} {} {}",
            symbol,
            style(&resource.id.kind).cyan(),
            &resource.id.name
        );
    }

    println!();
    println!(
        "  {} to create, {} to update, {} to remove",
        style(plan.creates()).green(),
        style(plan.updates()).yellow(),
        style(plan.removes()).red()
    );
    println!();

    if dry_run {
        println!("{} Dry-run mode, no changes made.", style("i").blue());
        return Ok(());
    }

    if !yes {
        print!("Apply these changes? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{} Aborted.", style("×").red());
            return Ok(());
        }
    }

    println!();
    let pb = ProgressBar::new(plan.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("█▓░"),
    );

    let mut success = 0;
    let mut failed = 0;

    for planned in &plan.resources {
        pb.set_message(planned.id.name.clone());

        let result = planned.resource.apply(&planned.change, &ctx);

        match result {
            Ok(()) => {
                success += 1;
                pb.println(format!(
                    "  {} {} {}",
                    style("✓").green(),
                    style(&planned.id.kind).cyan(),
                    &planned.id.name
                ));
            }
            Err(e) => {
                failed += 1;
                pb.println(format!(
                    "  {} {} {} - {}",
                    style("✗").red(),
                    style(&planned.id.kind).cyan(),
                    &planned.id.name,
                    style(e).red()
                ));
            }
        }

        pb.inc(1);
    }

    pb.finish_and_clear();

    println!();
    if failed == 0 {
        println!(
            "{} Successfully applied {} resources",
            style("✓").green(),
            success
        );
    } else {
        println!(
            "{} Applied {} resources, {} failed",
            style("!").yellow(),
            success,
            style(failed).red()
        );
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
