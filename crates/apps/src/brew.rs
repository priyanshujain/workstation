//! Recognise an app that Homebrew installed, so removal goes through
//! `brew uninstall --cask --zap` and does not strand brew's own metadata.

/// `Cloudflare WARP` becomes `cloudflare-warp`, the shape of a cask token.
pub fn slug(app_name: &str) -> String {
    let mut out = String::with_capacity(app_name.len());
    let mut pending_dash = false;
    for c in app_name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// The installed cask token for `app_name`, if one plausibly matches.
pub fn match_cask(app_name: &str, installed: &[String]) -> Option<String> {
    let wanted = slug(app_name);
    if wanted.is_empty() {
        return None;
    }
    installed
        .iter()
        .find(|token| slug(token) == wanted)
        .map(|t| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn casks(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn slug_lowercases_and_joins_words_with_dashes() {
        assert_eq!(slug("Cloudflare WARP"), "cloudflare-warp");
        assert_eq!(slug("Slack"), "slack");
        assert_eq!(slug("Visual Studio Code"), "visual-studio-code");
    }

    #[test]
    fn slug_collapses_punctuation_and_runs_of_spaces() {
        assert_eq!(slug("IINA+  (beta)"), "iina-beta");
        assert_eq!(slug("  Foo   Bar  "), "foo-bar");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn match_cask_finds_the_installed_token() {
        let installed = casks(&["cloudflare-warp", "slack", "ghostty"]);
        assert_eq!(
            match_cask("Cloudflare WARP", &installed).as_deref(),
            Some("cloudflare-warp")
        );
        assert_eq!(match_cask("Slack", &installed).as_deref(), Some("slack"));
    }

    #[test]
    fn match_cask_is_none_for_apps_brew_does_not_manage() {
        let installed = casks(&["slack"]);
        assert_eq!(match_cask("Xcode", &installed), None);
        assert_eq!(match_cask("!!!", &installed), None);
        assert_eq!(match_cask("Slack", &[]), None);
    }
}
