use crate::builder::Workstation;
use console::style;
use serde::Serialize;
use wsctl_core::{Change, Context, Executor};

#[derive(Serialize)]
struct DiffOutput {
    profile: String,
    changes: Vec<ChangeEntry>,
    summary: Summary,
}

#[derive(Serialize)]
struct ChangeEntry {
    kind: String,
    name: String,
    action: String,
}

#[derive(Serialize)]
struct Summary {
    create: usize,
    update: usize,
    remove: usize,
}

pub fn run(workstation: &Workstation, profile: &str, json: bool) -> anyhow::Result<()> {
    let graph = workstation.build_graph(profile)?;

    if graph.is_empty() {
        if json {
            let output = DiffOutput {
                profile: profile.to_string(),
                changes: vec![],
                summary: Summary {
                    create: 0,
                    update: 0,
                    remove: 0,
                },
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!(
                "{} No resources defined for profile '{}'",
                style("!").yellow(),
                profile
            );
        }
        return Ok(());
    }

    let ctx = Context::new(profile).with_dry_run(true);
    let executor = Executor::new();
    let plan = executor.plan(&graph, &ctx)?;

    if json {
        let changes: Vec<ChangeEntry> = plan
            .resources
            .iter()
            .map(|r| ChangeEntry {
                kind: r.id.kind.clone(),
                name: r.id.name.clone(),
                action: r.change.description().to_string(),
            })
            .collect();

        let output = DiffOutput {
            profile: profile.to_string(),
            summary: Summary {
                create: plan.creates(),
                update: plan.updates(),
                remove: plan.removes(),
            },
            changes,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if plan.is_empty() {
        println!(
            "{} No changes needed for profile '{}'",
            style("✓").green(),
            profile
        );
        return Ok(());
    }

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
        "{} Run 'wsctl apply {}' to apply these changes",
        style("i").blue(),
        profile
    );

    Ok(())
}
