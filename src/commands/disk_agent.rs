use anyhow::{Context, Result};
use console::style;
use disk::agent;
use disk::report;

pub fn enable() -> Result<()> {
    let exe = std::env::current_exe().context("could not resolve the running wsctl binary")?;
    let path = agent::install(&exe)?;

    println!();
    println!("  {}", style("Disk report refresh enabled").bold().green());
    println!("  plist    {}", style(path.display()).dim());
    println!("  log      {}", style(agent::log_path().display()).dim());
    println!(
        "  runs at  {}",
        style(
            agent::REFRESH_HOURS
                .iter()
                .map(|h| format!("{h:02}:00"))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .dim()
    );
    println!();
    Ok(())
}

pub fn disable() -> Result<()> {
    agent::uninstall()?;
    println!();
    println!("  {}", style("Disk report refresh disabled").bold());
    println!();
    Ok(())
}

pub fn status() -> Result<()> {
    println!();
    println!("  {}", style("Disk Report Cache").bold().underlined().cyan());
    println!();

    let cache = report::cache_path();
    match cache.as_deref() {
        Some(path) if path.exists() => {
            let age = report::load(std::time::SystemTime::now())
                .map(|r| report::humanize_age(r.age(std::time::SystemTime::now())));
            println!("  cache    {}", style(path.display()).dim());
            match age {
                Some(age) => println!("  updated  {}", style(age).green()),
                None => println!(
                    "  updated  {}",
                    style("unusable, will rescan on next read").yellow()
                ),
            }
        }
        Some(path) => {
            println!("  cache    {}", style(path.display()).dim());
            println!("  updated  {}", style("not built yet").yellow());
        }
        None => println!("  cache    {}", style("no data directory").red()),
    }

    let state = match (agent::is_installed(), agent::is_loaded()) {
        (true, true) => style("armed").green(),
        (true, false) => style("installed but not loaded").yellow(),
        (false, _) => style("not installed").dim(),
    };
    println!("  refresh  {state}");
    println!(
        "  runs at  {}",
        style(
            agent::REFRESH_HOURS
                .iter()
                .map(|h| format!("{h:02}:00"))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .dim()
    );
    println!();
    Ok(())
}
