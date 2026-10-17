use std::io::IsTerminal;

use anyhow::Result;
use console::style;
use display::{Outcome, agent, arrange, config, names, platform, spaces};

pub fn list() -> Result<()> {
    let displays = platform::list_displays()?;
    let labels = names::lookup();
    let pinned = config::load()?.preferred_main;

    println!();
    println!(
        "  {}",
        style("Connected Displays").bold().underlined().cyan()
    );
    println!();

    if displays.is_empty() {
        println!("  {}", style("No active displays.").dim());
        println!();
        return Ok(());
    }

    for d in &displays {
        let name = labels.get(&d.id).cloned().unwrap_or_else(|| {
            if d.builtin {
                "Built-in".into()
            } else {
                "External".into()
            }
        });

        let mut tags = Vec::new();
        if d.main {
            tags.push(style("main").green().bold().to_string());
        }
        if Some(d.key) == pinned {
            tags.push(style("pinned").cyan().bold().to_string());
        }
        if d.builtin {
            tags.push(style("built-in").dim().to_string());
        }
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(" "))
        };

        println!(
            "  {} {}{}",
            style(&name).white().bold(),
            style(format!("({})", arrange::position_label(&displays, d.id))).cyan(),
            tags
        );
        println!(
            "    {}  {}",
            style("key    ").dim(),
            style(d.key.to_string()).yellow()
        );
        println!(
            "    {}  {}x{} at ({}, {})",
            style("layout ").dim(),
            d.size.0,
            d.size.1,
            d.origin.0,
            d.origin.1
        );
        println!(
            "    {}  product {} / serial {}",
            style("edid   ").dim(),
            style(d.key.model_hex()).dim(),
            style(d.key.serial_hex()).dim()
        );
        println!();
    }

    if pinned.is_none() {
        println!(
            "  {} Nothing pinned yet. Declare a key from this list as {} in {}.",
            style("i").blue(),
            style("preferred_main").bold(),
            style(config::config_path().display()).bold()
        );
        println!();
    }

    Ok(())
}

pub fn apply() -> Result<()> {
    report(display::enforce()?);
    Ok(())
}

pub fn status() -> Result<()> {
    let prefs = config::load()?;
    let displays = platform::list_displays()?;
    let labels = names::lookup();

    println!();
    println!("  {}", style("Main Display Pin").bold().underlined().cyan());
    println!();

    match prefs.preferred_main {
        Some(key) => {
            let connected = displays.iter().find(|d| d.key == key);
            let name = match connected {
                Some(d) => format!(
                    "{}, {}",
                    labels.get(&d.id).cloned().unwrap_or_else(|| "panel".into()),
                    arrange::position_label(&displays, d.id)
                ),
                None => "not connected".to_string(),
            };

            println!(
                "  {}  {} {}",
                style("pinned ").dim(),
                style(key.to_string()).yellow().bold(),
                style(format!("({name})")).dim()
            );

            let state = match connected {
                Some(d) if d.main => style("connected, holding the menu bar").green(),
                Some(_) => style("connected, but not main").yellow(),
                None => style("not connected").dim(),
            };
            println!("  {}  {}", style("state  ").dim(), state);
        }
        None => {
            println!("  {}  {}", style("pinned ").dim(), style("nothing").dim());
        }
    }

    let spaces = match spaces::separate_spaces_enabled() {
        Ok(true) => {
            style("saved: separate per display (takes effect at login)".to_string()).green()
        }
        Ok(false) => {
            style("saved: shared across displays (takes effect at login)".to_string()).red()
        }
        Err(e) => style(format!("unknown ({e})")).dim(),
    };
    println!("  {}  {}", style("spaces ").dim(), spaces);

    let trigger = if agent::is_loaded() {
        style("loaded").green()
    } else {
        style("not loaded, enable services.wsctl.display").yellow()
    };
    println!("  {}  {}", style("trigger").dim(), trigger);
    println!(
        "  {}  {}",
        style("watches").dim(),
        style(agent::WATCHED_PATH).dim()
    );
    println!(
        "  {}  {}",
        style("config ").dim(),
        style(config::config_path().display()).dim()
    );
    println!(
        "  {}  {}",
        style("log    ").dim(),
        style(agent::log_path().display()).dim()
    );
    println!();

    Ok(())
}

fn report(outcome: Outcome) {
    // Under launchd stdout is a log file and the timer fires every few seconds, so only a
    // real correction earns a line. Interactively, every outcome is worth printing.
    if !std::io::stdout().is_terminal() {
        match &outcome {
            Outcome::Moved(key) => tracing::info!("restored {key} as the main display"),
            other => tracing::debug!("no change needed ({other:?})"),
        }
        return;
    }

    match outcome {
        Outcome::NotConfigured => println!(
            "  {} Nothing pinned yet. Declare {} in {}.",
            style("!").yellow(),
            style("preferred_main").bold(),
            style(config::config_path().display()).bold()
        ),
        Outcome::Disconnected => println!(
            "  {} Pinned panel is not connected; leaving the layout alone.",
            style("i").blue()
        ),
        Outcome::AlreadyMain => println!(
            "  {} Pinned panel already holds the menu bar.",
            style("✓").green()
        ),
        Outcome::Moved(key) => println!(
            "  {} Moved the menu bar back to {}.",
            style("✓").green(),
            style(key.to_string()).bold()
        ),
    }
}
