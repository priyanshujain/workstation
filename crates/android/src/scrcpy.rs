use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// What a scrcpy session is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Clipboard bridge only. scrcpy pushes the device clipboard to the computer whenever it
    /// changes, which is the one way to read it at all: Android 10 closed clipboard reads to
    /// background apps, and scrcpy's agent runs as the shell user, which is exempt.
    Clip,
    /// Full mirror. A superset of [`Mode::Clip`], since it syncs the clipboard the same way
    /// and adds the screen plus Ctrl+V for the computer-to-device direction.
    Mirror,
}

impl Mode {
    pub fn args(&self, serial: &str) -> Vec<String> {
        let mut args = vec!["-s".to_string(), serial.to_string()];
        if *self == Mode::Clip {
            args.extend(["--no-window", "--no-video", "--no-audio"].map(String::from));
        }
        args
    }
}

/// Start scrcpy in the foreground and block until it exits.
///
/// Any session already driving this device is stopped first. The two modes are exclusive:
/// mirror already does everything clip does, and running both would leave two agents racing
/// to set the computer clipboard.
pub fn run(mode: Mode, serial: &str) -> Result<()> {
    let stopped = stop_sessions(serial)?;
    if stopped > 0 {
        tracing::info!("stopped {stopped} existing scrcpy session(s) for {serial}");
    }

    let status = Command::new("scrcpy")
        .args(mode.args(serial))
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow!("scrcpy is not installed, run `brew install scrcpy`")
            }
            _ => anyhow!(e).context("failed to start scrcpy"),
        })?;

    match status.code() {
        // A signal exit is Ctrl-C, which is how you are meant to stop this.
        Some(0) | None => Ok(()),
        Some(code) => bail!("scrcpy exited with status {code}"),
    }
}

/// Stop every scrcpy session driving `serial`, returning how many were killed.
pub fn stop_sessions(serial: &str) -> Result<usize> {
    let output = Command::new("ps")
        .args(["-Ao", "pid=,args="])
        .output()
        .context("failed to list running processes")?;

    let pids = sessions_for(&String::from_utf8_lossy(&output.stdout), serial);
    for pid in &pids {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    Ok(pids.len())
}

/// Process ids of scrcpy sessions targeting `serial`, from `ps -Ao pid=,args=` output.
///
/// Matching on the first argument rather than the whole line matters: a shell that launched
/// scrcpy carries the same text, and killing that instead would leave the real session alive.
pub fn sessions_for(ps_output: &str, serial: &str) -> Vec<u32> {
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, args) = line.trim_start().split_once(char::is_whitespace)?;
            let binary = args.split_whitespace().next()?;
            if !is_scrcpy(binary) || !targets(args, serial) {
                return None;
            }
            pid.parse().ok()
        })
        .collect()
}

fn is_scrcpy(binary: &str) -> bool {
    binary.rsplit('/').next() == Some("scrcpy")
}

fn targets(args: &str, serial: &str) -> bool {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    tokens
        .windows(2)
        .any(|pair| matches!(pair[0], "-s" | "--serial") && pair[1] == serial)
        || tokens
            .iter()
            .any(|token| token.strip_prefix("--serial=") == Some(serial))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ps -Ao pid=,args=` shape, captured live: the session, the adb child it spawns,
    /// the shell that launched it, and unrelated noise.
    const PS: &str = "\
  34435 scrcpy -s 663c91b1 --no-window --no-video --no-audio
  34446 adb -s 663c91b1 shell CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / com.genymobile.scrcpy.Server 4.0 scid=66773e6e
  34240 /bin/zsh -c cd /tmp && scrcpy -s 663c91b1 --no-window
  96986 /opt/homebrew/bin/scrcpy -s 192.168.1.243:5555
   1234 /usr/bin/ssh -s 663c91b1
    501 /sbin/launchd";

    #[test]
    fn clip_mode_runs_without_a_window() {
        let args = Mode::Clip.args("663c91b1");
        assert_eq!(
            args,
            ["-s", "663c91b1", "--no-window", "--no-video", "--no-audio"]
        );
    }

    #[test]
    fn mirror_mode_keeps_the_screen() {
        let args = Mode::Mirror.args("663c91b1");
        assert_eq!(args, ["-s", "663c91b1"]);
        assert!(!args.iter().any(|a| a.starts_with("--no-")));
    }

    #[test]
    fn finds_the_session_for_a_device() {
        assert_eq!(sessions_for(PS, "663c91b1"), vec![34435]);
    }

    #[test]
    fn ignores_the_shell_that_launched_scrcpy() {
        // The wrapper carries the same text; killing it would leave the session running.
        assert!(!sessions_for(PS, "663c91b1").contains(&34240));
    }

    #[test]
    fn matches_an_absolute_scrcpy_path() {
        assert_eq!(sessions_for(PS, "192.168.1.243:5555"), vec![96986]);
    }

    #[test]
    fn ignores_the_adb_child_scrcpy_spawns() {
        // scrcpy's own adb helper carries the same `-s <serial>`. Killing it would tear down
        // the session's transport while leaving scrcpy itself running.
        assert!(!sessions_for(PS, "663c91b1").contains(&34446));
    }

    #[test]
    fn ignores_other_binaries_that_take_dash_s() {
        // ssh -s would otherwise look like a match on the serial alone.
        assert!(!sessions_for(PS, "663c91b1").contains(&1234));
    }

    #[test]
    fn ignores_sessions_for_other_devices() {
        assert!(sessions_for(PS, "emulator-5554").is_empty());
    }

    #[test]
    fn handles_the_long_serial_flag() {
        let ps = "  42 scrcpy --serial 663c91b1\n  43 scrcpy --serial=663c91b1";
        assert_eq!(sessions_for(ps, "663c91b1"), vec![42, 43]);
    }

    #[test]
    fn survives_empty_and_malformed_input() {
        assert!(sessions_for("", "663c91b1").is_empty());
        assert!(sessions_for("garbage\n\n  \n", "663c91b1").is_empty());
    }

    #[test]
    fn does_not_match_a_serial_that_is_only_a_prefix() {
        let ps = "  42 scrcpy -s 663c91b1aa";
        assert!(sessions_for(ps, "663c91b1").is_empty());
    }
}
