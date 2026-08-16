//! Brewfile parsing, discovery, and editing.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Formula,
    Cask,
    Tap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewfileEntry {
    pub kind: EntryKind,
    /// Unqualified package name. For `brew "tap/repo/name"` this is `name`.
    pub name: String,
    /// The full name as written, including any tap prefix.
    pub full_name: String,
    /// Original line, preserved for editing.
    pub raw: String,
    /// 1-based line number in the source file.
    pub line_number: usize,
}

/// Parse a Brewfile's contents into typed entries.
///
/// Only `brew`, `cask`, and `tap` lines are returned. Other DSL forms
/// (`vscode`, `mas`, `go`, `cargo`, `npm`, `krew`, ...) and comments are skipped.
pub fn parse(content: &str) -> Vec<BrewfileEntry> {
    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| parse_line(line, i + 1))
        .collect()
}

fn parse_line(line: &str, line_number: usize) -> Option<BrewfileEntry> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (prefix, rest) = trimmed.split_once(char::is_whitespace)?;
    let kind = match prefix {
        "brew" => EntryKind::Formula,
        "cask" => EntryKind::Cask,
        "tap" => EntryKind::Tap,
        _ => return None,
    };
    let full_name = extract_quoted(rest)?;
    let name = match kind {
        EntryKind::Formula | EntryKind::Cask => full_name
            .rsplit('/')
            .next()
            .unwrap_or(&full_name)
            .to_string(),
        EntryKind::Tap => full_name.clone(),
    };
    Some(BrewfileEntry {
        kind,
        name,
        full_name,
        raw: line.to_string(),
        line_number,
    })
}

fn extract_quoted(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'"' || b == b'\'')?;
    let quote = bytes[start];
    let after = &s[start + 1..];
    let end = after.find(quote as char)?;
    Some(after[..end].to_string())
}

/// Which step of the search order produced the Brewfile path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrewfileSource {
    /// `$HOMEBREW_BUNDLE_FILE` env var.
    Env,
    /// `./Brewfile` in the current working directory.
    Cwd,
    /// `~/.Brewfile` or `~/Brewfile`.
    Home,
    /// `$XDG_CONFIG_HOME/homebrew/Brewfile`.
    Xdg,
}

/// Discover a Brewfile following `brew bundle`'s search order.
///
/// 1. `$HOMEBREW_BUNDLE_FILE`            (`Env`)
/// 2. `./Brewfile`                       (`Cwd`)
/// 3. `~/.Brewfile`                      (`Home`)
/// 4. `~/Brewfile`                       (`Home`)
/// 5. `$XDG_CONFIG_HOME/homebrew/Brewfile` (`Xdg`, default `~/.config/homebrew/Brewfile`)
pub fn discover() -> Option<(PathBuf, BrewfileSource)> {
    discover_with(&Env::system())
}

#[derive(Debug, Clone)]
pub struct Env {
    pub bundle_file: Option<String>,
    pub xdg_config_home: Option<String>,
    pub cwd: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Env {
    pub fn system() -> Self {
        Self {
            bundle_file: std::env::var("HOMEBREW_BUNDLE_FILE").ok(),
            xdg_config_home: std::env::var("XDG_CONFIG_HOME").ok(),
            cwd: std::env::current_dir().ok(),
            home: dirs::home_dir(),
        }
    }
}

pub fn discover_with(env: &Env) -> Option<(PathBuf, BrewfileSource)> {
    if let Some(p) = env.bundle_file.as_deref() {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some((path, BrewfileSource::Env));
        }
    }
    if let Some(cwd) = &env.cwd {
        let p = cwd.join("Brewfile");
        if p.is_file() {
            return Some((p, BrewfileSource::Cwd));
        }
    }
    if let Some(home) = &env.home {
        let dot = home.join(".Brewfile");
        if dot.is_file() {
            return Some((dot, BrewfileSource::Home));
        }
        let plain = home.join("Brewfile");
        if plain.is_file() {
            return Some((plain, BrewfileSource::Home));
        }
    }
    let xdg = env
        .xdg_config_home
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| env.home.as_ref().map(|h| h.join(".config")));
    if let Some(xdg) = xdg {
        let p = xdg.join("homebrew/Brewfile");
        if p.is_file() {
            return Some((p, BrewfileSource::Xdg));
        }
    }
    None
}

/// A package to remove from a Brewfile, matched by (kind, unqualified name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoveTarget {
    pub kind: EntryKind,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveSummary {
    /// Path to the backup file, or `None` if nothing matched and no write occurred.
    pub backup: Option<PathBuf>,
    /// The entries that were removed.
    pub removed: Vec<BrewfileEntry>,
}

