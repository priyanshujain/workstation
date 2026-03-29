mod builder;
mod commands;
mod config;
mod tui;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "wsctl")]
#[command(about = "Workstation controller — manage macOS setup declaratively")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Install/update packages for a profile
    Apply {
        /// Profile to apply
        profile: String,

        /// Dry-run mode (show what would change without making changes)
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Preview what would change
    Diff {
        /// Profile to diff
        profile: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// List available profiles and scopes
    Profiles {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show disk usage by category
    Audit {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Interactive TUI for disk cleanup
    Cleanup,
}

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
        Commands::Audit { json } => {
            commands::audit::run(json)?;
        }
        Commands::Cleanup => {
            tui::run()?;
        }
    }

    Ok(())
}
