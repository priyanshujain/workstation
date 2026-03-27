use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// === Disk Overview ===

pub struct DiskOverview {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

impl DiskOverview {
    pub fn usage_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used as f64 / self.total as f64) * 100.0
    }
}

/// Get disk overview via `df`.
/// On macOS, uses `/System/Volumes/Data` (the APFS data volume) for accurate usage.
pub fn disk_overview() -> Option<DiskOverview> {
    // macOS APFS: df / shows the small system volume, not actual disk usage
    let path = if Path::new("/System/Volumes/Data").exists() {
        "/System/Volumes/Data"
    } else {
        "/"
    };
    let output = Command::new("df").args(["-k", path]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().nth(1)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let total = parts[1].parse::<u64>().ok()? * 1024;
    let used = parts[2].parse::<u64>().ok()? * 1024;
    let free = parts[3].parse::<u64>().ok()? * 1024;
    Some(DiskOverview { total, used, free })
}

// === Categories (for audit) ===

pub struct Category {
    pub name: String,
    pub paths: Vec<CategoryPath>,
    pub total_size: u64,
}

pub struct CategoryPath {
    pub label: String,
    pub path: PathBuf,
    pub size: u64,
}

/// Scan disk usage by category. Returns categories sorted by size (largest first).
pub fn scan_categories() -> Vec<Category> {
    let home = dirs::home_dir().expect("could not determine home directory");
    let mut categories = Vec::new();

    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));

    categories.push(build_category(
        "Docker",
        vec![(
            "Docker data",
            home.join("Library/Containers/com.docker.docker/Data"),
        )],
    ));

    categories.push(build_category(
        "Go",
        vec![
            ("Source (~/go/src)", home.join("go/src")),
            ("Packages (~/go/pkg)", home.join("go/pkg")),
            ("Binaries (~/go/bin)", home.join("go/bin")),
            ("Build cache", go_cache),
        ],
    ));

    categories.push(build_category(
        "Node.js",
        vec![
            ("nvm", home.join(".nvm")),
            ("npm cache", home.join(".npm")),
            ("pnpm", home.join("Library/pnpm")),
            ("bun", home.join(".bun")),
        ],
    ));

    categories.push(build_category(
        "Python",
        vec![
            ("uv cache", home.join(".cache/uv")),
            ("pyenv", home.join(".pyenv")),
        ],
    ));

    categories.push(build_category(
        "Rust",
        vec![
            ("rustup", home.join(".rustup")),
            ("cargo", home.join(".cargo")),
        ],
    ));

    categories.push(build_category(
        "Kotlin/Native",
        vec![("konan", home.join(".konan"))],
    ));

    categories.push(build_category(
        "Gradle",
        vec![("gradle", home.join(".gradle"))],
    ));

    categories.push(build_category(
        "Xcode",
        vec![
            (
                "DerivedData",
                home.join("Library/Developer/Xcode/DerivedData"),
            ),
            ("Simulators", home.join("Library/Developer/CoreSimulator")),
        ],
    ));

    categories.push(build_category(
        "Homebrew",
        vec![
            ("Installation", PathBuf::from("/opt/homebrew")),
            ("Cache", home.join("Library/Caches/Homebrew")),
        ],
    ));

    categories.push(build_category(
        "App Caches",
        vec![
            ("Chrome", home.join("Library/Caches/Google")),
            (
                "Slack",
                home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt"),
            ),
            ("Playwright", home.join("Library/Caches/ms-playwright")),
        ],
    ));

    categories.push(build_category(
        "Downloads",
        vec![("~/Downloads", home.join("Downloads"))],
    ));

    categories.sort_by(|a, b| b.total_size.cmp(&a.total_size));
    categories.retain(|c| c.total_size > 0);
    categories
}

fn build_category(name: &str, paths: Vec<(&str, PathBuf)>) -> Category {
    let mut cat_paths = Vec::new();
    let mut total_size = 0u64;
    for (label, path) in paths {
        let size = dir_size(&path);
        if size > 0 {
            total_size += size;
            cat_paths.push(CategoryPath {
                label: label.to_string(),
                path,
                size,
            });
        }
    }
    Category {
        name: name.to_string(),
        paths: cat_paths,
        total_size,
    }
}

// === Cleanup Targets (for TUI) ===

pub struct CleanupTarget {
    pub name: String,
    pub description: String,
    /// Estimated reclaimable bytes (0 means unknown/not scannable)
    pub size: u64,
    action: CleanAction,
}

enum CleanAction {
    RemoveContents(PathBuf),
    RunCommand(String, Vec<String>),
    RemoveByExtension(PathBuf, String),
}

