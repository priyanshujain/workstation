use std::path::PathBuf;
use std::process::Command;

use crate::cleanup::{CleanAction, Target};
use crate::util::dir_size;

/// Paths that get a category name in the audit. This list is attribution
/// only: it decides what a byte is *called*, never whether it is counted.
/// Everything it misses is still measured and reported as unattributed, which
/// is the difference between this and the allowlist it replaced.
///
/// A deeper rule wins over a shallower one, so a broad directory can carry a
/// general name while the interesting subtrees inside it keep their own.
pub fn audit_categories() -> Vec<(&'static str, Vec<(&'static str, PathBuf)>)> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));

    vec![
        (
            "Go",
            vec![
                ("Source (~/go/src)", home.join("go/src")),
                ("Packages (~/go/pkg)", home.join("go/pkg")),
                ("Binaries (~/go/bin)", home.join("go/bin")),
                ("Toolchains (~/sdk)", home.join("sdk")),
                ("Build cache", go_cache),
                ("goimports cache", home.join("Library/Caches/goimports")),
            ],
        ),
        (
            "Node.js",
            vec![
                ("nvm", home.join(".nvm")),
                ("npm cache", home.join(".npm")),
                ("pnpm", home.join("Library/pnpm")),
                ("pnpm cache", home.join("Library/Caches/pnpm")),
                ("bun", home.join(".bun")),
                ("deno", home.join(".deno")),
            ],
        ),
        (
            "Python",
            vec![
                ("uv cache", home.join(".cache/uv")),
                ("pyenv", home.join(".pyenv")),
                ("pip cache", home.join("Library/Caches/pip")),
            ],
        ),
        (
            "Rust",
            vec![
                ("rustup", home.join(".rustup")),
                ("cargo", home.join(".cargo")),
            ],
        ),
        (
            "Haskell",
            vec![
                ("ghcup", home.join(".ghcup")),
                ("cabal", home.join(".cabal")),
                ("stack", home.join(".stack")),
            ],
        ),
        ("OCaml", vec![("opam", home.join(".opam"))]),
        ("Lean", vec![("elan", home.join(".elan"))]),
        ("Kotlin/Native", vec![("konan", home.join(".konan"))]),
        (
            "Gradle",
            vec![
                ("gradle", home.join(".gradle")),
                ("maven", home.join(".m2")),
            ],
        ),
        (
            "Android",
            vec![
                ("Emulator images", home.join(".android/avd")),
                ("SDK state", home.join(".android")),
            ],
        ),
        (
            "Xcode",
            vec![
                (
                    "DerivedData",
                    home.join("Library/Developer/Xcode/DerivedData"),
                ),
                ("Simulators", home.join("Library/Developer/CoreSimulator")),
                ("Archives", home.join("Library/Developer/Xcode/Archives")),
                (
                    "Device support",
                    home.join("Library/Developer/Xcode/iOS DeviceSupport"),
                ),
                (
                    "SwiftPM cache",
                    home.join("Library/Caches/org.swift.swiftpm"),
                ),
            ],
        ),
        (
            "Homebrew",
            vec![
                ("Installation", PathBuf::from("/opt/homebrew")),
                ("Cache", home.join("Library/Caches/Homebrew")),
            ],
        ),
        (
            "Containers",
            vec![
                (
                    "Docker data",
                    home.join("Library/Containers/com.docker.docker/Data"),
                ),
                ("Docker config", home.join(".docker")),
                ("OrbStack", home.join(".orbstack")),
                ("colima", home.join(".colima")),
                ("lima", home.join(".lima")),
            ],
        ),
        (
            "Editors",
            vec![
                ("VS Code", home.join("Library/Application Support/Code")),
                ("VS Code extensions", home.join(".vscode")),
                ("Cursor", home.join(".cursor")),
                (
                    "JetBrains",
                    home.join("Library/Application Support/JetBrains"),
                ),
                ("JetBrains cache", home.join("Library/Caches/JetBrains")),
            ],
        ),
        (
            "Agent tooling",
            vec![
                ("Claude Code", home.join(".claude")),
                ("Claude experiments", home.join(".claude-science")),
                (
                    "Claude desktop",
                    home.join("Library/Application Support/Claude"),
                ),
                (
                    "Playwright browsers",
                    home.join("Library/Caches/ms-playwright"),
                ),
                (
                    "Playwright MCP",
                    home.join("Library/Caches/ms-playwright-mcp"),
                ),
            ],
        ),
        (
            "Apps",
            vec![
                ("Chrome", home.join("Library/Application Support/Google")),
                ("Chrome cache", home.join("Library/Caches/Google")),
                ("Slack", home.join("Library/Application Support/Slack")),
                (
                    "Slack updates",
                    home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt"),
                ),
                ("Discord", home.join("Library/Application Support/discord")),
                (
                    "WhatsApp",
                    home.join("Library/Group Containers/group.net.whatsapp.WhatsApp.shared"),
                ),
            ],
        ),
        (
            "Cloud CLIs",
            vec![
                ("gcloud", home.join("google-cloud-sdk")),
                ("~/.local", home.join(".local")),
            ],
        ),
        (
            "Personal",
            vec![
                ("~/Downloads", home.join("Downloads")),
                ("~/Documents", home.join("Documents")),
                ("~/dotfiles", home.join("dotfiles")),
                ("~/Desktop", home.join("Desktop")),
            ],
        ),
    ]
}

