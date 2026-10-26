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
    /// Switch to the locked configuration: platform, distro packages, home-manager
    Apply {
        /// Tracked inputs to move with --update; all of them when none are given
        #[arg(requires = "update")]
        inputs: Vec<String>,

        /// Move your tracked repos to their latest commit before switching
        #[arg(short, long)]
        update: bool,

        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value_t = default_flake())]
        flake: String,

        /// Build and show what would change without changing anything
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Check the live machine against the flake; any failure exits nonzero
    Doctor {
        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value_t = default_flake())]
        flake: String,
    },

    /// Show where an option or a package is declared, and its value
    Why {
        /// Option path such as `pacman.packages`, or a package name
        query: String,

        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value_t = default_flake())]
        flake: String,
    },

    /// Compare every pin with its source: flake inputs, the pacman snapshot, Homebrew
    Outdated {
        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value_t = default_flake())]
        flake: String,
    },

    /// Move third-party pins to their latest, then switch
    Upgrade {
        /// Inputs to move; every untracked input when none are given
        inputs: Vec<String>,

        /// Move pacman.snapshot to today instead of flake inputs
        #[arg(long)]
        snapshot: bool,

        /// Build and show what would change without moving pins or switching
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Flake holding the configuration, with an optional #host (defaults to `hostname -s`)
        #[arg(long, default_value_t = default_flake())]
        flake: String,
    },

    /// Where the disk went, one line per kind of data
    Disk {
        #[command(subcommand)]
        sub: Option<DiskCommand>,

        /// Rescan instead of reading the cached report, and refresh the cache
        #[arg(long)]
        no_cache: bool,
    },

    /// Running coding agents, the processes they orphaned and the clones they left
    Agents {
        #[command(subcommand)]
        sub: Option<AgentsCommand>,
    },

    #[cfg(target_os = "macos")]
    /// Remove an application and the support files it leaves behind
    App {
        #[command(subcommand)]
        sub: AppCommand,
    },

    #[cfg(target_os = "macos")]
    /// Point each declared file type at its declared app; macOS asks once per changed type
    DefaultApps,

    #[cfg(target_os = "macos")]
    /// Keep the menu bar on a chosen monitor across dock reconnects
    Display {
        #[command(subcommand)]
        sub: DisplayCommand,
    },

    /// Interactive TUI for Brewfile package cleanup (uninstall + autoremove + Brewfile edit)
    Packages,

    /// Manage the wsctl binary itself (uninstall)
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

    /// Show what is pinned and whether the launchd job is loaded
    Status,
}

#[derive(Subcommand)]
pub enum DiskCommand {
    /// Browse usage folder by folder, or print the full report when piped
    Browse {
        /// Rescan instead of reading the cached report, and refresh the cache
        #[arg(long)]
        no_cache: bool,
    },

    /// Browse and delete what is safe to remove, sizes filling in as it measures
    Clean,

    /// Rebuild the cached report without a terminal (what the scheduled job runs)
    #[command(hide = true)]
    Refresh,

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

#[derive(Subcommand)]
pub enum AgentsCommand {
    /// List orphaned processes, temp clones and unused worktrees without touching them
    Leftovers,

    /// Ask about each leftover, then stop or remove it
    Clean,
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Uninstall the wsctl binary
    Uninstall {
        /// Don't ask for confirmation
        #[arg(short = 'y', long)]
        yes: bool,

        /// Also remove logs, caches and config
        #[arg(long)]
        purge: bool,
    },
}

fn default_flake() -> String {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("home-manager")
        .display()
        .to_string()
}
