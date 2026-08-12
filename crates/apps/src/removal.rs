//! Stop an app, then delete what the scan turned up.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::bundle::AppBundle;
use crate::leftovers::{Kind, Leftover};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Move to ~/.Trash, recoverable from Finder.
    Trash,
    /// Unlink for good.
    Purge,
}

#[derive(Debug, Default)]
pub struct Report {
    pub removed: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub freed: u64,
    /// Root-owned items are deleted outright: the Trash is per-user.
    pub forced_purge: Vec<String>,
}

/// macOS refuses to let an app touch another app's container unless the caller
/// holds Full Disk Access. Sudo does not substitute for it: this is the privacy
/// layer, not file permissions.
pub fn is_tcc_denial(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("operation not permitted") || e.contains("permission to access")
}

/// Whether this process holds Full Disk Access, probed by reading a directory
/// that is readable only with it.
pub fn has_full_disk_access() -> bool {
    dirs::home_dir()
        .map(|h| std::fs::read_dir(h.join("Library/Application Support/com.apple.TCC")).is_ok())
        .unwrap_or(false)
}

/// True if anything in `items` sits outside the user's control.
pub fn requires_root(items: &[Leftover]) -> bool {
    items.iter().any(|l| l.needs_root)
}

/// Ask for the sudo timestamp up front, so removal does not stall on a
/// password prompt halfway through.
pub fn escalate() -> Result<(), String> {
    let status = Command::new("sudo")
        .arg("-v")
        .status()
        .map_err(|e| format!("could not run sudo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("sudo authentication failed".into())
    }
}

/// PIDs of processes whose executable lives inside `bundle`.
///
/// Parses `ps -Ao pid=,comm=`. Not `pgrep -f`: that silently skips any process
/// whose argv it cannot read, which on macOS includes hardened-runtime apps, so
/// a running app can look stopped. Matching on the executable path also means a
/// grep or editor that merely mentions the bundle is never mistaken for the app.
pub fn parse_pids(ps_output: &str, bundle: &Path) -> Vec<u32> {
    let prefix = format!("{}/", bundle.display());
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, comm) = line.trim_start().split_once(' ')?;
            comm.trim_start()
                .starts_with(&prefix)
                .then(|| pid.parse().ok())
                .flatten()
        })
        .collect()
}

pub fn running_pids(app: &AppBundle) -> Vec<u32> {
    let Ok(out) = Command::new("ps").args(["-Ao", "pid=,comm="]).output() else {
        return Vec::new();
    };
    parse_pids(&String::from_utf8_lossy(&out.stdout), &app.path)
}

pub fn is_running(app: &AppBundle) -> bool {
    !running_pids(app).is_empty()
}

