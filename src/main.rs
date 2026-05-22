mod builder;
mod cli;
mod commands;
mod config;
mod tui;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::IsTerminal;

use cli::{Cli, Commands, DiskCommand, SelfCommand};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = match cli.verbose {
        0 if cli.quiet => "error",
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .without_time()
        .init();

    let workstation = config::config();

    match cli.command {
        Commands::Apply {
            profile,
            dry_run,
            yes,
        } => {
            commands::apply::run(&workstation, &profile, dry_run, yes)?;
        }
        Commands::Diff { profile, json } => {
            commands::diff::run(&workstation, &profile, json)?;
        }
        Commands::Profiles { json } => {
            commands::profiles::run(&workstation, json)?;
        }
        Commands::Disk { sub } => match sub {
            DiskCommand::Cleanup { report } => {
                if report || !std::io::stdout().is_terminal() {
                    commands::audit::run_report()?;
                } else {
                    tui::cleanup::run()?;
                }
            }
        },
        Commands::Packages => {
            tui::packages::run()?;
        }
        Commands::SelfCmd(sub) => match sub {
            SelfCommand::Update { yes } => commands::self_cmd::update(yes)?,
            SelfCommand::Uninstall { yes } => commands::self_cmd::uninstall(yes)?,
        },
    }

    Ok(())
}
