use std::process::Command;

use anyhow::{Result, anyhow, bail};

/// Type `text` into whatever field is focused on the device.
///
/// These are synthetic keystrokes, not a clipboard write. Android blocks setting the clipboard
/// from the shell, so no app-free path exists to make a real paste happen from the Mac.
/// Typing is the closest adb can get, and for a phone number or a code it is arguably better
/// since it skips the paste step entirely.
pub fn type_text(serial: &str, text: &str) -> Result<()> {
    if text.is_empty() {
        bail!("nothing to type, the clipboard is empty");
    }
    // `input text` maps characters through a US key character map, so anything outside ASCII
    // is silently dropped or mangled. A wrong phone number is worse than a refusal.
    if !text.is_ascii() {
        bail!(
            "`input text` only handles ASCII and this has other characters in it; \
             use `wsctl android mirror` and Cmd+V instead"
        );
    }

    let status = Command::new("adb")
        .args(["-s", serial, "shell", "input", "text", &device_quote(text)])
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow!("adb is not installed, run `brew install android-platform-tools`")
            }
            _ => anyhow!(e).context("failed to run adb"),
        })?;

    if !status.success() {
        bail!("adb input failed, is a text field focused on the device?");
    }
    Ok(())
}

/// Quote for the device's shell.
///
/// `adb shell` joins its arguments into one string and hands it to sh on the device, so an
/// unquoted space would split the text into separate arguments and `$`, backticks or `;`
/// would be interpreted there rather than typed.
fn device_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_plain_text() {
        assert_eq!(device_quote("hello"), "'hello'");
    }

    #[test]
    fn keeps_spaces_in_one_argument() {
        // Unquoted, sh would split this and `input text` would only receive "hello".
        assert_eq!(device_quote("hello world"), "'hello world'");
    }

    #[test]
    fn escapes_embedded_single_quotes() {
        assert_eq!(device_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn neutralises_shell_metacharacters() {
        for raw in ["a;rm -rf b", "$(whoami)", "`id`", "a&&b", "a|b", "a>b"] {
            let quoted = device_quote(raw);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{quoted}"
            );
            assert_eq!(&quoted[1..quoted.len() - 1], raw);
        }
    }

    #[test]
    fn a_quote_injection_attempt_stays_inside_the_quotes() {
        // Naive quoting would let this break out and run `id` on the device.
        let quoted = device_quote("'; id; '");
        assert_eq!(quoted, r"''\''; id; '\'''");
    }

    #[test]
    fn typical_pastes_survive_intact() {
        assert_eq!(device_quote("+91 98765 43210"), "'+91 98765 43210'");
        assert_eq!(
            device_quote("https://example.com/a?b=1&c=2"),
            "'https://example.com/a?b=1&c=2'"
        );
    }
}
