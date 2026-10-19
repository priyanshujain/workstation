use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use agents::config;
use agents::process::{self, Process, Table};
use agents::running;
use console::style;

fn home_generation() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join(".local/state/home-manager/gcroots/current-home")
    } else {
        home.join(".local/state/nix/profiles/home-manager")
    }
}

pub fn find(table: &Table) -> Vec<Process> {
    let Ok(config) = config::load() else {
        return Vec::new();
    };
    let Some(activated) = std::fs::symlink_metadata(home_generation())
        .and_then(|meta| meta.modified())
        .ok()
    else {
        return Vec::new();
    };
    running::daemons(table, &config)
        .into_iter()
        .filter(|daemon| {
            SystemTime::now()
                .checked_sub(Duration::from_secs(daemon.elapsed_secs))
                .is_some_and(|started| started < activated)
        })
        .collect()
}

pub fn restart_unused() {
    let table = Table::snapshot();
    for daemon in find(&table) {
        if running::has_session(&table, &daemon.name) {
            println!(
                "{} {} daemon {} predates this apply, restart it once its sessions close",
                style("!").yellow(),
                daemon.name,
                daemon.pid
            );
            continue;
        }
        match process::terminate(daemon.pid) {
            Ok(()) => println!(
                "    {}",
                style(format!(
                    "stopped stale {} daemon {}",
                    daemon.name, daemon.pid
                ))
                .dim()
            ),
            Err(e) => println!(
                "{} could not stop {} daemon {}: {e}",
                style("!").yellow(),
                daemon.name,
                daemon.pid
            ),
        }
    }
}
