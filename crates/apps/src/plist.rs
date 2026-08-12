//! Read plists as JSON via `plutil`, which handles binary and XML alike.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

pub fn read(path: &Path) -> Option<Value> {
    let out = Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// String value of `key` at the top level of the plist at `path`.
pub fn string_at(path: &Path, key: &str) -> Option<String> {
    read(path)?
        .get(key)?
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Every executable path a launchd job would run: `Program` plus `ProgramArguments`.
pub fn program_paths(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(p) = value.get("Program").and_then(Value::as_str) {
        paths.push(p.to_string());
    }
    if let Some(args) = value.get("ProgramArguments").and_then(Value::as_array) {
        paths.extend(args.iter().filter_map(Value::as_str).map(str::to_string));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_plist(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(
            &path,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>{body}</dict></plist>"#
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn string_at_reads_a_key() {
        let dir = tempdir().unwrap();
        let path = write_plist(
            dir.path(),
            "Info.plist",
            "<key>CFBundleIdentifier</key><string>com.example.thing</string>",
        );
        assert_eq!(
            string_at(&path, "CFBundleIdentifier").as_deref(),
            Some("com.example.thing")
        );
    }

    #[test]
    fn string_at_is_none_for_missing_key_or_file() {
        let dir = tempdir().unwrap();
        let path = write_plist(
            dir.path(),
            "Info.plist",
            "<key>Other</key><string>x</string>",
        );
        assert_eq!(string_at(&path, "CFBundleIdentifier"), None);
        assert_eq!(string_at(&dir.path().join("nope.plist"), "Any"), None);
    }

    #[test]
    fn string_at_ignores_empty_values() {
        let dir = tempdir().unwrap();
        let path = write_plist(dir.path(), "Info.plist", "<key>K</key><string>  </string>");
        assert_eq!(string_at(&path, "K"), None);
    }

    #[test]
    fn program_paths_merges_program_and_arguments() {
        let dir = tempdir().unwrap();
        let path = write_plist(
            dir.path(),
            "job.plist",
            "<key>Program</key><string>/usr/bin/foo</string>\
             <key>ProgramArguments</key><array><string>/usr/bin/foo</string><string>--run</string></array>",
        );
        let value = read(&path).unwrap();
        assert_eq!(
            program_paths(&value),
            vec!["/usr/bin/foo", "/usr/bin/foo", "--run"]
        );
    }

    #[test]
    fn program_paths_empty_when_absent() {
        let dir = tempdir().unwrap();
        let path = write_plist(
            dir.path(),
            "job.plist",
            "<key>Label</key><string>x</string>",
        );
        assert!(program_paths(&read(&path).unwrap()).is_empty());
    }
}
