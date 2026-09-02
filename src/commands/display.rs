use std::io::IsTerminal;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use console::style;
use display::{DisplayKey, Outcome, agent, arrange, config, names, platform, spaces};

/// How often the watcher re-reads the display layout.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
            "  {} Nothing pinned yet. Run {} while the right panel holds the menu bar.",
            style("i").blue(),
            style("wsctl display pin").bold()
        );
        println!();
    }

    Ok(())
}

pub fn pin(key: Option<String>) -> Result<()> {
    let displays = platform::list_displays()?;

    let target = match key {
        Some(raw) => DisplayKey::from_str(&raw).context("invalid display key")?,
        None => {
            arrange::current_main(&displays)
                .context("no main display found; pass a key from `wsctl display list`")?
                .key
        }
    };

    if !displays.iter().any(|d| d.key == target) {
        println!(
            "  {} {} is not connected right now. Pinning it anyway.",
            style("!").yellow(),
            style(target.to_string()).bold()
        );
    }

    if target.is_ambiguous() {
        println!(
            "  {} This panel reports no EDID serial, so an identical model cannot be told apart from it.",
            style("!").yellow()
        );
    }

    if arrange::has_duplicate_key(&displays, target) {
        println!(
            "  {} Two connected panels share this key, so which one wins is arbitrary.",
            style("!").yellow()
        );
    }

    let mut prefs = config::load()?;
    prefs.preferred_main = Some(target);
    config::save(&prefs)?;

    println!(
        "  {} Pinned {} as the main display.",
        style("✓").green(),
        style(target.to_string()).bold()
    );
    println!("    {}", style(config::config_path().display()).dim());

    report(display::enforce()?);

    if !agent::is_installed() {
        println!();
        println!(
            "  {} Run {} to keep it pinned across dock reconnects.",
            style("i").blue(),
            style("wsctl display enable").bold()
        );
    }

    Ok(())
}

pub fn apply() -> Result<()> {
    report(display::enforce()?);
    Ok(())
}

/// Long-running mode used by the launchd agent.
pub fn watch() -> Result<()> {
    tracing::info!("watching for display changes");
    platform::watch(POLL_INTERVAL, || match display::enforce() {
        Ok(Outcome::Moved(key)) => tracing::info!("restored {key} as the main display"),
        Ok(outcome) => tracing::debug!("no change needed ({outcome:?})"),
        Err(e) => tracing::error!("could not enforce the main display: {e:#}"),
    })
}

pub fn enable() -> Result<()> {
    if config::load()?.preferred_main.is_none() {
        anyhow::bail!("nothing pinned yet; run `wsctl display pin` first");
    }

    let exe = std::env::current_exe()
        .context("could not determine the wsctl binary path")?
        .canonicalize()
        .context("could not resolve the wsctl binary path")?;

    if exe.components().any(|c| c.as_os_str() == "target") {
        println!(
            "  {} Registering a build-directory binary at {}. Re-run this after `just install`.",
            style("!").yellow(),
            style(exe.display()).dim()
        );
    }

    let installed = agent::install(&exe)?;

    println!(
        "  {} Armed. launchd will re-apply the pin whenever the display layout changes.",
        style("✓").green()
    );
    println!("    {}", style(installed.plist.display()).dim());
    println!("    {}", style(agent::log_path().display()).dim());
    if !installed.approved {
        println!(
            "  {} macOS is holding it until Workstation is allowed under System Settings > Login Items.",
            style("!").yellow()
        );
    }
    Ok(())
}

pub fn disable() -> Result<()> {
    agent::uninstall()?;
    println!("  {} Trigger removed.", style("✓").green());
    println!(
        "    {}",
        style("Your pinned panel is kept; run `wsctl display enable` to resume.").dim()
    );
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
        Ok(true) => style("separate per display".to_string()).green(),
        Ok(false) => style("shared across displays, a swipe moves both".to_string()).red(),
        Err(e) => style(format!("unknown ({e})")).dim(),
    };
    println!("  {}  {}", style("spaces ").dim(), spaces);

    let trigger = match (agent::is_installed(), agent::is_loaded()) {
        (true, true) => style("armed").green(),
        (true, false) => style("installed but not loaded").yellow(),
        (false, _) => style("not enabled").dim(),
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
            "  {} Nothing pinned yet. Run {} first.",
            style("!").yellow(),
            style("wsctl display pin").bold()
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
