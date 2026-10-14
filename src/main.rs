mod cli;
mod commands;
mod elevate;
mod tui;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::IsTerminal;

#[cfg(target_os = "macos")]
use cli::{AppCommand, DisplayCommand};
use cli::{Cli, Commands, DiskCommand, SelfCommand};

fn main() -> anyhow::Result<()> {
    // Rust ignores SIGPIPE, so println! panics once a reader goes away and
    // `wsctl disk browse | head` ends in a backtrace instead of quietly.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

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

    match cli.command {
        Commands::Apply { flake, dry_run } => {
            let target = commands::apply::Target::parse(&flake)?;
            commands::apply::run_apply(&target, dry_run)?;
        }
        Commands::Doctor { flake } => {
            let target = commands::apply::Target::parse(&flake)?;
            if !commands::doctor::run(&target)? {
                std::process::exit(1);
            }
        }
        Commands::Why { query, flake } => {
            commands::why::run(&commands::apply::Target::parse(&flake)?, &query)?;
        }
        Commands::Outdated { flake } => {
            commands::outdated::run(&commands::apply::Target::parse(&flake)?)?;
        }
        Commands::Update {
            inputs,
            snapshot,
            flake,
        } => {
            commands::update::run(&commands::apply::Target::parse(&flake)?, &inputs, snapshot)?;
        }
        Commands::Disk { sub, no_cache } => {
            let walks = match &sub {
                None => no_cache || disk::report::load(std::time::SystemTime::now()).is_none(),
                Some(DiskCommand::Browse { .. } | DiskCommand::Clean) => true,
                Some(DiskCommand::Refresh | DiskCommand::Projects { .. }) => false,
            };
            if walks {
                elevate::rerun_as_root();
            }
            if let Some(notice) = disk::report::privileged_notice() {
                tracing::info!("{notice}");
            }
            match sub {
                None => commands::disk::summary(no_cache)?,
                Some(DiskCommand::Browse { no_cache }) => {
                    if std::io::stdout().is_terminal() {
                        tui::audit::run(no_cache)?;
                    } else {
                        commands::audit::run_report(no_cache)?;
                    }
                }
                Some(DiskCommand::Clean) => {
                    if std::io::stdout().is_terminal() {
                        tui::cleanup::run()?;
                    } else {
                        commands::audit::run_report(false)?;
                    }
                }
                Some(DiskCommand::Refresh) => commands::disk::refresh()?,
                Some(DiskCommand::Projects {
                    roots,
                    idle_days,
                    min_size_mb,
                    clean,
                    dry_run,
                    yes,
                    no_cache,
                }) => {
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
            }
        }
        #[cfg(target_os = "macos")]
        Commands::Display { sub } => match sub {
            DisplayCommand::List => commands::display::list()?,
            DisplayCommand::Apply => commands::display::apply()?,
            DisplayCommand::Status => commands::display::status()?,
            DisplayCommand::Enable => commands::display::enable()?,
            DisplayCommand::Disable => commands::display::disable()?,
            DisplayCommand::Watch => commands::display::watch()?,
        },
        #[cfg(target_os = "macos")]
        Commands::App { sub } => match sub {
            AppCommand::Remove {
                name,
                dry_run,
                purge,
                include_likely,
                yes,
            } => commands::app::remove(name, dry_run, purge, include_likely, yes)?,
        },
        Commands::Packages => {
            tui::packages::run()?;
        }
        Commands::SelfCmd(sub) => match sub {
            SelfCommand::Update { yes } => commands::self_cmd::update(yes)?,
            SelfCommand::Uninstall { yes, purge } => commands::self_cmd::uninstall(yes, purge)?,
            #[cfg(target_os = "macos")]
            SelfCommand::RegisterAgent { label } => wsctl_core::bundle::register_here(&label)?,
            #[cfg(target_os = "macos")]
            SelfCommand::UnregisterAgent { label } => wsctl_core::bundle::unregister_here(&label)?,
            #[cfg(target_os = "macos")]
            SelfCommand::AgentStatus { label } => {
                println!("{}", wsctl_core::bundle::status_here(&label))
            }
        },
    }

    Ok(())
}
