use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use wsctl_core::bundle;

/// launchd label for the job that keeps the menu bar pinned. It lives under the bundle id
/// on purpose: Login Items keys its record by label and keeps the name it computed the first
/// time it saw that label, even after the plist is deleted and written again, so the old
/// `wsctl` name could only be shed by moving to a label macOS had never seen.
pub const LABEL: &str = "com.priyanshujain.workstation.display";
const LEGACY_LABEL: &str = "com.priyanshujain.wsctl.display";

/// The WindowServer rewrites this file whenever the display layout changes, so launchd
/// watching it gives an event-driven trigger with no resident process.
///
/// This is an implementation detail rather than documented API. The supported alternative,
/// `CGDisplayRegisterReconfigurationCallback`, does not deliver to a plain binary even after
/// a successful `NSApplicationLoad`, and would mean shipping an `.app` bundle instead of the
/// single CLI binary this repo installs. See docs/displays.md.
pub const WATCHED_PATH: &str = "/Library/Preferences/com.apple.windowserver.displays.plist";

/// launchd also re-runs the check on this interval, and that is what actually makes this
/// reliable. `WatchPaths` only fires for layout changes that get *persisted*. A change
/// completed with `kCGConfigurePermanently` writes `WATCHED_PATH` and trips the watch, but a
/// live session-only change moves the menu bar without touching that file, so the watch
/// never fires. Verified: a `.forSession` reconfiguration left the menu bar on the wrong
/// panel indefinitely with the job's run count unchanged.
///
/// Polling on a timer catches every case because it reads the real layout instead of
/// trusting a notification. One run costs about 10ms and no measurable CPU.
pub const POLL_SECONDS: u32 = 5;

pub fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn legacy_plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{LEGACY_LABEL}.plist"))
}

pub fn log_path() -> PathBuf {
    home().join("Library/Logs/wsctl-display.log")
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

/// Whether launchd currently has the job loaded. The job is one-shot, so this reports that
/// the trigger is armed, not that a process is alive.
pub fn is_loaded() -> bool {
    let Ok(domain) = gui_domain() else {
        return false;
    };
    Command::new("launchctl")
        .args(["print", &format!("{domain}/{LABEL}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Write the launchd plist and load it. Replaces any previously loaded copy so this is
/// safe to re-run after the binary moves. The job runs the copy of `exe` inside the
/// Workstation app bundle, which is what gives it a name and an icon in Login Items.
pub fn install(exe: &Path) -> Result<PathBuf> {
    let exe = bundle::install(exe)?;
    let path = plist_path();
    let parent = path
        .parent()
        .context("could not determine LaunchAgents directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    std::fs::write(&path, plist_contents(&exe, &log_path()))
        .with_context(|| format!("failed to write {}", path.display()))?;

    let domain = gui_domain()?;
    unload(&domain);

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

/// Unload the job and delete its plist.
pub fn uninstall() -> Result<()> {
    if let Ok(domain) = gui_domain() {
        unload(&domain);
    }

    let path = plist_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    bundle::remove_if_unused()
}

pub fn plist_contents(exe: &Path, log: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    let log = xml_escape(&log.to_string_lossy());
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
        <string>-v</string>
        <string>display</string>
        <string>apply</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>WatchPaths</key>
    <array>
        <string>{WATCHED_PATH}</string>
    </array>
    <key>StartInterval</key>
    <integer>{POLL_SECONDS}</integer>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Unload the job under both labels it has had, and drop the plist an older install wrote
/// under the previous one. Failure is ignored: on a first install nothing is loaded yet.
fn unload(domain: &str) {
    for label in [LABEL, LEGACY_LABEL] {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .output();
    }
    let _ = std::fs::remove_file(legacy_plist_path());
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn gui_domain() -> Result<String> {
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

    #[test]
    fn plist_runs_the_one_shot_apply() {
        let plist = plist_contents(
            Path::new("/Users/pj/.local/bin/wsctl"),
            Path::new("/Users/pj/Library/Logs/wsctl-display.log"),
        );
        assert!(plist.contains("<string>/Users/pj/.local/bin/wsctl</string>"));
        assert!(plist.contains("<string>display</string>"));
        assert!(plist.contains("<string>apply</string>"));
        assert!(plist.contains(LABEL));
    }

    #[test]
    fn plist_raises_verbosity_so_the_log_is_not_empty() {
        // Without this the job runs at `warn` and a working trigger writes nothing.
        let plist = plist_contents(Path::new("/bin/wsctl"), Path::new("/tmp/w.log"));
        assert!(plist.contains("<string>-v</string>"));
    }

    #[test]
    fn plist_is_triggered_by_the_windowserver_layout_file() {
        let plist = plist_contents(Path::new("/bin/wsctl"), Path::new("/tmp/w.log"));
        assert!(plist.contains("<key>WatchPaths</key>"));
        assert!(plist.contains(WATCHED_PATH));
    }

    #[test]
    fn plist_also_polls_on_a_timer() {
        // WatchPaths alone misses session-only layout changes, which never write the file.
        let plist = plist_contents(Path::new("/bin/wsctl"), Path::new("/tmp/w.log"));
        assert!(plist.contains("<key>StartInterval</key>"));
        assert!(plist.contains(&format!("<integer>{POLL_SECONDS}</integer>")));
    }

    #[test]
    fn plist_runs_at_login_but_does_not_stay_resident() {
        let plist = plist_contents(Path::new("/bin/wsctl"), Path::new("/tmp/w.log"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        // KeepAlive would restart the one-shot job forever.
        assert!(
            !plist.contains("KeepAlive"),
            "a WatchPaths job must not be kept alive"
        );
    }

    #[test]
    fn plist_escapes_special_characters_in_paths() {
        let plist = plist_contents(Path::new("/tmp/a&b<c>/wsctl"), Path::new("/tmp/w.log"));
        assert!(plist.contains("/tmp/a&amp;b&lt;c&gt;/wsctl"));
        assert!(!plist.contains("a&b"));
    }

    #[test]
    fn plist_uses_absolute_log_path() {
        // launchd does not expand `~`, so a relative path would silently drop logs.
        let plist = plist_contents(Path::new("/bin/wsctl"), &log_path());
        assert!(!plist.contains("<string>~"));
    }

    #[test]
    fn paths_land_in_the_expected_locations() {
        assert!(plist_path().ends_with(format!("Library/LaunchAgents/{LABEL}.plist")));
        assert!(log_path().ends_with("Library/Logs/wsctl-display.log"));
    }

    #[test]
    fn label_lives_under_the_bundle_id_and_retires_the_old_one() {
        // A label Login Items has never seen is the only way to drop the cached `wsctl` name.
        assert!(LABEL.starts_with(bundle::BUNDLE_ID), "{LABEL}");
        assert_ne!(LABEL, LEGACY_LABEL);
        assert!(
            legacy_plist_path().ends_with(format!("Library/LaunchAgents/{LEGACY_LABEL}.plist"))
        );
    }

    #[test]
    fn escape_leaves_ordinary_paths_untouched() {
        assert_eq!(
            xml_escape("/Users/pj/.local/bin/wsctl"),
            "/Users/pj/.local/bin/wsctl"
        );
    }
}
