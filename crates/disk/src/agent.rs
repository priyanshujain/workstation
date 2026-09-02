use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use wsctl_core::bundle::{self, Installed};

/// launchd label for the job that refreshes the cached disk report, and the
/// name of its plist inside the Workstation bundle. It lives under the bundle
/// id on purpose: Login Items keys its record by label and keeps the first
/// name it computed for it, so the old `wsctl` name could only be shed by
/// moving to a label macOS had never seen.
pub const LABEL: &str = "dev.pj.workstation.disk-report";

/// Labels earlier versions loaded from `~/Library/LaunchAgents`. The current
/// one is among them because it first shipped that way too.
const LEGACY_LABELS: [&str; 3] = [
    LABEL,
    "com.priyanshujain.workstation.disk-report",
    "com.priyanshujain.wsctl.disk-report",
];

/// Hours at which the refresh runs. Fixed clock times rather than
/// `StartInterval`: launchd fires a missed interval the moment the machine
/// wakes, which would land the heaviest IO job on the system exactly as the
/// lid opens. With a calendar schedule a missed slot is coalesced into one
/// catch-up run instead of a backlog.
pub const REFRESH_HOURS: [u32; 4] = [0, 6, 12, 18];

pub fn log_path() -> PathBuf {
    home().join("Library/Logs/wsctl-disk-report.log")
}

pub fn is_installed() -> bool {
    bundle::has_agent(LABEL)
}

/// Whether launchd has the job loaded. The job is one-shot, so this reports
/// that the schedule is armed, not that a process is alive.
pub fn is_loaded() -> bool {
    let (Ok(domain), Some(label)) = (domain(), bundle::agent_label(LABEL)) else {
        return false;
    };
    Command::new("launchctl")
        .args(["print", &format!("{domain}/{label}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Put the job in the Workstation bundle next to a copy of `exe` and register
/// it, replacing any previous copy so this is safe to re-run after the binary
/// changes. The job runs the bundled copy, which is what gives it a name and
/// an icon in Login Items.
pub fn install(exe: &Path) -> Result<Installed> {
    bundle::retire_launch_agents(&LEGACY_LABELS);
    bundle::install_agent(exe, LABEL, &|label| plist_contents(label, &log_path()))
}

pub fn uninstall() -> Result<()> {
    bundle::retire_launch_agents(&LEGACY_LABELS);
    bundle::remove_agent(LABEL)
}

/// `label` is the build-tagged label the bundle assigns; see `wsctl_core::bundle`.
pub fn plist_contents(label: &str, log: &Path) -> String {
    let log = xml_escape(&log.to_string_lossy());
    let calendar: String = REFRESH_HOURS
        .iter()
        .map(|h| {
            format!(
                "        <dict>\n            <key>Hour</key>\n            <integer>{h}</integer>\n            <key>Minute</key>\n            <integer>0</integer>\n        </dict>\n"
            )
        })
        .collect();

    // `disk agent refresh` rather than `disk audit --no-cache`. The audit
    // opens everything, and a logged-in user's tccd will put a dialog on the
    // screen for the directories macOS gates, four times a day, with the scan
    // hung behind it until somebody clicks. Neither the background domain nor
    // a session-type limit stops that while the user is logged in, which is
    // exactly when the refresh runs; the only thing that does is not opening
    // those directories. The refresh leaves them closed and the cache says so.
    //
    // RunAtLoad is false on purpose. The scan takes minutes, and running it at
    // every login is exactly the cost this cache exists to avoid.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>BundleProgram</key>
    <string>Contents/MacOS/{name}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{name}</string>
        <string>disk</string>
        <string>agent</string>
        <string>refresh</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>StartCalendarInterval</key>
    <array>
{calendar}    </array>
    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>Nice</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        name = bundle::NAME
    )
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The GUI session domain, where a logged-in user's agents live.
fn domain() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine current uid")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        bail!("could not determine current uid");
    }
    Ok(format!("gui/{uid}"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents() -> String {
        plist_contents(
            "dev.pj.workstation.disk-report.1",
            Path::new("/Users/pj/Library/Logs/wsctl-disk-report.log"),
        )
    }

    #[test]
    fn plist_runs_the_unattended_refresh_from_the_bundled_copy() {
        let plist = contents();
        assert!(
            plist.contains(
                "<key>BundleProgram</key>\n    <string>Contents/MacOS/Workstation</string>"
            )
        );
        assert!(plist.contains("<string>disk</string>"));
        assert!(plist.contains("<string>agent</string>"));
        assert!(plist.contains("<string>refresh</string>"));
        assert!(
            !plist.contains("<string>audit</string>") && !plist.contains("--no-cache"),
            "the audit opens every directory, and from launchd that is a \
             permission dialog per protected folder, four times a day"
        );
    }

    #[test]
    fn a_logged_in_agent_lives_in_the_gui_domain() {
        // A background-domain or session-type agent either does not run while
        // the user is logged in, or still prompts when it does. Neither helps,
        // so the job lives in the ordinary GUI domain and the closed list is
        // what keeps it from prompting.
        let domain = domain().unwrap();
        assert!(domain.starts_with("gui/"), "{domain}");
    }

    #[test]
    fn plist_does_not_scan_at_login() {
        let plist = contents();
        assert!(
            plist.contains("<key>RunAtLoad</key>\n    <false/>"),
            "a minutes-long scan must not run on every login"
        );
    }

    #[test]
    fn plist_uses_a_calendar_schedule_not_an_interval() {
        let plist = contents();
        assert!(plist.contains("<key>StartCalendarInterval</key>"));
        assert!(
            !plist.contains("<key>StartInterval</key>"),
            "StartInterval fires missed runs on wake, which is what we are avoiding"
        );
        for hour in REFRESH_HOURS {
            assert!(
                plist.contains(&format!("<integer>{hour}</integer>")),
                "missing hour {hour}"
            );
        }
    }

    #[test]
    fn plist_yields_to_foreground_work() {
        let plist = contents();
        assert!(plist.contains("<key>LowPriorityIO</key>\n    <true/>"));
        assert!(plist.contains("<key>Nice</key>"));
        assert!(plist.contains("<string>Background</string>"));
    }

    #[test]
    fn plist_is_well_formed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.plist");
        std::fs::write(&path, contents()).unwrap();

        let out = Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .expect("plutil should exist on macOS");
        assert!(
            out.status.success(),
            "plutil rejected the plist: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn paths_land_where_launchd_expects() {
        assert!(log_path().ends_with("Library/Logs/wsctl-disk-report.log"));
    }

    #[test]
    fn plist_carries_the_tagged_label_it_is_given() {
        assert!(contents().contains("<string>dev.pj.workstation.disk-report.1</string>"));
    }

    #[test]
    fn label_lives_under_the_bundle_id_and_the_old_ones_are_retired() {
        assert!(LABEL.starts_with(bundle::BUNDLE_ID), "{LABEL}");
        assert_ne!(LABEL, "dev.pj.workstation.display");
        assert!(
            LEGACY_LABELS.contains(&LABEL),
            "the label first shipped in ~/Library/LaunchAgents"
        );
        assert!(LEGACY_LABELS.contains(&"com.priyanshujain.wsctl.disk-report"));
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        let plist = plist_contents(LABEL, Path::new("/tmp/a&b<c>/log"));
        assert!(plist.contains("/tmp/a&amp;b&lt;c&gt;/log"));
        assert!(!plist.contains("a&b<c>"));
    }
}
