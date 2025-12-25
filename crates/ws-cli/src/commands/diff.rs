//! Diff command implementation

use console::style;
use ws_core::{Change, Context, Executor};
use ws_dsl::Workstation;

/// Run the diff command
pub fn run(workstation: &Workstation, profile: &str) -> anyhow::Result<()> {
    // Build the resource graph for the profile
    let graph = workstation.build_graph(profile)?;

    if graph.is_empty() {
        println!(
            "{} No resources defined for profile '{}'",
            style("!").yellow(),
            profile
        );
        return Ok(());
    }

    // Create context (diff is always dry-run)
    let ctx = Context::new(profile).with_dry_run(true);

    // Plan execution
    let executor = Executor::new();
    let plan = executor.plan(&graph, &ctx)?;

    if plan.is_empty() {
        println!(
            "{} No changes needed for profile '{}'",
            style("✓").green(),
            profile
        );
        return Ok(());
    }

    // Show diff
    println!(
        "\n{} Changes for profile '{}':\n",
        style("→").cyan(),
        style(profile).bold()
    );

    for resource in &plan.resources {
        let (symbol, color) = match &resource.change {
            Change::Create => ("+", "green"),
            Change::Update(_) => ("~", "yellow"),
            Change::Remove => ("-", "red"),
            Change::NoOp => ("=", "dim"),
        };

        let styled_symbol = match color {
            "green" => style(symbol).green(),
            "yellow" => style(symbol).yellow(),
            "red" => style(symbol).red(),
            _ => style(symbol).dim(),
        };

        println!(
            "  {} {} {}",
            styled_symbol,
            style(&resource.id.kind).cyan(),
            &resource.id.name
        );

        // Show update details if any
        if let Change::Update(details) = &resource.change {
            for detail in details {
                println!(
                    "      {} {} → {}",
                    style(&detail.field).dim(),
                    style(&detail.from).red(),
                    style(&detail.to).green()
                );
            }
        }
    }

    println!();
    println!(
        "  {} to create, {} to update, {} to remove",
        style(plan.creates()).green(),
        style(plan.updates()).yellow(),
        style(plan.removes()).red()
    );
    println!();
    println!(
        "{} Run 'ws apply --profile {}' to apply these changes",
        style("i").blue(),
        profile
    );

    Ok(())
}