/// Remove matching entries from a Brewfile in place, writing a `.bak` backup first.
///
/// Matching is by `(kind, unqualified name)`, so `brew "tap/repo/maestro"` matches
/// `RemoveTarget { kind: Formula, name: "maestro" }`. Comments, blank lines, and
/// unsupported DSL forms (`vscode`, `go`, `cargo`, `npm`, `krew`, `mas`) are preserved.
pub fn remove_entries(path: &Path, targets: &[RemoveTarget]) -> std::io::Result<RemoveSummary> {
    let content = fs::read_to_string(path)?;
    let entries = parse(&content);

    let target_set: HashSet<(EntryKind, &str)> =
        targets.iter().map(|t| (t.kind, t.name.as_str())).collect();

    let mut remove_lines: HashSet<usize> = HashSet::new();
    let mut removed: Vec<BrewfileEntry> = Vec::new();
    for entry in &entries {
        if target_set.contains(&(entry.kind, entry.name.as_str())) {
            remove_lines.insert(entry.line_number);
            removed.push(entry.clone());
        }
    }

    if remove_lines.is_empty() {
        return Ok(RemoveSummary {
            backup: None,
            removed: Vec::new(),
        });
    }

    let backup = backup_path(path);
    fs::copy(path, &backup)?;

    let mut out = String::with_capacity(content.len());
    for (i, line) in content.lines().enumerate() {
        if remove_lines.contains(&(i + 1)) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !content.ends_with('\n') {
        out.pop();
    }
    fs::write(path, out)?;

    Ok(RemoveSummary {
        backup: Some(backup),
        removed,
    })
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("Brewfile"));
    name.push(".bak");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn parses_simple_formula() {
        let entries = parse(r#"brew "ripgrep""#);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Formula);
        assert_eq!(entries[0].name, "ripgrep");
        assert_eq!(entries[0].full_name, "ripgrep");
        assert_eq!(entries[0].line_number, 1);
    }

    #[test]
    fn parses_formula_with_options() {
        let entries = parse(r#"brew "postgresql@14", restart_service: :changed"#);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "postgresql@14");
    }

    #[test]
    fn strips_tap_prefix_from_formula_name() {
        let entries = parse(r#"brew "mobile-dev-inc/tap/maestro""#);
        assert_eq!(entries[0].name, "maestro");
        assert_eq!(entries[0].full_name, "mobile-dev-inc/tap/maestro");
    }

    #[test]
    fn parses_cask_and_strips_tap_prefix() {
        let entries = parse("cask \"visual-studio-code\"\ncask \"ngrok/ngrok/ngrok\"");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, EntryKind::Cask);
        assert_eq!(entries[0].name, "visual-studio-code");
        assert_eq!(entries[1].name, "ngrok");
        assert_eq!(entries[1].full_name, "ngrok/ngrok/ngrok");
    }

    #[test]
    fn parses_tap_without_stripping() {
        let entries = parse(r#"tap "antoniorodr/memo""#);
        assert_eq!(entries[0].kind, EntryKind::Tap);
        assert_eq!(entries[0].name, "antoniorodr/memo");
    }

    #[test]
    fn skips_comments_blanks_and_unsupported_dsl() {
        let input = "# header comment\n\nvscode \"anthropic.claude-code\"\ngo \"x/y\"\ncargo \"loc\"\nnpm \"pkg\"\nkrew \"graph\"\nmas \"App\", id: 1\n";
        let entries = parse(input);
        assert!(entries.is_empty());
    }

    #[test]
    fn preserves_raw_line_and_line_number() {
        let input = "tap \"a/b\"\n  brew \"fzf\"\n";
        let entries = parse(input);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].line_number, 2);
        assert_eq!(entries[1].raw, "  brew \"fzf\"");
        assert_eq!(entries[1].name, "fzf");
    }

    #[test]
    fn discover_prefers_env_override() {
        let dir = tempdir();
        let file = dir.join("custom-Brewfile");
        std::fs::write(&file, "brew \"x\"").unwrap();
        let env = Env {
            bundle_file: Some(file.to_string_lossy().into_owned()),
            xdg_config_home: None,
            cwd: None,
            home: None,
        };
        assert_eq!(discover_with(&env), Some((file, BrewfileSource::Env)));
    }

    #[test]
    fn discover_returns_cwd_source_when_cwd_brewfile_present() {
        let dir = tempdir();
        let cwd_file = dir.join("Brewfile");
        std::fs::write(&cwd_file, "").unwrap();
        let env = Env {
            bundle_file: None,
            xdg_config_home: None,
            cwd: Some(dir.clone()),
            home: Some(dir.join("home")),
        };
        assert_eq!(discover_with(&env), Some((cwd_file, BrewfileSource::Cwd)));
    }

    #[test]
    fn discover_falls_through_to_home_then_xdg() {
        let dir = tempdir();
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".config/homebrew")).unwrap();
        let xdg_path = home.join(".config/homebrew/Brewfile");
        std::fs::write(&xdg_path, "").unwrap();
        let env = Env {
            bundle_file: None,
            xdg_config_home: None,
            cwd: Some(dir.join("nowhere")),
            home: Some(home),
        };
        assert_eq!(discover_with(&env), Some((xdg_path, BrewfileSource::Xdg)));
    }

    #[test]
    fn discover_returns_none_when_missing() {
        let dir = tempdir();
        let env = Env {
            bundle_file: None,
            xdg_config_home: None,
            cwd: Some(dir.join("nowhere")),
            home: Some(dir.join("home")),
        };
        assert!(discover_with(&env).is_none());
    }

    #[test]
    fn remove_entries_removes_matching_and_preserves_rest() {
        let dir = tempdir();
        let file = dir.join("Brewfile");
        let original =
            "tap \"a/b\"\n# header\nbrew \"ripgrep\"\nbrew \"fzf\"\ncask \"vlc\"\nvscode \"x.y\"\n";
        fs::write(&file, original).unwrap();

        let targets = vec![
            RemoveTarget {
                kind: EntryKind::Formula,
                name: "fzf".into(),
            },
            RemoveTarget {
                kind: EntryKind::Cask,
                name: "vlc".into(),
            },
        ];
        let summary = remove_entries(&file, &targets).unwrap();

        assert_eq!(summary.removed.len(), 2);
        assert!(summary.backup.is_some());

        let after = fs::read_to_string(&file).unwrap();
        assert_eq!(
            after,
            "tap \"a/b\"\n# header\nbrew \"ripgrep\"\nvscode \"x.y\"\n"
        );

        let backup = fs::read_to_string(summary.backup.unwrap()).unwrap();
        assert_eq!(backup, original);
    }

    #[test]
    fn remove_entries_matches_tap_prefixed_packages() {
        let dir = tempdir();
        let file = dir.join("Brewfile");
        fs::write(&file, "brew \"mobile-dev-inc/tap/maestro\"\nbrew \"fzf\"\n").unwrap();
        let summary = remove_entries(
            &file,
            &[RemoveTarget {
                kind: EntryKind::Formula,
                name: "maestro".into(),
            }],
        )
        .unwrap();
        assert_eq!(summary.removed.len(), 1);
        let after = fs::read_to_string(&file).unwrap();
        assert_eq!(after, "brew \"fzf\"\n");
    }

    #[test]
    fn remove_entries_noop_when_no_matches() {
        let dir = tempdir();
        let file = dir.join("Brewfile");
        fs::write(&file, "brew \"ripgrep\"\n").unwrap();
        let summary = remove_entries(
            &file,
            &[RemoveTarget {
                kind: EntryKind::Formula,
                name: "fzf".into(),
            }],
        )
        .unwrap();
        assert!(summary.backup.is_none());
        assert!(summary.removed.is_empty());
        assert!(!file.with_file_name("Brewfile.bak").exists());
    }

    #[test]
    fn remove_entries_preserves_no_trailing_newline() {
        let dir = tempdir();
        let file = dir.join("Brewfile");
        fs::write(&file, "brew \"a\"\nbrew \"b\"").unwrap();
        remove_entries(
            &file,
            &[RemoveTarget {
                kind: EntryKind::Formula,
                name: "a".into(),
            }],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "brew \"b\"");
    }

    #[test]
    fn remove_entries_does_not_match_across_kinds() {
        let dir = tempdir();
        let file = dir.join("Brewfile");
        fs::write(&file, "brew \"foo\"\ncask \"foo\"\n").unwrap();
        remove_entries(
            &file,
            &[RemoveTarget {
                kind: EntryKind::Cask,
                name: "foo".into(),
            }],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "brew \"foo\"\n");
    }

    /// A directory of this test's own. The counter is what makes that true:
    /// the clock alone is not unique enough, since macOS hands out the same
    /// reading to two threads that ask within the same microsecond, and two
    /// tests that agreed on a directory then wrote each other's Brewfile. It
    /// only failed when the suite ran in parallel, which is every time except
    /// the one where you go looking for it.
    fn tempdir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "wsctl-brewfile-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
