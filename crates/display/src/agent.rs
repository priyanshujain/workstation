use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use wsctl_core::bundle::{self, Installed};

/// launchd label for the job that keeps the menu bar pinned, and the name of its plist
/// inside the Workstation bundle. It lives under the bundle id on purpose: Login Items keys
/// its record by label and keeps the first name it computed for it, so the old `wsctl` name
/// could only be shed by moving to a label macOS had never seen.
pub const LABEL: &str = "dev.pj.workstation.display";

/// Labels earlier versions loaded from `~/Library/LaunchAgents`. The current one is among
/// them because it first shipped that way too.
const LEGACY_LABELS: [&str; 3] = [
    LABEL,
    "com.priyanshujain.workstation.display",
    "com.priyanshujain.wsctl.display",
];

/// The WindowServer rewrites this file whenever the display layout changes, so launchd
/// watching it gives an event-driven trigger with no resident process.
///
/// This is an implementation detail rather than documented API. The supported alternative,
/// `CGDisplayRegisterReconfigurationCallback`, does not deliver to a plain binary even after
/// a successful `NSApplicationLoad`, and would mean a resident process. See docs/displays.md.
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

pub fn log_path() -> PathBuf {
    home().join("Library/Logs/wsctl-display.log")
}

pub fn is_installed() -> bool {
    bundle::has_agent(LABEL)
}

/// Whether launchd currently has the job loaded. The job is one-shot, so this reports that
/// the trigger is armed, not that a process is alive.
pub fn is_loaded() -> bool {
    let (Ok(domain), Some(label)) = (gui_domain(), bundle::agent_label(LABEL)) else {
        return false;
    };
    Command::new("launchctl")
        .args(["print", &format!("{domain}/{label}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Put the job in the Workstation bundle next to a copy of `exe` and register it, replacing
/// any previous copy so this is safe to re-run after the binary changes. The job runs the
/// bundled copy, which is what gives it a name and an icon in Login Items.
pub fn install(exe: &Path) -> Result<Installed> {
    bundle::retire_launch_agents(&LEGACY_LABELS);
    bundle::install_agent(exe, LABEL, &|label| plist_contents(label, &log_path()))
}

/// Unregister the job and drop it from the bundle.
pub fn uninstall() -> Result<()> {
    bundle::retire_launch_agents(&LEGACY_LABELS);
    bundle::remove_agent(LABEL)
}

/// `label` is the build-tagged label the bundle assigns; see `wsctl_core::bundle`.
pub fn plist_contents(label: &str, log: &Path) -> String {
    let log = xml_escape(&log.to_string_lossy());
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
"#,
        name = bundle::NAME
    )
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

    fn contents() -> String {
        plist_contents("dev.pj.workstation.display.1", Path::new("/tmp/w.log"))
    }

    #[test]
    fn plist_runs_the_one_shot_apply_from_the_bundled_copy() {
        let plist = contents();
        assert!(
            plist.contains(
                "<key>BundleProgram</key>\n    <string>Contents/MacOS/Workstation</string>"
            )
        );
        assert!(plist.contains("<string>display</string>"));
        assert!(plist.contains("<string>apply</string>"));
        assert!(plist.contains("<string>dev.pj.workstation.display.1</string>"));
    }

    #[test]
    fn plist_raises_verbosity_so_the_log_is_not_empty() {
        // Without this the job runs at `warn` and a working trigger writes nothing.
        assert!(contents().contains("<string>-v</string>"));
    }

    #[test]
    fn plist_is_triggered_by_the_windowserver_layout_file() {
        let plist = contents();
        assert!(plist.contains("<key>WatchPaths</key>"));
        assert!(plist.contains(WATCHED_PATH));
    }

    #[test]
    fn plist_also_polls_on_a_timer() {
        // WatchPaths alone misses session-only layout changes, which never write the file.
        let plist = contents();
        assert!(plist.contains("<key>StartInterval</key>"));
        assert!(plist.contains(&format!("<integer>{POLL_SECONDS}</integer>")));
    }

    #[test]
    fn plist_runs_at_login_but_does_not_stay_resident() {
        let plist = contents();
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        // KeepAlive would restart the one-shot job forever.
        assert!(
            !plist.contains("KeepAlive"),
            "a WatchPaths job must not be kept alive"
        );
    }

    #[test]
    fn plist_escapes_special_characters_in_paths() {
        let plist = plist_contents(LABEL, Path::new("/tmp/a&b<c>/w.log"));
        assert!(plist.contains("/tmp/a&amp;b&lt;c&gt;/w.log"));
        assert!(!plist.contains("a&b"));
    }

    #[test]
    fn plist_uses_absolute_log_path() {
        // launchd does not expand `~`, so a relative path would silently drop logs.
        let plist = plist_contents(LABEL, &log_path());
        assert!(!plist.contains("<string>~"));
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
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn paths_land_in_the_expected_locations() {
        assert!(log_path().ends_with("Library/Logs/wsctl-display.log"));
    }

    #[test]
    fn label_lives_under_the_bundle_id_and_the_old_ones_are_retired() {
        assert!(LABEL.starts_with(bundle::BUNDLE_ID), "{LABEL}");
        assert!(
            LEGACY_LABELS.contains(&LABEL),
            "the label first shipped in ~/Library/LaunchAgents"
        );
        assert!(LEGACY_LABELS.contains(&"com.priyanshujain.wsctl.display"));
    }

    #[test]
    fn escape_leaves_ordinary_paths_untouched() {
        assert_eq!(
            xml_escape("/Users/pj/.local/bin/wsctl"),
            "/Users/pj/.local/bin/wsctl"
        );
    }
}
