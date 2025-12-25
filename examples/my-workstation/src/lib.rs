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
                // Terminal & Shell
                .brew_cask("ghostty")           // Modern terminal emulator

                // Productivity
                .brew_cask("raycast")           // Launcher and productivity tool

                // Code Editors
                .brew_cask("visual-studio-code") // VS Code

                // CLI Tools
                .brew_formula("git")            // Version control
                .brew_formula("ripgrep")        // Fast search (rg)
                .brew_formula("fzf")            // Fuzzy finder
                .brew_formula("neovim")         // Text editor

                // Containers
                .brew_cask("docker")            // Docker Desktop
        })

        // ============================================
        // OKCREDIT SCOPE
        // Work-specific tools
        // ============================================
        .scope("okcredit", |s| {
            s
                // Database Tools
                .brew_cask("datagrip")          // JetBrains DataGrip
        })

        // ============================================
        // MACHINE PROFILES
        // Define which scopes are active on each machine
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
        // Personal scope has: ghostty, raycast, vscode, git, ripgrep, fzf, neovim, docker = 8 resources
        assert_eq!(graph.len(), 8);
    }

    #[test]
    fn test_build_graph_work() {
        let workstation = config();
        let graph = workstation.build_graph("work-macbook").unwrap();
        // Work scope has: personal (8) + okcredit (1) = 9 resources
        assert_eq!(graph.len(), 9);
    }
}