impl CleanupTarget {
    pub fn clean(&self) -> Result<u64, String> {
        match &self.action {
            CleanAction::RemoveContents(path) => {
                let size = dir_size(path);
                if path.exists() {
                    for entry in fs::read_dir(path).map_err(|e| e.to_string())?.flatten() {
                        let p = entry.path();
                        if p.is_dir() {
                            fs::remove_dir_all(&p)
                                .map_err(|e| format!("{}: {}", p.display(), e))?;
                        } else {
                            fs::remove_file(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
                        }
                    }
                }
                Ok(size)
            }
            CleanAction::RunCommand(cmd, args) => {
                let size_before = self.size;
                let output = Command::new(cmd)
                    .args(args)
                    .output()
                    .map_err(|e| format!("{cmd}: {e}"))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("{cmd} failed: {stderr}"));
                }
                Ok(size_before)
            }
            CleanAction::RemoveByExtension(dir, ext) => {
                let mut freed = 0u64;
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|e| e == ext.as_str()) {
                            if let Ok(meta) = path.metadata() {
                                freed += meta.len();
                                fs::remove_file(&path)
                                    .map_err(|e| format!("{}: {}", path.display(), e))?;
                            }
                        }
                    }
                }
                Ok(freed)
            }
        }
    }
}

/// Discover all cleanup targets and scan their sizes.
pub fn discover_cleanup_targets() -> Vec<CleanupTarget> {
    let home = dirs::home_dir().expect("could not determine home directory");
    let mut targets = Vec::new();

    // Homebrew cache
    let brew_cache = home.join("Library/Caches/Homebrew");
    targets.push(CleanupTarget {
        name: "Homebrew cache".into(),
        description: "Old bottles and stale downloads".into(),
        size: dir_size(&brew_cache),
        action: CleanAction::RunCommand(
            "brew".into(),
            vec!["cleanup".into(), "--prune=all".into()],
        ),
    });

    // Go build cache
    let go_cache = go_cache_dir().unwrap_or_else(|| home.join("Library/Caches/go-build"));
    targets.push(CleanupTarget {
        name: "Go build cache".into(),
        description: "Compiled build artifacts".into(),
        size: dir_size(&go_cache),
        action: CleanAction::RunCommand("go".into(), vec!["clean".into(), "-cache".into()]),
    });

    // npm cache
    let npm_cache = home.join(".npm/_cacache");
    targets.push(CleanupTarget {
        name: "npm cache".into(),
        description: "Package download cache".into(),
        size: dir_size(&npm_cache),
        action: CleanAction::RunCommand(
            "npm".into(),
            vec!["cache".into(), "clean".into(), "--force".into()],
        ),
    });

    // pnpm store
    targets.push(CleanupTarget {
        name: "pnpm store (unreferenced)".into(),
        description: "Unreferenced packages in pnpm store".into(),
        size: 0,
        action: CleanAction::RunCommand("pnpm".into(), vec!["store".into(), "prune".into()]),
    });

    // Playwright browsers
    let playwright = home.join("Library/Caches/ms-playwright");
    targets.push(CleanupTarget {
        name: "Playwright browsers".into(),
        description: "Cached browser binaries for testing".into(),
        size: dir_size(&playwright),
        action: CleanAction::RemoveContents(playwright),
    });

    // Chrome cache
    let chrome_cache = home.join("Library/Caches/Google");
    targets.push(CleanupTarget {
        name: "Chrome cache".into(),
        description: "Google Chrome browser cache".into(),
        size: dir_size(&chrome_cache),
        action: CleanAction::RemoveContents(chrome_cache),
    });

    // Slack update cache
    let slack_cache = home.join("Library/Caches/com.tinyspeck.slackmacgap.ShipIt");
    targets.push(CleanupTarget {
        name: "Slack update cache".into(),
        description: "Slack auto-update downloads".into(),
        size: dir_size(&slack_cache),
        action: CleanAction::RemoveContents(slack_cache),
    });

    // Xcode DerivedData
    let derived_data = home.join("Library/Developer/Xcode/DerivedData");
    targets.push(CleanupTarget {
        name: "Xcode DerivedData".into(),
        description: "Build artifacts from Xcode projects".into(),
        size: dir_size(&derived_data),
        action: CleanAction::RemoveContents(derived_data),
    });

    // Xcode stale simulators
    if command_exists("xcrun") {
        targets.push(CleanupTarget {
            name: "Xcode stale simulators".into(),
            description: "Unavailable simulator runtimes".into(),
            size: 0,
            action: CleanAction::RunCommand(
                "xcrun".into(),
                vec!["simctl".into(), "delete".into(), "unavailable".into()],
            ),
        });
    }

    // Docker unused data
    if command_exists("docker") {
        targets.push(CleanupTarget {
            name: "Docker unused data".into(),
            description: "Dangling images, stopped containers, unused networks".into(),
            size: 0,
            action: CleanAction::RunCommand(
                "docker".into(),
                vec!["system".into(), "prune".into(), "-f".into()],
            ),
        });
    }

    // DMGs in Downloads
    let downloads = home.join("Downloads");
    targets.push(CleanupTarget {
        name: "DMG installers".into(),
        description: "Downloaded .dmg files in ~/Downloads".into(),
        size: files_by_extension_size(&downloads, "dmg"),
        action: CleanAction::RemoveByExtension(downloads, "dmg".into()),
    });

    targets
}

// === Shared Helpers ===

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    dir_size_recursive(path)
}

fn dir_size_recursive(path: &Path) -> u64 {
    let mut size = 0u64;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            size += dir_size_recursive(&path);
        } else if let Ok(meta) = path.metadata() {
            size += meta.len();
        }
    }
    size
}

fn files_by_extension_size(dir: &Path, ext: &str) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == ext) {
                if let Ok(meta) = path.metadata() {
                    size += meta.len();
                }
            }
        }
    }
    size
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
