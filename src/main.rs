#[cfg(target_os = "macos")]
mod builder;
mod cli;
mod commands;
#[cfg(target_os = "macos")]
mod config;
mod tui;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::IsTerminal;

#[cfg(target_os = "macos")]
use cli::{AppCommand, DiskAgentCommand, DisplayCommand};
use cli::{Cli, Commands, DiskCommand, SelfCommand};

fn main() -> anyhow::Result<()> {
    // Rust ignores SIGPIPE, so println! panics once a reader goes away and
    // `wsctl disk audit | head` ends in a backtrace instead of quietly.
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

    #[cfg(target_os = "macos")]
    let workstation = config::config();

    match cli.command {
        #[cfg(target_os = "macos")]
        Commands::Apply {
            profile,
            dry_run,
            yes,
        } => {
            commands::apply::run(&workstation, &profile, dry_run, yes)?;
        }
        #[cfg(target_os = "macos")]
        Commands::Diff { profile, json } => {
            commands::diff::run(&workstation, &profile, json)?;
        }
        #[cfg(target_os = "macos")]
        Commands::Profiles { json } => {
            commands::profiles::run(&workstation, json)?;
        }
        Commands::Disk { sub } => {
            // Said before anything takes the screen, because a privileged run
            // writes its cache somewhere an unprivileged one would not.
            if let Some(notice) = disk::report::privileged_notice() {
                tracing::warn!("{notice}");
            }
            match sub {
                DiskCommand::Audit { no_cache } => {
                    if std::io::stdout().is_terminal() {
                        tui::audit::run(no_cache)?;
                    } else {
                        commands::audit::run_report(no_cache)?;
                    }
                }
                DiskCommand::Cleanup => {
                    if std::io::stdout().is_terminal() {
                        tui::cleanup::run()?;
                    } else {
                        commands::audit::run_report(false)?;
                    }
                }
                #[cfg(target_os = "macos")]
                DiskCommand::Agent { sub } => match sub {
                    DiskAgentCommand::Enable => commands::disk_agent::enable()?,
                    DiskAgentCommand::Disable => commands::disk_agent::disable()?,
                    DiskAgentCommand::Status => commands::disk_agent::status()?,
                    DiskAgentCommand::Refresh => commands::disk_agent::refresh()?,
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
            }
        }
        #[cfg(target_os = "macos")]
        Commands::Display { sub } => match sub {
            DisplayCommand::List => commands::display::list()?,
            DisplayCommand::Pin { key } => commands::display::pin(key)?,
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
