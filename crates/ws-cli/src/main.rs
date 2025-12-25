//! ws CLI - Declarative workstation configuration
//!
//! Main entry point for the ws command-line tool.

mod commands;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "ws")]
#[command(about = "Declarative workstation configuration", long_about = None)]
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
    /// Apply the declared configuration
    Apply {
        /// Profile to apply
        #[arg(short, long)]
        profile: String,

        /// Dry-run mode (show what would change without making changes)
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Show what would change (dry-run)
    Diff {
        /// Profile to diff
        #[arg(short, long)]
        profile: String,
    },

    /// List available profiles
    Profiles,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
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

    // Load the workstation configuration
    // For now, we use the example config directly
    // In the future, this will load from the user's config crate
    let workstation = example_config();

    match cli.command {
        Commands::Apply {
            profile,
            dry_run,
            yes,
        } => {
            commands::apply::run(&workstation, &profile, dry_run, yes)?;
        }
        Commands::Diff { profile } => {
            commands::diff::run(&workstation, &profile)?;
        }
        Commands::Profiles => {
            commands::profiles::run(&workstation)?;
        }
    }

    Ok(())
}

/// Example configuration for development
///
/// In production, this would be loaded from the user's config crate
fn example_config() -> ws_dsl::Workstation {
    use ws_dsl::prelude::*;

    Workstation::builder("pj-workstation")
        // Personal tools scope
        .scope("personal", |s| {
            s.brew_cask("ghostty")
                .brew_cask("raycast")
                .brew_cask("visual-studio-code")
                .brew_formula("git")
                .brew_formula("ripgrep")
                .brew_formula("fzf")
                .brew_formula("neovim")
                .brew_cask("docker")
        })
        // Work (OkCredit) scope
        .scope("okcredit", |s| s.brew_cask("datagrip"))
        // Machine profiles
        .profile("personal-macbook", &["personal"])
        .profile("work-macbook", &["personal", "okcredit"])
        .build()
}
