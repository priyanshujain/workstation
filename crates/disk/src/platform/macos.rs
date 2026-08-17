use std::path::PathBuf;
use std::process::Command;

use crate::cleanup::{CleanAction, Measure, Target};

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

/// Paths the cleanup screen is allowed to empty.
///
/// Accounting and deletion are different questions, so they get different
/// lists. A path in [`audit_categories`] is there to say where the bytes went;
/// it earns a delete key only by appearing here, and it qualifies only when
/// what it holds is derived and comes back on its own, so losing it costs time
/// and bandwidth instead of work. Anything authored, installed deliberately, or
/// holding credentials, profiles or history stays accounting-only, and so does
/// anything undecided: the worst case here is a row nobody can delete, and the
/// worst case the other way is destroyed work.
///
/// Some entries are narrower than the audit path that accounts for them. A
/// directory that keeps a download cache next to installed binaries or tokens
/// is not deletable as a whole, but the cache inside it is.
pub fn cleanable_paths() -> Vec<(&'static str, &'static str, PathBuf)> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));

    vec![
        ("Go", "Packages (~/go/pkg)", home.join("go/pkg")),
        ("Go", "Build cache", go_cache),
        (
            "Go",
            "goimports cache",
            home.join("Library/Caches/goimports"),
        ),
        ("Node.js", "npm cache", home.join(".npm")),
        ("Node.js", "pnpm cache", home.join("Library/Caches/pnpm")),
        // ~/.bun itself holds the bun binary and every global install.
        (
            "Node.js",
            "bun package cache",
            home.join(".bun/install/cache"),
        ),
        ("Python", "uv cache", home.join(".cache/uv")),
        ("Python", "pip cache", home.join("Library/Caches/pip")),
        // ~/.cargo also holds ~/.cargo/bin and the registry tokens.
        ("Rust", "cargo registry", home.join(".cargo/registry")),
        ("Kotlin/Native", "konan", home.join(".konan")),
        // ~/.gradle keeps gradle.properties and init scripts beside the caches.
        ("Gradle", "gradle caches", home.join(".gradle/caches")),
        (
            "Gradle",
            "gradle wrapper dists",
            home.join(".gradle/wrapper/dists"),
        ),
        // ~/.m2 keeps settings.xml, which usually holds repository credentials.
        ("Gradle", "maven repository", home.join(".m2/repository")),
        (
            "Xcode",
            "DerivedData",
            home.join("Library/Developer/Xcode/DerivedData"),
        ),
        // Symbols copied off a device, re-copied the next time it is plugged in.
        (
            "Xcode",
            "Device support",
            home.join("Library/Developer/Xcode/iOS DeviceSupport"),
        ),
        (
            "Xcode",
            "SwiftPM cache",
            home.join("Library/Caches/org.swift.swiftpm"),
        ),
        ("Homebrew", "Cache", home.join("Library/Caches/Homebrew")),
        (
            "Editors",
            "JetBrains cache",
            home.join("Library/Caches/JetBrains"),
        ),
        (
            "Agent tooling",
            "Playwright browsers",
            home.join("Library/Caches/ms-playwright"),
        ),
        // ms-playwright-mcp is deliberately absent: it sits under Caches but
        // holds per-session browser profiles, and signing the automation back
        // into every site is not something that happens on its own.
        ("Apps", "Chrome cache", home.join("Library/Caches/Google")),
        (
            "Apps",
            "Slack updates",
            home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt"),
        ),
    ]
}

/// The curated actions, listed without measuring any of them.
///
/// Every size here comes from a walk, and walking them all before returning is
/// what used to leave the cleanup screen blank for tens of seconds. Each target
/// says how it wants to be measured instead and the caller does the walking,
/// which lets it draw the list first and fill the numbers in.
pub fn cleanup_targets() -> Vec<Target> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut targets = Vec::new();

    let brew_cache = home.join("Library/Caches/Homebrew");
    targets.push(Target::pending(
        "Homebrew cache",
        "Old bottles and stale downloads",
        Measure::Tree(brew_cache),
        CleanAction::RunCommand("brew".into(), vec!["cleanup".into(), "--prune=all".into()]),
    ));

    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));
    targets.push(Target::pending(
        "Go build cache",
        "Compiled build artifacts",
        Measure::Tree(go_cache),
        CleanAction::RunCommand("go".into(), vec!["clean".into(), "-cache".into()]),
    ));

    let npm_cache = home.join(".npm/_cacache");
    targets.push(Target::pending(
        "npm cache",
        "Package download cache",
        Measure::Tree(npm_cache),
        CleanAction::RunCommand(
            "npm".into(),
            vec!["cache".into(), "clean".into(), "--force".into()],
        ),
    ));

    targets.push(Target::unknown(
        "pnpm store (unreferenced)",
        "Unreferenced packages in pnpm store",
        CleanAction::RunCommand("pnpm".into(), vec!["store".into(), "prune".into()]),
    ));

    let playwright = home.join("Library/Caches/ms-playwright");
    targets.push(Target::pending(
        "Playwright browsers",
        "Cached browser binaries for testing",
        Measure::Tree(playwright.clone()),
        CleanAction::RemoveContents(playwright),
    ));

    let chrome_cache = home.join("Library/Caches/Google");
    targets.push(Target::pending(
        "Chrome cache",
        "Google Chrome browser cache",
        Measure::Tree(chrome_cache.clone()),
        CleanAction::RemoveContents(chrome_cache),
    ));

    let slack_cache = home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt");
    targets.push(Target::pending(
        "Slack update cache",
        "Slack auto-update downloads",
        Measure::Tree(slack_cache.clone()),
        CleanAction::RemoveContents(slack_cache),
    ));

    let derived_data = home.join("Library/Developer/Xcode/DerivedData");
    targets.push(Target::pending(
        "Xcode DerivedData",
        "Build artifacts from Xcode projects",
        Measure::Tree(derived_data.clone()),
        CleanAction::RemoveContents(derived_data),
    ));

    if command_exists("xcrun") {
        targets.push(Target::unknown(
            "Xcode stale simulators",
            "Unavailable simulator runtimes",
            CleanAction::RunCommand(
                "xcrun".into(),
                vec!["simctl".into(), "delete".into(), "unavailable".into()],
            ),
        ));

        // Runtimes are 15-20 GB each. 180 days rather than simctl's own
        // shorter suggestions: a runtime kept for occasional back-compat
        // testing is worth far more than the space it holds.
        targets.push(Target::unknown(
            "Simulator runtimes unused 180 days",
            "Whole iOS/tvOS/watchOS runtime images nothing has booted",
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
        targets.push(Target::pending(
            "Claude session scratchpads",
            "Scratch dirs from sessions idle over a week, live ones kept",
            Measure::IdleChildren {
                dir: scratch.clone(),
                idle_days: IDLE_DAYS,
            },
            CleanAction::RemoveIdleChildren {
                dir: scratch,
                idle_days: IDLE_DAYS,
            },
        ));
    }

    if command_exists("docker") {
        targets.push(Target::unknown(
            "Docker unused data",
            "Dangling images, stopped containers, unused networks",
            CleanAction::RunCommand(
                "docker".into(),
                vec!["system".into(), "prune".into(), "-f".into()],
            ),
        ));
    }

    // One readdir, no recursion: this one is cheap enough to answer up front.
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
