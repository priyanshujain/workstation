use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wsctl")]
#[command(about = "Margin Workstation: developer control over desktop configuration")]
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
    /// Apply the flake's configuration: the platform switch, distro packages and home-manager
    Apply {
        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value = ".")]
        flake: String,

        /// Build and show what would change without changing anything
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Check the live machine against the flake; any failure exits nonzero
    Doctor {
        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value = ".")]
        flake: String,
    },

    /// Show where an option or a package is declared, and its value
    Why {
        /// Option path such as `pacman.packages`, or a package name
        query: String,

        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value = ".")]
        flake: String,
    },

    /// Compare every pin with its source: flake inputs, the pacman snapshot, Homebrew
    Outdated {
        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value = ".")]
        flake: String,
    },

    /// Move the pins: update flake inputs, or the pacman snapshot to today
    Update {
        /// Inputs to update; all of them when none are given
        inputs: Vec<String>,

        /// Move pacman.snapshot to today instead of updating flake inputs
        #[arg(long)]
        snapshot: bool,

        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value = ".")]
        flake: String,
    },

    /// Inspect and clean up disk usage
    Disk {
        #[command(subcommand)]
        sub: DiskCommand,
    },

    #[cfg(target_os = "macos")]
    /// Remove an application and the support files it leaves behind
    App {
        #[command(subcommand)]
        sub: AppCommand,
    },

    #[cfg(target_os = "macos")]
    /// Keep the menu bar on a chosen monitor across dock reconnects
    Display {
        #[command(subcommand)]
        sub: DisplayCommand,
    },

    /// Interactive TUI for Brewfile package cleanup (uninstall + autoremove + Brewfile edit)
    Packages,

    /// Manage the wsctl binary itself (update, uninstall)
    #[command(name = "self", subcommand)]
    SelfCmd(SelfCommand),
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
pub enum DisplayCommand {
    /// Show connected displays and their keys
    List,

    /// Move the menu bar back to the pinned panel now
    Apply,

    /// Show what is pinned and whether the watcher is running
    Status,

    /// Start watching for display changes in the background (launchd agent)
    Enable,

    /// Stop watching for display changes
    Disable,

    /// Run the watcher in the foreground (used by the launchd agent)
    #[command(hide = true)]
    Watch,
}

#[derive(Subcommand)]
pub enum DiskCommand {
    /// Where the space went: a read-only TUI that fills in as it measures,
    /// or the plain text report when piped
    Audit {
        /// Rescan instead of reading the cached report, and refresh the cache
        #[arg(long)]
        no_cache: bool,
    },

    /// Browse and delete what is safe to remove, sizes filling in as it measures
    Cleanup,

    #[cfg(target_os = "macos")]
    /// Manage the scheduled refresh of the cached disk report
    Agent {
        #[command(subcommand)]
        sub: DiskAgentCommand,
    },

    /// Build artifacts per project, with how long each has sat idle
    Projects {
        /// Where to look, repeatable (default: ~/Workspace and ~/go/src)
        #[arg(long = "root")]
        roots: Vec<std::path::PathBuf>,

        /// Only clean projects whose sources have not changed in this many days
        #[arg(long, default_value_t = 7)]
        idle_days: u64,

        /// Hide projects with less than this many MB of artifacts
        #[arg(long, default_value_t = 50)]
        min_size_mb: u64,

        /// Remove the eligible artifacts instead of only listing them
        #[arg(long)]
        clean: bool,

        /// With --clean, show what would go without removing anything
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,

        /// Rescan instead of reading the cached report, and refresh the cache
        #[arg(long)]
        no_cache: bool,
    },
}

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
pub enum DiskAgentCommand {
    /// Install and load the scheduled refresh
    Enable,

    /// Unload and remove the scheduled refresh
    Disable,

    /// Show cache age and whether the refresh is armed
    Status,

    /// Rebuild the cache without opening anything macOS would ask about
    /// (what the launchd job runs)
    #[command(hide = true)]
    Refresh,
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Update wsctl to the latest release
    Update {
        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Uninstall the wsctl binary, its launchd jobs and the Margin Workstation app
    Uninstall {
        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,

        /// Also remove logs, caches and config
        #[arg(long)]
        purge: bool,
    },

    #[cfg(target_os = "macos")]
    /// Register a job of the Workstation bundle with launchd. Run by the bundled copy:
    /// the Service Management API registers jobs of the calling app only.
    #[command(hide = true)]
    RegisterAgent { label: String },

    #[cfg(target_os = "macos")]
    /// Unregister a job of the Workstation bundle. Run by the bundled copy.
    #[command(hide = true)]
    UnregisterAgent { label: String },

    #[cfg(target_os = "macos")]
    /// Print the SMAppService status of a job of the Workstation bundle. Run by the bundled copy.
    #[command(hide = true)]
    AgentStatus { label: String },
}