/// Ask the app to quit, then kill it if it ignores the request.
pub fn quit(app: &AppBundle) -> bool {
    if !is_running(app) {
        return false;
    }
    if let Some(id) = &app.bundle_id {
        let _ = Command::new("osascript")
            .args(["-e", &format!(r#"tell application id "{id}" to quit"#)])
            .output();
    }
    for _ in 0..10 {
        if !is_running(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    // Still up: kill the exact PIDs, never a pattern.
    let stubborn = running_pids(app);
    if !stubborn.is_empty() {
        let mut kill = Command::new("kill");
        kill.arg("-9");
        for pid in stubborn {
            kill.arg(pid.to_string());
        }
        let _ = kill.output();
    }
    true
}

pub fn execute(items: &[Leftover], mode: Mode) -> Report {
    let mut report = Report::default();

    // Jobs first: a running daemon rewrites its support files as they vanish.
    for item in items {
        if let Kind::LaunchdJob { label, domain, .. } = &item.kind {
            bootout(domain, label, item.needs_root);
        }
    }

    let mut to_trash: Vec<&Leftover> = Vec::new();
    for item in items {
        if item.target_path().is_some_and(gone) {
            continue; // already gone, e.g. brew's zap got there first
        }
        match (&item.kind, item.needs_root, mode) {
            (Kind::Receipt(id), _, _) => record(&mut report, item, forget_receipt(id)),
            (_, false, Mode::Trash) => to_trash.push(item),
            (_, needs_root, _) => {
                let path = item.target_path().expect("only a receipt has no path");
                if needs_root && mode == Mode::Trash {
                    report.forced_purge.push(path.display().to_string());
                }
                let outcome = remove_path(path, mode, needs_root);
                record(&mut report, item, outcome);
            }
        }
    }
    trash_batch(&to_trash, &mut report);
    report
}

fn record(report: &mut Report, item: &Leftover, outcome: Result<(), String>) {
    match outcome {
        Ok(()) => {
            report.freed += item.size;
            report.removed.push(item.display());
        }
        Err(e) => report.failed.push((item.display(), e)),
    }
}

/// The `trash` crate defaults to driving Finder over AppleScript. That needs
/// Automation permission, and without it the call blocks until the AppleEvent
/// times out, minutes later, having sometimes moved the files anyway. Use
/// `trashItemAtURL` instead: no Finder, no permission prompt, immediate.
#[cfg(target_os = "macos")]
fn trash_context() -> trash::TrashContext {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx
}

#[cfg(not(target_os = "macos"))]
fn trash_context() -> trash::TrashContext {
    trash::TrashContext::default()
}

fn gone(path: &Path) -> bool {
    !path.exists() && path.symlink_metadata().is_err()
}

/// Trash the batch in one call, then retry individually so a single stubborn
/// item is reported by name instead of sinking the whole batch.
fn trash_batch(items: &[&Leftover], report: &mut Report) {
    if items.is_empty() {
        return;
    }
    let ctx = trash_context();
    let paths: Vec<&Path> = items.iter().filter_map(|i| i.target_path()).collect();
    if ctx.delete_all(&paths).is_ok() {
        for item in items {
            record(report, item, Ok(()));
        }
        return;
    }
    for item in items {
        let Some(path) = item.target_path() else {
            continue;
        };
        // A partially applied batch already moved some of these.
        let outcome = if gone(path) {
            Ok(())
        } else {
            ctx.delete(path).map_err(|e| format!("{e}"))
        };
        record(report, item, outcome);
    }
}

fn trash_one(path: &Path) -> Result<(), String> {
    trash_context().delete(path).map_err(|e| format!("{e}"))
}

fn bootout(domain: &str, label: &str, needs_root: bool) {
    let target = format!("{domain}/{label}");
    if needs_root {
        let _ = Command::new("sudo")
            .args(["launchctl", "bootout", &target])
            .output();
    } else {
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .output();
    }
}

/// The Trash is per-user, so a root-owned item is deleted outright. Callers
/// record that in the report before calling.
fn remove_path(path: &Path, mode: Mode, needs_root: bool) -> Result<(), String> {
    if gone(path) {
        return Ok(());
    }
    if needs_root {
        return sudo_rm(path);
    }
    match mode {
        Mode::Trash => trash_one(path),
        Mode::Purge => rm_rf(path),
    }
}

fn sudo_rm(path: &Path) -> Result<(), String> {
    let output = Command::new("sudo")
        .arg("rm")
        .arg("-rf")
        .arg(path)
        .output()
        .map_err(|e| format!("sudo rm: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// `rm -rf` rather than `fs::remove_dir_all`: BSD `rm -f` chmods read-only
/// files before unlink, where std returns EACCES.
fn rm_rf(path: &Path) -> Result<(), String> {
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(path).output();
    let output = Command::new("rm")
        .arg("-rf")
        .arg(path)
        .output()
        .map_err(|e| format!("rm: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn forget_receipt(id: &str) -> Result<(), String> {
    let output = Command::new("sudo")
        .args(["pkgutil", "--forget", id])
        .output()
        .map_err(|e| format!("pkgutil: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leftovers::Confidence;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn item(path: &Path, size: u64) -> Leftover {
        Leftover {
            kind: Kind::Path(path.to_path_buf()),
            reason: "test".into(),
            size,
            confidence: Confidence::Exact,
            needs_root: false,
        }
    }

    #[test]
    fn purge_deletes_a_directory_tree() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("Support");
        fs::create_dir_all(target.join("nested")).unwrap();
        fs::write(target.join("nested/a"), b"x").unwrap();

        remove_path(&target, Mode::Purge, false).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn purge_deletes_read_only_files() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("locked");
        fs::create_dir(&target).unwrap();
        let file = target.join("ro");
        fs::write(&file, b"x").unwrap();
        let mut perms = fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o400);
        fs::set_permissions(&file, perms).unwrap();

        remove_path(&target, Mode::Purge, false).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn purge_removes_a_dangling_symlink() {
        let dir = tempdir().unwrap();
        let link = dir.path().join("warp-cli");
        std::os::unix::fs::symlink("/no/such/target", &link).unwrap();
        assert!(!link.exists(), "precondition: target is missing");

        remove_path(&link, Mode::Purge, false).unwrap();
        assert!(link.symlink_metadata().is_err());
    }

    #[test]
    fn removing_a_missing_path_is_not_an_error() {
        let dir = tempdir().unwrap();
        remove_path(&dir.path().join("gone"), Mode::Purge, false).unwrap();
    }

    #[test]
    fn trash_moves_the_file_out_of_the_way() {
        let dir = tempdir().unwrap();
        let name = format!("wsctl-trash-test-{}", std::process::id());
        let target = dir.path().join(&name);
        fs::write(&target, b"x").unwrap();

        let result = remove_path(&target, Mode::Trash, false);

        // Clean up whatever landed in the Trash before asserting.
        if let Some(home) = dirs::home_dir() {
            let trashed: PathBuf = home.join(".Trash").join(&name);
            let existed = trashed.exists();
            let _ = fs::remove_file(&trashed);
            assert!(result.is_ok(), "trash failed: {result:?}");
            assert!(existed, "expected {} in ~/.Trash", name);
        }
        assert!(!target.exists());
    }

    #[test]
    fn execute_trashes_a_whole_batch_in_one_go() {
        let dir = tempdir().unwrap();
        let names: Vec<String> = (0..4)
            .map(|i| format!("wsctl-batch-{}-{i}", std::process::id()))
            .collect();
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|n| {
                let p = dir.path().join(n);
                fs::write(&p, b"x").unwrap();
                p
            })
            .collect();

        let items: Vec<Leftover> = paths.iter().map(|p| item(p, 10)).collect();
        let report = execute(&items, Mode::Trash);

        let trash = dirs::home_dir().unwrap().join(".Trash");
        let landed = names.iter().filter(|n| trash.join(n).exists()).count();
        for n in &names {
            let _ = fs::remove_file(trash.join(n));
        }

        assert!(report.failed.is_empty(), "failed: {:?}", report.failed);
        assert_eq!(report.removed.len(), 4);
        assert_eq!(landed, 4, "every item should reach the Trash");
        assert!(paths.iter().all(|p| !p.exists()));
    }

    #[test]
    fn execute_totals_freed_bytes_and_lists_what_went() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"x").unwrap();
        fs::write(&b, b"x").unwrap();

        let report = execute(&[item(&a, 100), item(&b, 40)], Mode::Purge);
        assert_eq!(report.freed, 140);
        assert_eq!(report.removed.len(), 2);
        assert!(report.failed.is_empty());
        assert!(!a.exists() && !b.exists());
    }

    #[test]
    fn execute_records_failures_without_stopping() {
        let dir = tempdir().unwrap();
        let good = dir.path().join("good");
        fs::write(&good, b"x").unwrap();
        let bad = PathBuf::from("/System/Library/CoreServices/SystemVersion.plist");

        let mut items = vec![item(&good, 10)];
        let mut blocked = item(&bad, 10);
        blocked.needs_root = false;
        items.push(blocked);

        let report = execute(&items, Mode::Purge);
        assert!(!good.exists(), "the removable item still went");
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.failed.len(), 1, "SIP should refuse the second");
        assert!(bad.exists(), "SIP-protected file must survive");
    }

    #[test]
    fn tcc_denials_are_told_apart_from_ordinary_failures() {
        assert!(is_tcc_denial("rm: /x: Operation not permitted"));
        assert!(is_tcc_denial(
            "\u{201c}x\u{201d} couldn\u{2019}t be moved to the trash because you don\u{2019}t have permission to access it."
        ));
        assert!(!is_tcc_denial("No such file or directory"));
        assert!(!is_tcc_denial("Permission denied"));
    }

    #[test]
    fn requires_root_reports_any_privileged_item() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("x");
        let mut privileged = item(&p, 0);
        privileged.needs_root = true;
        assert!(!requires_root(&[item(&p, 0)]));
        assert!(requires_root(&[item(&p, 0), privileged]));
    }

    const PS: &str = "\
  3637 /Applications/Ghostty.app/Contents/MacOS/ghostty
 64229 /Applications/Slack.app/Contents/MacOS/Slack
 64241 /Applications/Slack.app/Contents/Frameworks/Slack Helper.app/Contents/MacOS/Slack Helper
   901 /usr/libexec/secinitd
 55010 /Applications/Slackware.app/Contents/MacOS/Slackware
 62852 sh -c sleep 120 # /Applications/Slack.app/Contents/MacOS/Slack
";

    #[test]
    fn parse_pids_finds_every_process_inside_the_bundle() {
        let pids = parse_pids(PS, Path::new("/Applications/Slack.app"));
        assert_eq!(
            pids,
            vec![64229, 64241],
            "the main process and its helper, including a path with spaces"
        );
    }

    #[test]
    fn parse_pids_ignores_a_similarly_named_bundle() {
        let pids = parse_pids(PS, Path::new("/Applications/Slack.app"));
        assert!(!pids.contains(&55010), "Slackware.app is a different app");
    }

    #[test]
    fn parse_pids_ignores_processes_that_merely_mention_the_bundle() {
        let pids = parse_pids(PS, Path::new("/Applications/Slack.app"));
        assert!(
            !pids.contains(&62852),
            "a shell whose arguments name the app is not the app"
        );
    }

    #[test]
    fn parse_pids_survives_junk_lines() {
        assert!(parse_pids("", Path::new("/Applications/X.app")).is_empty());
        assert!(parse_pids("garbage\n\n   \n", Path::new("/Applications/X.app")).is_empty());
        assert!(
            parse_pids(
                "notanumber /Applications/X.app/Contents/MacOS/X",
                Path::new("/Applications/X.app")
            )
            .is_empty()
        );
    }

    #[test]
    fn running_pids_sees_a_process_pgrep_would_miss() {
        // Guards the reason ps is used at all: any real running app must be
        // visible here. Skipped when no app happens to be running.
        let ps = Command::new("ps")
            .args(["-Ao", "pid=,comm="])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&ps.stdout);
        let Some(bundle) = text
            .lines()
            .filter_map(|l| l.trim_start().split_once(' ').map(|(_, c)| c.trim_start()))
            .find_map(|comm| comm.find(".app/").map(|i| comm[..i + 4].to_string()))
        else {
            return;
        };
        let app = AppBundle {
            path: PathBuf::from(&bundle),
            name: "probe".into(),
            bundle_id: None,
        };
        assert!(
            !running_pids(&app).is_empty(),
            "{bundle} is running but was not detected"
        );
    }

    #[test]
    fn is_running_is_false_for_a_bundle_nobody_is_running() {
        let app = AppBundle {
            path: PathBuf::from("/Applications/Definitely Not Running 9f3a.app"),
            name: "x".into(),
            bundle_id: None,
        };
        assert!(!is_running(&app));
        assert!(!quit(&app));
    }
}
