use std::collections::HashSet;
use std::path::PathBuf;

use crate::config::Config;
use crate::process::{self, Process, Table};

pub struct Running {
    pub process: Process,
    pub cwd: Option<PathBuf>,
    pub tree_rss_bytes: u64,
}

pub struct Orphan {
    pub process: Process,
    pub cwd: Option<PathBuf>,
}

pub fn agents(table: &Table, config: &Config) -> Vec<Running> {
    let mut found: Vec<Running> = table
        .processes
        .values()
        .filter(|process| is_agent(process, config))
        .filter(|process| !table.ancestors(process.pid).any(|up| is_agent(up, config)))
        .map(|process| Running {
            process: process.clone(),
            cwd: process::cwd(process.pid),
            tree_rss_bytes: process.rss_bytes
                + table
                    .descendants(process.pid)
                    .iter()
                    .map(|child| child.rss_bytes)
                    .sum::<u64>(),
        })
        .collect();
    found.sort_by_key(|running| std::cmp::Reverse(running.process.elapsed_secs));
    found
}

pub fn orphans(table: &Table, config: &Config) -> Vec<Orphan> {
    let markers: Vec<String> = config
        .agents
        .iter()
        .filter_map(|agent| agent.marker.clone())
        .collect();
    let carrying: HashSet<i32> = process::carrying(&markers).into_iter().collect();
    let mut found: Vec<Orphan> = table
        .processes
        .values()
        .filter(|process| carrying.contains(&process.pid))
        .filter(|process| !is_agent(process, config))
        .filter(|process| !table.ancestors(process.pid).any(|up| is_agent(up, config)))
        .filter(|process| {
            !table
                .ancestors(process.pid)
                .any(|up| carrying.contains(&up.pid))
        })
        .map(|process| Orphan {
            process: process.clone(),
            cwd: process::cwd(process.pid),
        })
        .collect();
    found.sort_by_key(|orphan| std::cmp::Reverse(orphan.process.elapsed_secs));
    found
}

fn is_agent(process: &Process, config: &Config) -> bool {
    config
        .agents
        .iter()
        .any(|agent| agent.command == process.name)
}
