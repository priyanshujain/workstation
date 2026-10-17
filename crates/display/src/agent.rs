use std::path::PathBuf;
use std::process::Command;

pub const LABEL: &str = "org.nix-community.home.wsctl-display";

/// The WindowServer rewrites this file whenever the display layout changes, so launchd
/// watching it gives an event-driven trigger with no resident process.
///
/// This is an implementation detail rather than documented API. The supported alternative,
/// `CGDisplayRegisterReconfigurationCallback`, does not deliver to a plain binary even after
/// a successful `NSApplicationLoad`, and would mean a resident process.
pub const WATCHED_PATH: &str = "/Library/Preferences/com.apple.windowserver.displays.plist";

pub fn log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Logs/wsctl-display.log")
}

pub fn is_loaded() -> bool {
    let Ok(out) = Command::new("id").arg("-u").output() else {
        return false;
    };
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    !uid.is_empty()
        && Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/{LABEL}")])
            .output()
            .is_ok_and(|o| o.status.success())
}
