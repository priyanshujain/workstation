use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The app bundle the launchd jobs run from.
///
/// Login Items names a background job after the app bundle its executable
/// lives in, and only when that bundle carries a code signature sealing its
/// Info.plist. A bare binary, or an unsigned bundle, is listed as `wsctl`
/// with no icon. So each agent installer copies the running binary into
/// `~/Applications/Workstation.app`, signs it ad hoc, and points launchd at
/// the copy. The copy is a different file from the one on PATH, so the bundle
/// records which build it came from and `is_current` reports when a reinstall
/// has left it behind.
pub const BUNDLE_ID: &str = "dev.pj.workstation";
pub const NAME: &str = "Workstation";

const ICON: &[u8] = include_bytes!("../assets/Workstation.icns");
const STAMP: &str = "Contents/Resources/source";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

pub fn app_path() -> PathBuf {
    home().join("Applications").join(format!("{NAME}.app"))
}

/// The copy of wsctl that launchd runs, named after the app as bundles
/// conventionally are.
pub fn executable() -> PathBuf {
    executable_in(&app_path())
}

/// Copy `exe` into the bundle, sign it, and return the bundled path.
/// Safe to call while a job is mid-run: the new bundle is built beside the
/// old one and swapped in with a rename, so a running copy keeps its file.
pub fn install(exe: &Path) -> Result<PathBuf> {
    let app = app_path();
    let target = install_in(exe, &app)?;
    // Only the real install registers with LaunchServices. A registration
    // outlives its directory, and stale ones under the same bundle id make
    // LaunchServices resolve the id to paths that no longer exist, which is
    // how bundles built by the test suite left Login Items showing `wsctl`.
    let _ = Command::new(LSREGISTER).arg("-f").arg(&app).output();
    Ok(target)
}

/// Whether the bundled copy was made from this build of `exe`.
pub fn is_current(exe: &Path) -> bool {
    is_current_in(exe, &app_path())
}

/// Delete the bundle once no launchd job points into it. Both agents share
/// it, so each uninstall asks rather than removing outright.
pub fn remove_if_unused() -> Result<()> {
    remove_if_unused_in(&app_path(), &home().join("Library/LaunchAgents"))
}

