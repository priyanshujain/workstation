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

    /// Inspect and clean up disk usage
    Disk {
        #[command(subcommand)]
        sub: DiskCommand,
    },

    /// Remove an application and the support files it leaves behind
    App {
        #[command(subcommand)]
        sub: AppCommand,
    },

    /// Interactive TUI for Brewfile package cleanup (uninstall + autoremove + Brewfile edit)
    Packages,

    /// Manage the wsctl binary itself (update, uninstall)
    #[command(name = "self", subcommand)]
    SelfCmd(SelfCommand),
}

#[derive(Subcommand)]
pub enum AppCommand {
    /// Delete an app plus its data, launchd jobs, symlinks and installer receipt
    Remove {
        /// App name as it appears in /Applications, or an absolute .app path
        name: String,

        /// Show what would go without removing anything
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Erase instead of moving to the Trash
        #[arg(long)]
        purge: bool,

        /// Also remove folders named for the app or its vendor
        #[arg(long)]
        include_likely: bool,

        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum DiskCommand {
    /// Show disk usage by category (read-only summary)
    Audit,

    /// Interactive disk cleanup: audit + drill-down + delete in one TUI
    Cleanup,
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
