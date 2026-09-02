use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// launchd label for the job that refreshes the cached disk report.
pub const LABEL: &str = "com.priyanshujain.wsctl.disk-report";

/// Hours at which the refresh runs. Fixed clock times rather than
/// `StartInterval`: launchd fires a missed interval the moment the machine
/// wakes, which would land the heaviest IO job on the system exactly as the
/// lid opens. With a calendar schedule a missed slot is coalesced into one
/// catch-up run instead of a backlog.
pub const REFRESH_HOURS: [u32; 4] = [0, 6, 12, 18];

pub fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

pub fn log_path() -> PathBuf {
    home().join("Library/Logs/wsctl-disk-report.log")
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

/// Whether launchd has the job loaded. The job is one-shot, so this reports
/// that the schedule is armed, not that a process is alive.
pub fn is_loaded() -> bool {
    let Ok(domain) = domain() else {
        return false;
    };
    Command::new("launchctl")
        .args(["print", &format!("{domain}/{LABEL}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Write the plist and load it, replacing any previously loaded copy so this
/// is safe to re-run after the binary moves.
pub fn install(exe: &Path) -> Result<PathBuf> {
    let path = plist_path();
    let parent = path
        .parent()
        .context("could not determine LaunchAgents directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    std::fs::write(&path, plist_contents(exe, &log_path()))
        .with_context(|| format!("failed to write {}", path.display()))?;

    let domain = domain()?;
    bootout();

    let out = Command::new("launchctl")
        .args(["bootstrap", &domain, &path.to_string_lossy()])
        .output()
        .context("failed to run launchctl bootstrap")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("launchctl bootstrap failed: {}", stderr.trim());
    }

    Ok(path)
}

pub fn uninstall() -> Result<()> {
    bootout();

    let path = plist_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

pub fn plist_contents(exe: &Path, log: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
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
    // opens everything, and from a process no terminal owns each directory
    // macOS asks about first is a dialog on the screen, four times a day,
    // with the scan hung behind it until somebody clicks. The refresh leaves
    // the well-known ones closed and the cache says so.
    //
    // The session type is the guarantee behind that. Apple's list of asked-
    // about paths is long, undocumented and includes caches, so a list alone
    // is always one macOS release behind. A process in the background
    // session has no graphics access and cannot be asked anything: macOS
    // denies instead, quietly, and the directory reads as protected.
    //
    // RunAtLoad is false on purpose. The scan takes minutes, and running it at
    // every login is exactly the cost this cache exists to avoid.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>disk</string>
        <string>agent</string>
        <string>refresh</string>
    </array>
    <key>LimitLoadToSessionType</key>
    <string>Background</string>
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
"#
    )
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The per-user background domain, not the GUI one: see [`plist_contents`].
fn domain() -> Result<String> {
    Ok(format!("user/{}", uid()?))
}

fn uid() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine current uid")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        bail!("could not determine current uid");
    }
    Ok(uid)
}

/// Unload the job wherever an earlier version put it. Before the background
/// session it lived in the GUI domain, and a copy left there would go on
/// raising dialogs next to the new one.
fn bootout() {
    let Ok(uid) = uid() else {
        return;
    };
    for domain in [format!("user/{uid}"), format!("gui/{uid}")] {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LABEL}")])
            .output();
    }
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
            Path::new("/Users/pj/.cargo/bin/wsctl"),
            Path::new("/Users/pj/Library/Logs/wsctl-disk-report.log"),
        )
    }

    #[test]
    fn plist_runs_the_unattended_refresh() {
        let plist = contents();
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
    fn plist_keeps_the_job_out_of_the_gui_session() {
        // Without this, launchd loads the plist into the Aqua session at the
        // next login regardless of where `enable` bootstrapped it, and a
        // process there can be asked about directories, so it is.
        let plist = contents();
        assert!(
            plist.contains("<key>LimitLoadToSessionType</key>\n    <string>Background</string>"),
            "the job must live where macOS cannot put a dialog in front of it"
        );
    }

    #[test]
    fn the_job_is_bootstrapped_into_the_background_domain() {
        let domain = domain().unwrap();
        assert!(domain.starts_with("user/"), "{domain}");
        assert!(!domain.starts_with("gui/"), "{domain}");
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
        assert!(plist_path().ends_with(format!("Library/LaunchAgents/{LABEL}.plist")));
        assert!(log_path().ends_with("Library/Logs/wsctl-disk-report.log"));
    }

    #[test]
    fn label_does_not_collide_with_the_display_agent() {
        assert_ne!(LABEL, "com.priyanshujain.wsctl.display");
    }

    #[test]
    fn xml_special_characters_are_escaped() {
        let plist = plist_contents(Path::new("/tmp/a&b<c>"), Path::new("/tmp/log"));
        assert!(plist.contains("/tmp/a&amp;b&lt;c&gt;"));
        assert!(!plist.contains("a&b<c>"));
    }
}
