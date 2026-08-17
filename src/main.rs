mod builder;
mod cli;
mod commands;
mod config;
mod tui;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::IsTerminal;

use cli::{
    AppCommand, AudioCommand, Cli, Commands, DiskAgentCommand, DiskCommand, DisplayCommand,
    SelfCommand,
};

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
            DiskCommand::Audit { no_cache } => {
                commands::audit::run_report(no_cache)?;
            }
            DiskCommand::Cleanup => {
                if std::io::stdout().is_terminal() {
                    tui::cleanup::run()?;
                } else {
                    commands::audit::run_report(false)?;
                }
            }
            DiskCommand::Agent { sub } => match sub {
                DiskAgentCommand::Enable => commands::disk_agent::enable()?,
                DiskAgentCommand::Disable => commands::disk_agent::disable()?,
                DiskAgentCommand::Status => commands::disk_agent::status()?,
            },
            DiskCommand::Projects {
                roots,
                idle_days,
                min_size_mb,
                clean,
                dry_run,
                yes,
                no_cache,
            } => {
                commands::projects::run(commands::projects::Options {
                    roots,
                    idle_days,
                    min_size_mb,
                    clean,
                    dry_run,
                    yes,
                    no_cache,
                })?;
            }
        },
        Commands::Display { sub } => match sub {
            DisplayCommand::List => commands::display::list()?,
            DisplayCommand::Pin { key } => commands::display::pin(key)?,
            DisplayCommand::Apply => commands::display::apply()?,
            DisplayCommand::Status => commands::display::status()?,
            DisplayCommand::Enable => commands::display::enable()?,
            DisplayCommand::Disable => commands::display::disable()?,
            DisplayCommand::Watch => commands::display::watch()?,
        },
        Commands::App { sub } => match sub {
            AppCommand::Remove {
                name,
                dry_run,
                purge,
                include_likely,
                yes,
            } => commands::app::remove(name, dry_run, purge, include_likely, yes)?,
        },
        Commands::Audio { sub } => match sub {
            AudioCommand::Install => commands::audio::install()?,
            AudioCommand::Uninstall => commands::audio::uninstall()?,
            AudioCommand::Status => commands::audio::status()?,
            AudioCommand::Devices => commands::audio::devices()?,
            AudioCommand::Bridge {
                input,
                output,
                seconds,
                denoise,
                voice_threshold,
            } => commands::audio::bridge(&input, &output, seconds, denoise, voice_threshold)?,
            AudioCommand::Calibrate {
                input,
                output,
                probes,
                dry_run,
            } => commands::audio::calibrate(&input, &output, probes, dry_run)?,
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