pub fn cleanup_targets() -> Vec<Target> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut targets = Vec::new();

    let brew_cache = home.join("Library/Caches/Homebrew");
    targets.push(Target::new(
        "Homebrew cache",
        "Old bottles and stale downloads",
        dir_size(&brew_cache),
        CleanAction::RunCommand("brew".into(), vec!["cleanup".into(), "--prune=all".into()]),
    ));

    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));
    targets.push(Target::new(
        "Go build cache",
        "Compiled build artifacts",
        dir_size(&go_cache),
        CleanAction::RunCommand("go".into(), vec!["clean".into(), "-cache".into()]),
    ));

    let npm_cache = home.join(".npm/_cacache");
    targets.push(Target::new(
        "npm cache",
        "Package download cache",
        dir_size(&npm_cache),
        CleanAction::RunCommand(
            "npm".into(),
            vec!["cache".into(), "clean".into(), "--force".into()],
        ),
    ));

    targets.push(Target::new(
        "pnpm store (unreferenced)",
        "Unreferenced packages in pnpm store",
        0,
        CleanAction::RunCommand("pnpm".into(), vec!["store".into(), "prune".into()]),
    ));

    let playwright = home.join("Library/Caches/ms-playwright");
    targets.push(Target::new(
        "Playwright browsers",
        "Cached browser binaries for testing",
        dir_size(&playwright),
        CleanAction::RemoveContents(playwright),
    ));

    let chrome_cache = home.join("Library/Caches/Google");
    targets.push(Target::new(
        "Chrome cache",
        "Google Chrome browser cache",
        dir_size(&chrome_cache),
        CleanAction::RemoveContents(chrome_cache),
    ));

    let slack_cache = home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt");
    targets.push(Target::new(
        "Slack update cache",
        "Slack auto-update downloads",
        dir_size(&slack_cache),
        CleanAction::RemoveContents(slack_cache),
    ));

    let derived_data = home.join("Library/Developer/Xcode/DerivedData");
    targets.push(Target::new(
        "Xcode DerivedData",
        "Build artifacts from Xcode projects",
        dir_size(&derived_data),
        CleanAction::RemoveContents(derived_data),
    ));

    if command_exists("xcrun") {
        targets.push(Target::new(
            "Xcode stale simulators",
            "Unavailable simulator runtimes",
            0,
            CleanAction::RunCommand(
                "xcrun".into(),
                vec!["simctl".into(), "delete".into(), "unavailable".into()],
            ),
        ));

        // Runtimes are 15-20 GB each. 180 days rather than simctl's own
        // shorter suggestions: a runtime kept for occasional back-compat
        // testing is worth far more than the space it holds.
        targets.push(Target::new(
            "Simulator runtimes unused 180 days",
            "Whole iOS/tvOS/watchOS runtime images nothing has booted",
            0,
            CleanAction::RunCommand(
                "xcrun".into(),
                vec![
                    "simctl".into(),
                    "runtime".into(),
                    "delete".into(),
                    "--notUsedSinceDays".into(),
                    "180".into(),
                ],
            ),
        ));
    }

    // Session scratch dirs from agent tooling. Cleaned per child, never as a
    // whole: sessions still running keep working state in here.
    if let Some(scratch) = claude_scratch_dir() {
        const IDLE_DAYS: u64 = 7;
        let reclaimable = crate::cleanup::idle_children(&scratch, IDLE_DAYS)
            .iter()
            .map(|(_, size)| size)
            .sum();
        targets.push(Target::new(
            "Claude session scratchpads",
            "Scratch dirs from sessions idle over a week, live ones kept",
            reclaimable,
            CleanAction::RemoveIdleChildren {
                dir: scratch,
                idle_days: IDLE_DAYS,
            },
        ));
    }

    if command_exists("docker") {
        targets.push(Target::new(
            "Docker unused data",
            "Dangling images, stopped containers, unused networks",
            0,
            CleanAction::RunCommand(
                "docker".into(),
                vec!["system".into(), "prune".into(), "-f".into()],
            ),
        ));
    }

    let downloads = home.join("Downloads");
    targets.push(Target::new(
        "DMG installers",
        "Downloaded .dmg files in ~/Downloads",
        crate::cleanup::files_by_extension_size(&downloads, "dmg"),
        CleanAction::RemoveByExtension(downloads, "dmg".into()),
    ));

    targets
}

fn claude_scratch_dir() -> Option<PathBuf> {
    let out = Command::new("id").arg("-u").output().ok()?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        return None;
    }
    let dir = PathBuf::from(format!("/private/tmp/claude-{uid}"));
    dir.is_dir().then_some(dir)
}

fn go_cache_dir() -> Option<PathBuf> {
    let output = Command::new("go").args(["env", "GOCACHE"]).output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn command_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}
