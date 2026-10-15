use std::path::{Path, PathBuf};

use agents::config::{self, Config};
use agents::leftovers::{self, Kind, Leftover};
use agents::process::{self, Table};
use agents::running::{self, Orphan};
use anyhow::{Result, bail};
use console::style;
use disk::liveness::Liveness;
use disk::util::format_size;

pub fn list() -> Result<()> {
    let config = load_config()?;
    let table = Table::snapshot();
    let running = running::agents(&table, &config);
    if running.is_empty() {
        println!("no agents running");
    }
    for agent in &running {
        println!(
            "{:<8} {:>7} {:>8} {:>9}  {}",
            agent.process.name,
            agent.process.pid,
            age(agent.process.elapsed_secs),
            format_size(agent.tree_rss_bytes),
            shown(agent.cwd.as_deref()),
        );
    }
    let orphans = running::orphans(&table, &config).len();
    if orphans > 0 {
        println!(
            "{}",
            style(format!(
                "{orphans} orphaned, `wsctl agents leftovers` to see them"
            ))
            .yellow()
        );
    }
    Ok(())
}

pub fn leftovers() -> Result<()> {
    let found = Found::scan()?;
    if found.is_empty() {
        println!("no leftovers");
    }
    for orphan in &found.orphans {
        println!("{}", describe_orphan(orphan));
    }
    for leftover in &found.leftovers {
        println!("{}", describe_leftover(leftover));
    }
    Ok(())
}

pub fn clean() -> Result<()> {
    let found = Found::scan()?;
    if found.is_empty() {
        println!("no leftovers");
        return Ok(());
    }
    for orphan in &found.orphans {
        if confirm(&format!("stop {}?", describe_orphan(orphan)))? {
            match stop_tree(&found.table, orphan.process.pid) {
                Ok(()) => println!("  stopped"),
                Err(err) => println!("  {}", style(err).red()),
            }
        }
    }
    for leftover in &found.leftovers {
        if confirm(&format!("remove {}?", describe_leftover(leftover)))? {
            match leftovers::remove(leftover) {
                Ok(()) => println!("  removed"),
                Err(reason) => println!("  {}", style(format!("kept: {reason}")).yellow()),
            }
        }
    }
    Ok(())
}

pub fn disk_line() -> Option<String> {
    let config = config::load()
        .ok()
        .filter(|config| !config.agents.is_empty())?;
    let table = Table::snapshot();
    let found = leftovers::find(&agent_cwds(&table, &config), &Liveness::snapshot());
    if found.is_empty() {
        return None;
    }
    let bytes: u64 = found
        .iter()
        .map(|leftover| disk::util::dir_size(&leftover.path))
        .sum();
    Some(format!(
        "agent leftovers  {} in {} places, `wsctl agents clean` to review",
        format_size(bytes),
        found.len()
    ))
}

fn agent_cwds(table: &Table, config: &Config) -> Vec<PathBuf> {
    running::agents(table, config)
        .into_iter()
        .filter_map(|agent| agent.cwd)
        .collect()
}

struct Found {
    table: Table,
    orphans: Vec<Orphan>,
    leftovers: Vec<Leftover>,
}

impl Found {
    fn scan() -> Result<Self> {
        let config = load_config()?;
        let table = Table::snapshot();
        let orphans = running::orphans(&table, &config);
        let leftovers = leftovers::find(&agent_cwds(&table, &config), &Liveness::snapshot());
        Ok(Self {
            table,
            orphans,
            leftovers,
        })
    }

    fn is_empty(&self) -> bool {
        self.orphans.is_empty() && self.leftovers.is_empty()
    }
}

fn load_config() -> Result<Config> {
    let config = config::load()?;
    if config.agents.is_empty() {
        bail!(
            "no agents declared: set services.wsctl.agents, which writes {}",
            config::path().display()
        );
    }
    Ok(config)
}

fn stop_tree(table: &Table, pid: i32) -> std::io::Result<()> {
    for child in table.descendants(pid) {
        let _ = process::terminate(child.pid);
    }
    process::terminate(pid)
}

fn describe_orphan(orphan: &Orphan) -> String {
    format!(
        "orphan   {} (pid {}, {}, {}) in {}",
        orphan.process.name,
        orphan.process.pid,
        age(orphan.process.elapsed_secs),
        format_size(orphan.process.rss_bytes),
        shown(orphan.cwd.as_deref()),
    )
}

fn describe_leftover(leftover: &Leftover) -> String {
    match &leftover.kind {
        Kind::TempClone => format!("clone    {}", shown(Some(&leftover.path))),
        Kind::Worktree { repo } => format!(
            "worktree {} of {}",
            shown(Some(&leftover.path)),
            shown(Some(repo))
        ),
    }
}

fn shown(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "?".into();
    };
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn age(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}m", s / 60),
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}
