use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wsctl")]
#[command(about = "Workstation controller, manage macOS setup declaratively")]
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

    /// Manage the virtual audio devices and the echo-cancelling bridge
    Audio {
        #[command(subcommand)]
        sub: AudioCommand,
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
pub enum AudioCommand {
    /// Install the virtual audio devices (asks for sudo, restarts coreaudiod)
    Install,

    /// Remove the virtual audio devices (asks for sudo, restarts coreaudiod)
    Uninstall,

    /// Show whether the devices are installed and loaded
    Status,

    /// List the real devices the bridge can use, by UID
    Devices,

    /// Run the echo-cancelling bridge until interrupted
    Bridge {
        /// UID of the real microphone
        #[arg(short, long)]
        input: String,

        /// UID of the real speaker
        #[arg(short, long)]
        output: String,

        /// Stop after this many seconds instead of running until interrupted
        #[arg(long)]
        seconds: Option<u64>,

        /// Suppress background noise on the microphone with RNNoise
        #[arg(long)]
        denoise: bool,

        /// Fade the microphone out below this voice probability, 0 to 1
        #[arg(long, default_value_t = 0.0)]
        voice_threshold: f32,
    },

    /// Measure the real round trip between a speaker and a microphone, out loud
    Calibrate {
        /// UID of the real microphone
        #[arg(short, long)]
        input: String,

        /// UID of the real speaker
        #[arg(short, long)]
        output: String,

        /// How many times to play the probe
        #[arg(long)]
        probes: Option<usize>,

        /// Measure and report without saving the correction
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum DiskCommand {
    /// Show disk usage by category (read-only summary)
    Audit {
        /// Rescan instead of reading the cached report, and refresh the cache
        #[arg(long)]
        no_cache: bool,
    },

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