fn install_in(exe: &Path, app: &Path) -> Result<PathBuf> {
    let target = executable_in(app);
    if is_inside(exe, app) {
        return Ok(target);
    }

    let parent = app
        .parent()
        .context("bundle path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    let staging = parent.join(format!(".{NAME}.app.staging"));
    let _ = std::fs::remove_dir_all(&staging);
    build(exe, &staging)?;
    sign(&staging)?;
    swap(&staging, app)?;
    Ok(target)
}

fn build(exe: &Path, app: &Path) -> Result<()> {
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    for dir in [&macos, &resources] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::copy(exe, macos.join(NAME))
        .with_context(|| format!("failed to copy {}", exe.display()))?;
    std::fs::write(resources.join(format!("{NAME}.icns")), ICON)?;
    std::fs::write(app.join(STAMP), stamp(exe)?)?;
    std::fs::write(app.join("Contents/Info.plist"), info_plist())?;
    Ok(())
}

fn sign(app: &Path) -> Result<()> {
    let out = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .output()
        .context("failed to run codesign")?;
    if !out.status.success() {
        bail!(
            "codesign failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn swap(staging: &Path, app: &Path) -> Result<()> {
    let old = staging.with_extension("old");
    let _ = std::fs::remove_dir_all(&old);
    if app.exists() {
        std::fs::rename(app, &old)
            .with_context(|| format!("failed to move aside {}", app.display()))?;
    }
    if let Err(e) = std::fs::rename(staging, app) {
        let _ = std::fs::rename(&old, app);
        return Err(e).with_context(|| format!("failed to install {}", app.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn is_current_in(exe: &Path, app: &Path) -> bool {
    if !executable_in(app).exists() {
        return false;
    }
    if is_inside(exe, app) {
        return true;
    }
    let Ok(expected) = stamp(exe) else {
        return false;
    };
    std::fs::read_to_string(app.join(STAMP)).is_ok_and(|s| s == expected)
}

fn remove_if_unused_in(app: &Path, agents: &Path) -> Result<()> {
    if !app.exists() {
        return Ok(());
    }
    let needle = xml_escape(&app.to_string_lossy());
    let used = std::fs::read_dir(agents)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "plist"))
        .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains(&needle)));
    if used {
        return Ok(());
    }
    let _ = Command::new(LSREGISTER).arg("-u").arg(app).output();
    std::fs::remove_dir_all(app).with_context(|| format!("failed to remove {}", app.display()))
}

fn executable_in(app: &Path) -> PathBuf {
    app.join("Contents/MacOS").join(NAME)
}

fn is_inside(exe: &Path, app: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(exe).starts_with(canon(app))
}

/// Size and mtime of the source binary: enough to notice a reinstall, and
/// cheaper than hashing six megabytes on every `wsctl apply`.
fn stamp(exe: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(exe).with_context(|| format!("failed to stat {}", exe.display()))?;
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}:{}", meta.len(), modified))
}

fn info_plist() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>{BUNDLE_ID}</string>
    <key>CFBundleName</key>
    <string>{NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>{NAME}</string>
    <key>CFBundleExecutable</key>
    <string>{NAME}</string>
    <key>CFBundleIconFile</key>
    <string>{NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
"#
    )
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real Mach-O to bundle: codesign refuses anything else as a main
    /// executable, and the test binary is the one we know is here.
    fn source(dir: &Path) -> PathBuf {
        let src = dir.join("wsctl");
        std::fs::copy(std::env::current_exe().unwrap(), &src).unwrap();
        src
    }

    fn signed(app: &Path) -> bool {
        Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(app)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn install_builds_a_signed_bundle_around_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());

        let bundled = install_in(&exe, &app).unwrap();

        assert_eq!(bundled, app.join("Contents/MacOS/Workstation"));
        assert!(bundled.exists());
        assert!(app.join("Contents/Info.plist").exists());
        assert!(app.join("Contents/Resources/Workstation.icns").exists());
        assert!(
            signed(&app),
            "Login Items ignores the bundle unless it is signed"
        );
        assert!(!dir.path().join(".Workstation.app.staging").exists());
        assert!(is_current_in(&exe, &app));
    }

    #[test]
    fn install_replaces_an_existing_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_in(&exe, &app).unwrap();
        std::fs::write(app.join("Contents/Resources/leftover"), b"x").unwrap();

        install_in(&exe, &app).unwrap();

        assert!(!app.join("Contents/Resources/leftover").exists());
        assert!(signed(&app));
        assert!(!dir.path().join(".Workstation.app.old").exists());
    }

    #[test]
    fn a_reinstalled_binary_makes_the_bundle_stale() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_in(&exe, &app).unwrap();

        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::open(&exe)
            .unwrap()
            .set_modified(later)
            .unwrap();

        assert!(!is_current_in(&exe, &app));
    }

    #[test]
    fn a_missing_bundle_is_never_current() {
        let dir = tempfile::tempdir().unwrap();
        let exe = source(dir.path());
        assert!(!is_current_in(&exe, &dir.path().join("Workstation.app")));
    }

    #[test]
    fn running_from_inside_the_bundle_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let inside = app.join("Contents/MacOS/Workstation");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, b"placeholder").unwrap();

        assert_eq!(install_in(&inside, &app).unwrap(), inside);
        assert!(!app.join("Contents/Info.plist").exists());
        assert!(is_current_in(&inside, &app));
    }

    #[test]
    fn the_bundle_stays_while_a_job_points_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let agents = dir.path().join("LaunchAgents");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("job.plist"),
            format!(
                "<string>{}/Contents/MacOS/Workstation</string>",
                app.display()
            ),
        )
        .unwrap();

        remove_if_unused_in(&app, &agents).unwrap();
        assert!(app.exists());

        std::fs::remove_file(agents.join("job.plist")).unwrap();
        remove_if_unused_in(&app, &agents).unwrap();
        assert!(!app.exists());
    }

    #[test]
    fn removing_an_absent_bundle_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        remove_if_unused_in(&dir.path().join("nope.app"), dir.path()).unwrap();
    }

    #[test]
    fn info_plist_is_well_formed_and_names_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Info.plist");
        std::fs::write(&path, info_plist()).unwrap();
        let out = Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );

        let plist = info_plist();
        assert!(plist.contains("<string>Workstation</string>"));
        assert!(plist.contains("<string>dev.pj.workstation</string>"));
        assert!(plist.contains("<key>CFBundleIconFile</key>\n    <string>Workstation</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key>\n    <string>Workstation</string>"));
        assert!(
            plist.contains("<key>LSUIElement</key>\n    <true/>"),
            "a Dock icon would flash on every run"
        );
    }

    #[test]
    fn paths_land_in_the_user_applications_folder() {
        assert!(app_path().ends_with("Applications/Workstation.app"));
        assert!(executable().ends_with("Workstation.app/Contents/MacOS/Workstation"));
    }
}
