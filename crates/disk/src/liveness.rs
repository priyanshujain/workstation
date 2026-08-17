use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Why a path must not be deleted right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    /// A process is sitting in this directory.
    Cwd { pid: i32, name: String },
    /// A process named this path on its command line.
    Argv { pid: i32, name: String },
}

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Busy::Cwd { pid, name } => write!(f, "{name} (pid {pid}) is working in it"),
            Busy::Argv { pid, name } => write!(f, "{name} (pid {pid}) has it open"),
        }
    }
}

/// A snapshot of what every running process is touching.
///
/// Built once and reused across many `check` calls, because shelling out per
/// candidate path is far too slow when scanning hundreds of project dirs.
pub struct Liveness {
    cwds: Vec<(i32, String, PathBuf)>,
    argv: Vec<(i32, String, String)>,
}

impl Liveness {
    pub fn snapshot() -> Self {
        Self {
            cwds: process_cwds(),
            argv: process_argv(),
        }
    }

    /// Empty snapshot, for tests and for callers that opt out of the check.
    pub fn empty() -> Self {
        Self {
            cwds: Vec::new(),
            argv: Vec::new(),
        }
    }

    /// Every reason `path` is in use. Empty means safe to remove.
    ///
    /// A process counts as busy when its working directory is at or below
    /// `path`, or when `path` appears as a substring of its command line. The
    /// second test is deliberately loose: a build writing into a target dir
    /// usually names an ancestor of it, not the dir itself.
    pub fn check(&self, path: &Path) -> Vec<Busy> {
        let mut out = Vec::new();
        let mut seen: HashSet<i32> = HashSet::new();

        for (pid, name, cwd) in &self.cwds {
            if cwd.starts_with(path) && seen.insert(*pid) {
                out.push(Busy::Cwd {
                    pid: *pid,
                    name: name.clone(),
                });
            }
        }

        let needle = path.to_string_lossy();
        for (pid, name, cmd) in &self.argv {
            if cmd.contains(needle.as_ref()) && seen.insert(*pid) {
                out.push(Busy::Argv {
                    pid: *pid,
                    name: name.clone(),
                });
            }
        }

        out
    }

    pub fn is_busy(&self, path: &Path) -> bool {
        !self.check(path).is_empty()
    }
}

fn process_cwds() -> Vec<(i32, String, PathBuf)> {
    // -F emits a machine-readable record stream: p<pid>, c<command>, n<name>.
    let Ok(out) = Command::new("lsof")
        .args(["-n", "-P", "-d", "cwd", "-Fpcn"])
        .output()
    else {
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut pid = 0i32;
    let mut name = String::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let (tag, rest) = line.split_at(1);
        match tag {
            "p" => pid = rest.parse().unwrap_or(0),
            "c" => name = rest.to_string(),
            "n" if pid != 0 && rest.starts_with('/') => {
                result.push((pid, name.clone(), PathBuf::from(rest)));
            }
            _ => {}
        }
    }
    result
}

