use std::process::Command;

use anyhow::{Context, Result, bail};

/// Mission Control's "Displays have separate Spaces", as macOS stores it.
pub const DOMAIN: &str = "com.apple.spaces";
pub const KEY: &str = "spans-displays";

/// Whether every panel keeps its own set of Spaces.
///
/// The stored key is the inverse of the checkbox. `spans-displays = 1` stretches one Space
/// across all panels, which is what makes a three-finger swipe switch every screen at once
/// instead of the one under the cursor, and leaves the menu bar on the main panel only. An
/// unset key means separate Spaces, the macOS default.
pub fn separate_spaces_enabled() -> Result<bool> {
    read(DOMAIN)
}

/// Turn separate Spaces on or off. The WindowServer reads this key at login and never again,
/// so the change only shows up after the next one.
pub fn set_separate_spaces(enabled: bool) -> Result<()> {
    write(DOMAIN, enabled)
}

fn read(domain: &str) -> Result<bool> {
    let out = Command::new("defaults")
        .args(["read", domain, KEY])
        .output()
        .context("failed to run `defaults read`")?;

    // A missing key or domain exits non-zero, and absent is the macOS default: separate.
    if !out.status.success() {
        return Ok(true);
    }

    Ok(!spans(&String::from_utf8_lossy(&out.stdout)))
}

fn write(domain: &str, enabled: bool) -> Result<()> {
    let value = if enabled { "false" } else { "true" };
    let out = Command::new("defaults")
        .args(["write", domain, KEY, "-bool", value])
        .output()
        .context("failed to run `defaults write`")?;

    if !out.status.success() {
        bail!(
            "`defaults write {domain} {KEY} -bool {value}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}

/// `defaults` prints a boolean as 0 or 1, but a plist written by hand can hold the word form.
fn spans(raw: &str) -> bool {
    matches!(raw.trim(), "1" | "true" | "YES")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_every_spelling_of_a_set_key() {
        for raw in ["1", "true", "YES", " 1\n"] {
            assert!(spans(raw), "{raw:?} should read as spanning");
        }
    }

    #[test]
    fn anything_else_means_separate_spaces() {
        for raw in ["0", "false", "NO", "", "\n"] {
            assert!(!spans(raw), "{raw:?} should not read as spanning");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn round_trips_through_defaults() {
        // A scratch domain, so the test never touches the real Mission Control setting.
        let domain = "com.priyanshujain.wsctl.spaces-test";

        write(domain, false).expect("writing the key should succeed");
        assert!(!read(domain).unwrap(), "spans-displays=1 is not separate");

        write(domain, true).expect("writing the key should succeed");
        assert!(read(domain).unwrap(), "spans-displays=0 is separate");

        Command::new("defaults")
            .args(["delete", domain])
            .output()
            .expect("cleaning up the scratch domain should succeed");

        // A domain that does not exist reads as the macOS default.
        assert!(read(domain).unwrap(), "an absent key means separate Spaces");
    }
}
