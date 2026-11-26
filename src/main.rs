mod cli;
mod commands;
mod elevate;
mod tui;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use std::io::IsTerminal;

use cli::{
    AgentsCommand, Cli, Commands, DiskCommand, PackagesCommand, SelfCommand, ShareCommand,
    ShareMode,
};
#[cfg(target_os = "macos")]
use cli::{AppCommand, DisplayCommand};

fn use_askpass_without_terminal() {
    if std::io::stdin().is_terminal()
        || wsctl_core::privilege::unattended()
        || std::env::var_os("SUDO_ASKPASS").is_some()
    {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let askpass = exe.with_file_name("wsctl-askpass");
    if askpass.is_file() {
        // SAFETY: runs first in main, before any thread exists.
        unsafe { std::env::set_var("SUDO_ASKPASS", askpass) };
    }
}

fn main() -> anyhow::Result<()> {
    // Rust ignores SIGPIPE, so println! panics once a reader goes away and
    // `wsctl disk browse | head` ends in a backtrace instead of quietly.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    use_askpass_without_terminal();

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
        Commands::Apply {
            inputs,
            update,
            flake,
            dry_run,
        } => {
            let target = commands::apply::Target::parse(&flake)?;
            let pins = update.then_some(commands::pins::Pins::Tracked);
            commands::apply::sync(&target, pins, &inputs, dry_run)?;
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
        Commands::Upgrade {
            inputs,
            snapshot,
            dry_run,
            flake,
        } => {
            let target = commands::apply::Target::parse(&flake)?;
            commands::upgrade::run(&target, &inputs, snapshot, dry_run)?;
        }
        Commands::Disk {
            sub,
            no_cache,
            refresh,
        } => {
            let cache_missing = || disk::report::load(std::time::SystemTime::now()).is_none();
            let walks = match &sub {
                None => no_cache || !refresh.is_empty() || cache_missing(),
                Some(DiskCommand::List {
                    no_cache, refresh, ..
                }) => *no_cache || !refresh.is_empty() || cache_missing(),
                Some(DiskCommand::Browse { no_cache }) => *no_cache || cache_missing(),
                Some(
                    DiskCommand::Clean { .. } | DiskCommand::Refresh | DiskCommand::Projects { .. },
                ) => false,
            };
            if walks {
                elevate::rerun_as_root();
            }
            if let Some(notice) = disk::report::privileged_notice() {
                tracing::info!("{notice}");
            }
            match sub {
                None => commands::disk::summary(no_cache, &refresh)?,
                Some(DiskCommand::Browse { no_cache }) => commands::audit::run_report(no_cache)?,
                Some(DiskCommand::Clean {
                    items,
                    dry_run,
                    yes,
                }) => commands::disk_clean::run(&items, dry_run, yes)?,
                Some(DiskCommand::Refresh) => commands::disk::refresh()?,
                Some(DiskCommand::List {
                    kind,
                    no_cache,
                    refresh,
                }) => commands::disk::list(kind.as_deref(), no_cache, &refresh)?,
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
        Commands::Agents { sub } => match sub {
            None => commands::agents::list()?,
            Some(AgentsCommand::Leftovers) => commands::agents::leftovers()?,
            Some(AgentsCommand::Clean { yes }) => commands::agents::clean(yes)?,
        },
        #[cfg(target_os = "macos")]
        Commands::Display { sub } => match sub {
            DisplayCommand::List => commands::display::list()?,
            DisplayCommand::Apply => commands::display::apply()?,
            DisplayCommand::Status => commands::display::status()?,
        },
        #[cfg(target_os = "macos")]
        Commands::DefaultApps { yes } => commands::default_apps::run(yes)?,
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
        Commands::Share { sub, target } => match sub {
            None => commands::share::share(share_target(target))?,
            Some(sub) => match *sub {
                ShareCommand::List => commands::share::list()?,
                ShareCommand::Manage => commands::share::manage()?,
                ShareCommand::Stop { block, all } => commands::share::stop(block, all)?,
                ShareCommand::Serve { target } => commands::share::serve(share_target(target))?,
                ShareCommand::Relay {
                    viewer_listen,
                    agent_listen,
                    public_url,
                    secret_file,
                    db_url,
                    db_token_file,
                    trust_proxy_header,
                } => {
                    let config = share_relay::RelayConfig {
                        viewer_listen,
                        agent_listen,
                        public_url,
                        secret: String::new(),
                        db_url,
                        db_token_file,
                        trust_proxy_header,
                    };
                    commands::share::relay(config, &secret_file)?;
                }
            },
        },
        Commands::Packages { sub, flake } => {
            let target = commands::apply::Target::parse(&flake)?;
            match sub {
                None => commands::packages::list(&target)?,
                Some(PackagesCommand::Remove {
                    names,
                    dry_run,
                    yes,
                }) => commands::packages::remove(&target, &names, dry_run, yes)?,
            }
        }
        Commands::SelfCmd(sub) => match sub {
            SelfCommand::Uninstall { yes, purge } => commands::self_cmd::uninstall(yes, purge)?,
        },
    }

    Ok(())
}

fn share_target(target: cli::ShareArgs) -> commands::share::Target {
    commands::share::Target {
        block: target.block,
        session: target.session,
        mode: target.mode.map(protocol_mode),
        guests: target.guests,
        ttl: target.ttl,
    }
}

fn protocol_mode(mode: ShareMode) -> share_protocol::Mode {
    match mode {
        ShareMode::View => share_protocol::Mode::View,
        ShareMode::Input => share_protocol::Mode::Input,
    }
}
