//! Brewfile parsing, discovery, and editing.

use std::path::PathBuf;

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

/// Discover a Brewfile following `brew bundle`'s search order.
///
/// 1. `$HOMEBREW_BUNDLE_FILE`
/// 2. `./Brewfile`
/// 3. `~/.Brewfile`
/// 4. `~/Brewfile`
/// 5. `$XDG_CONFIG_HOME/homebrew/Brewfile` (default `~/.config/homebrew/Brewfile`)
pub fn discover() -> Option<PathBuf> {
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

pub fn discover_with(env: &Env) -> Option<PathBuf> {
    if let Some(p) = env.bundle_file.as_deref() {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(cwd) = &env.cwd {
        let p = cwd.join("Brewfile");
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(home) = &env.home {
        let dot = home.join(".Brewfile");
        if dot.is_file() {
            return Some(dot);
        }
        let plain = home.join("Brewfile");
        if plain.is_file() {
            return Some(plain);
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
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
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
        assert_eq!(discover_with(&env), Some(file));
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
        assert_eq!(discover_with(&env), Some(xdg_path));
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

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "wsctl-brewfile-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
