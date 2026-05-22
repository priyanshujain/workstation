use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wsctl")]
#[command(about = "Workstation controller — manage macOS setup declaratively")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Increase verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Commands {
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

    /// Interactive TUI for Brewfile package cleanup (uninstall + autoremove + Brewfile edit)
    Packages,

    /// Manage the wsctl binary itself (update, uninstall)
    #[command(name = "self", subcommand)]
    SelfCmd(SelfCommand),
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Update wsctl to the latest release
    Update {
        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Uninstall the wsctl binary
    Uninstall {
        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