fn process_argv() -> Vec<(i32, String, String)> {
    // -ww disables the width truncation ps otherwise applies to args, which
    // would silently drop the tail of long build command lines. `comm` is
    // deliberately not requested: macOS truncates that column to 16 chars,
    // so "/Users/pj/Workspace/.../wsctl" comes back as "/Users/pj/Worksp".
    let Ok(out) = Command::new("ps")
        .args(["-eww", "-o", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };

    let self_pid = std::process::id() as i32;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .filter(|(pid, _, _)| *pid != self_pid)
        .collect()
}

/// Split one `ps -o pid=,args=` row into pid, display name, and full argv.
/// The name is the basename of argv[0], which is never truncated.
fn parse_ps_line(line: &str) -> Option<(i32, String, String)> {
    let line = line.trim_start();
    let (pid_str, args) = line.split_once(char::is_whitespace)?;
    let pid = pid_str.parse::<i32>().ok()?;
    let args = args.trim_start();
    if args.is_empty() {
        return None;
    }

    let argv0 = args.split_whitespace().next().unwrap_or(args);
    let name = Path::new(argv0)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| argv0.to_string());

    Some((pid, name, args.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Stdio};
    use tempfile::tempdir;

    struct Reaped(Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Keep a process alive holding `args`, without relying on any one binary
    /// accepting them: `sh -c` ignores the extras beyond the script.
    fn spawn_holding(cwd: Option<&Path>, args: &[&Path]) -> Reaped {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30; true");
        for a in args {
            cmd.arg(a);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        Reaped(
            cmd.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// Retry the snapshot until `pid` shows up: spawn returns before exec has
    /// necessarily happened, so a single sample can race.
    fn snapshot_seeing(pid: i32) -> Liveness {
        for _ in 0..40 {
            let live = Liveness::snapshot();
            let known = live.cwds.iter().any(|(p, _, _)| *p == pid)
                || live.argv.iter().any(|(p, _, _)| *p == pid);
            if known {
                return live;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Liveness::snapshot()
    }

    #[test]
    fn empty_snapshot_reports_nothing_busy() {
        let dir = tempdir().unwrap();
        let live = Liveness::empty();
        assert!(!live.is_busy(dir.path()));
        assert!(live.check(dir.path()).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn detects_a_process_sitting_in_the_directory() {
        let dir = tempdir().unwrap();
        // tempdir may hand back a symlinked path (/var -> /private/var on
        // macOS); lsof reports the resolved one, so compare against that.
        let real = dir.path().canonicalize().unwrap();

        let child = spawn_holding(Some(&real), &[]);
        let pid = child.0.id() as i32;

        let live = snapshot_seeing(pid);
        let reasons = live.check(&real);

        assert!(
            reasons
                .iter()
                .any(|r| matches!(r, Busy::Cwd { pid: p, .. } if *p == pid)),
            "expected pid {pid} to be reported busy in {}, got {reasons:?}",
            real.display()
        );
        assert!(live.is_busy(&real));
    }

    #[test]
    #[cfg(unix)]
    fn detects_a_path_named_on_a_command_line() {
        let dir = tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let marker = real.join("build-output");

        let child = spawn_holding(None, &[&marker]);
        let pid = child.0.id() as i32;

        let live = snapshot_seeing(pid);
        let reasons = live.check(&marker);

        assert!(
            reasons
                .iter()
                .any(|r| matches!(r, Busy::Argv { pid: p, .. } if *p == pid)),
            "expected pid {pid} to be reported busy for {}, got {reasons:?}",
            marker.display()
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_parent_directory_counts_as_busy() {
        // The whole point: deleting the parent would take the live child with it.
        let dir = tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let nested = real.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let child = spawn_holding(Some(&nested), &[]);
        let pid = child.0.id() as i32;

        let live = snapshot_seeing(pid);
        assert!(
            live.check(&real)
                .iter()
                .any(|r| matches!(r, Busy::Cwd { pid: p, .. } if *p == pid)),
            "deleting {} would hit a process running in {}",
            real.display(),
            nested.display()
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_unrelated_directory_is_not_busy() {
        let busy_dir = tempdir().unwrap();
        let quiet_dir = tempdir().unwrap();
        let busy_real = busy_dir.path().canonicalize().unwrap();
        let quiet_real = quiet_dir.path().canonicalize().unwrap();

        let child = spawn_holding(Some(&busy_real), &[]);

        let live = snapshot_seeing(child.0.id() as i32);
        assert!(live.is_busy(&busy_real), "busy dir should be flagged");
        assert!(!live.is_busy(&quiet_real), "quiet dir wrongly flagged busy");
    }

    #[test]
    fn ps_line_name_comes_from_argv0_not_a_truncated_column() {
        let line =
            "91483 /Users/pj/Workspace/projects/rust/workstation/target/debug/wsctl disk projects";
        let (pid, name, args) = parse_ps_line(line).unwrap();
        assert_eq!(pid, 91483);
        assert_eq!(name, "wsctl");
        assert!(args.ends_with("disk projects"));
    }

    #[test]
    fn ps_line_keeps_the_whole_command_line() {
        let line = "  47797 go run ./cmd/campaign --output /tmp/run/seed-101";
        let (pid, name, args) = parse_ps_line(line).unwrap();
        assert_eq!(pid, 47797);
        assert_eq!(name, "go");
        assert!(
            args.contains("/tmp/run/seed-101"),
            "argv tail was dropped: {args}"
        );
    }

    #[test]
    fn ps_line_rejects_junk() {
        assert!(parse_ps_line("").is_none());
        assert!(parse_ps_line("PID ARGS").is_none());
        assert!(parse_ps_line("1234").is_none());
        assert!(parse_ps_line("1234   ").is_none());
    }

    #[test]
    fn sibling_prefix_does_not_count_as_a_child() {
        // "/tmp/target" must not match "/tmp/target-old".
        let live = Liveness {
            cwds: vec![(1, "sh".into(), PathBuf::from("/tmp/target-old"))],
            argv: Vec::new(),
        };
        assert!(!live.is_busy(Path::new("/tmp/target")));
    }
}
