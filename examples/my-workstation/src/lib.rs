//! Example workstation configuration
//!
//! This demonstrates how to define your workstation configuration using the ws DSL.
//! Copy this as a starting point for your own configuration.

use ws_dsl::prelude::*;

/// Define your workstation configuration
///
/// This function returns a `Workstation` that defines:
/// - Scopes: Groups of related resources (e.g., "personal", "work")
/// - Profiles: Machine configurations that include specific scopes
pub fn config() -> Workstation {
    Workstation::builder("pj-workstation")
        // ============================================
        // PERSONAL SCOPE
        // Core development tools and applications
        // ============================================
        .scope("personal", |s| {
            s
                // --- CLI Tools ---
                .brew_formula("git")
                .brew_formula("ripgrep")
                .brew_formula("fzf")
                .brew_formula("neovim")
                .brew_formula("helix")
                .brew_formula("imagemagick")
                .brew_formula("ffmpeg")
                .brew_formula("graphviz")
                .brew_formula("pandoc")
                .brew_formula("pandoc-crossref")
                .brew_formula("weasyprint")
                .brew_formula("et")
                // --- Languages & Runtimes ---
                .brew_formula("go")
                .brew_formula("pyenv")
                .brew_formula("nvm")
                .brew_formula("rustup")
                .brew_formula("pnpm")
                .brew_formula("bun")
                .brew_formula("clojure")
                .brew_formula("sbcl")
                .brew_formula("elixir")
                .brew_formula("erlang")
                // --- iOS & Mobile ---
                .brew_formula("cocoapods")
                .brew_formula("ios-deploy")
                .brew_formula("applesimutils")
                .brew_formula("fastlane")
                .brew_formula("maestro")
                // --- Infra & DevOps ---
                .brew_formula("terraform")
                .brew_formula("istioctl")
                // --- Cross-compilation & Signing ---
                .brew_formula("mingw-w64")
                .brew_formula("makensis")
                .brew_formula("osslsigncode")
                // --- Misc ---
                .brew_formula("timidity")
                // --- Terminals ---
                .brew_cask("ghostty")
                // --- Editors & Dev Tools ---
                .brew_cask("visual-studio-code")
                .brew_cask("docker")
                .brew_cask("figma")
                // --- Communication ---
                .brew_cask("slack")
                .brew_cask("whatsapp")
                .brew_cask("telegram")
                // --- AI ---
                .brew_cask("claude")
                // --- Productivity ---
                .brew_cask("raycast")
                .brew_cask("google-chrome")
                .brew_cask("google-drive")
                // --- Security & Networking ---
                .brew_cask("cloudflare-warp")
                .brew_cask("protonvpn")
                .brew_cask("bitwarden")
                .brew_cask("tailscale")
                // --- Media ---
                .brew_cask("vlc")
                .brew_cask("qbittorrent")
        })
        // ============================================
        // OKCREDIT SCOPE
        // Work-specific tools
        // ============================================
        .scope("okcredit", |s| {
            s.brew_cask("datagrip").brew_cask("google-cloud-sdk")
        })
        // ============================================
        // MACHINE PROFILES
        // ============================================
        // Personal MacBook: Only personal tools
        .profile("personal-macbook", &["personal"])
        // Work MacBook: Personal + OkCredit tools
        .profile("work-macbook", &["personal", "okcredit"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builds() {
        let workstation = config();
        assert_eq!(workstation.name, "pj-workstation");
    }

    #[test]
    fn test_profiles_exist() {
        let workstation = config();
        let profiles = workstation.profile_names();
        assert!(profiles.contains(&"personal-macbook".to_string()));
        assert!(profiles.contains(&"work-macbook".to_string()));
    }

    #[test]
    fn test_scopes_exist() {
        let workstation = config();
        let scopes = workstation.scope_names();
        assert!(scopes.contains(&"personal".to_string()));
        assert!(scopes.contains(&"okcredit".to_string()));
    }

    #[test]
    fn test_build_graph_personal() {
        let workstation = config();
        let graph = workstation.build_graph("personal-macbook").unwrap();
        // Personal scope: 33 formulae + 17 casks = 50 resources
        assert_eq!(graph.len(), 50);
    }

    #[test]
    fn test_build_graph_work() {
        let workstation = config();
        let graph = workstation.build_graph("work-macbook").unwrap();
        // Work scope: personal (50) + okcredit (2) = 52 resources
        assert_eq!(graph.len(), 52);
    }
}
