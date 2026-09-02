use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSError, NSString};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// The app bundle the launchd jobs live in and run from.
///
/// Login Items names a background job after its app and draws the app's icon
/// only when the job is registered through the Service Management API from
/// inside that app: the plist sits in `Contents/Library/LaunchAgents` and the
/// bundled copy of wsctl asks `SMAppService` to register it. A plist in
/// `~/Library/LaunchAgents` pointing at a signed bundle gets the bundle's
/// name but the executable's generic icon, and a bare binary is listed as
/// `wsctl`. So each agent installer rebuilds `~/Applications/Workstation.app`
/// from the running binary, the icon and every enabled job's plist, signs it
/// ad hoc, and registers the jobs from the copy inside.
///
/// A registration pins the signature of the bundle it was made against, and
/// every rebuild changes that signature. Background Task Management keeps
/// its record per label, so a label registered once never takes a new
/// signature: unregistering, waiting, removing the plist and registering
/// again all left the job dying at spawn with OS_REASON_CODESIGNING, while a
/// label it had never seen worked every time. So a job's label carries the
/// build it belongs to, `dev.pj.workstation.display.<millis>`, and a rebuild
/// unregisters every old label and registers new ones.
pub const BUNDLE_ID: &str = "dev.pj.workstation";
pub const NAME: &str = "Workstation";

const ICON: &[u8] = include_bytes!("../assets/Workstation.icns");
const STAMP: &str = "Contents/Resources/source";
const AGENTS: &str = "Contents/Library/LaunchAgents";
const AD_HOC: &str = "-";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// `SMAppServiceStatus` values: not registered, held until the user allows it
/// in Login Items, and registered but the plist is gone.
const NOT_REGISTERED: isize = 0;
const REQUIRES_APPROVAL: isize = 2;
const NOT_FOUND: isize = 3;

/// Produces a job's plist for the tagged label it is given.
type PlistFor<'a> = &'a dyn Fn(&str) -> String;

/// What `install_agent` left behind.
pub struct Installed {
    pub plist: PathBuf,
    /// False when macOS is holding the job until it is allowed in Login Items.
    pub approved: bool,
    /// What the bundle is signed as: a Developer ID, or `ad hoc`.
    pub signed_as: String,
}

/// The codesign identity to sign the bundle with. A Developer ID Application
/// identity in the keychain is used when there is exactly one, which is what
/// puts a developer name under the entry in Login Items instead of
/// "unidentified developer"; otherwise the bundle is signed ad hoc, which
/// works everywhere else. Two candidates are ambiguous, and picking one would
/// sign with a team that may not be the user's, so that falls back too.
pub fn signing_identity() -> String {
    let out = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output();
    let Ok(out) = out else {
        return AD_HOC.to_string();
    };
    let mut found: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("\"Developer ID Application:"))
        .filter_map(|l| {
            let start = l.find('"')? + 1;
            let end = l.rfind('"')?;
            (start < end).then(|| l[start..end].to_string())
        })
        .collect();
    found.sort();
    found.dedup();
    match found.as_slice() {
        [one] => one.clone(),
        [] => AD_HOC.to_string(),
        many => {
            tracing::warn!(
                "{} Developer ID identities in the keychain, signing ad hoc",
                many.len()
            );
            AD_HOC.to_string()
        }
    }
}

pub fn app_path() -> PathBuf {
    home().join("Applications").join(format!("{NAME}.app"))
}

/// The copy of wsctl that launchd runs, named after the app as bundles
/// conventionally are.
pub fn executable() -> PathBuf {
    executable_in(&app_path())
}

/// The tagged label the bundle currently carries for a job, if any.
pub fn agent_label(base: &str) -> Option<String> {
    agent_label_in(&app_path(), base)
}

pub fn has_agent(base: &str) -> bool {
    agent_label(base).is_some()
}

/// Put this job's plist in the bundle next to a copy of `exe`, keeping every
/// other job, and make sure every job is registered. `plist` is given the
/// tagged label to put in its `Label` key.
///
/// A rebuild unregisters every job first and registers them all under new
/// labels after, for the reason given above; when nothing changed the bundle
/// is left alone. Safe while a job is mid-run: the new contents are built
/// beside the old and swapped in with a rename, so a running copy keeps its
/// file.
pub fn install_agent(exe: &Path, base: &str, plist: PlistFor) -> Result<Installed> {
    let app = app_path();
    let unchanged = is_current_in(exe, &app)
        && agent_label_in(&app, base).is_some_and(|label| {
            std::fs::read_to_string(agent_plist_in(&app, &label))
                .is_ok_and(|current| current == plist(&label))
        });
    let identity = signing_identity();
    if !unchanged {
        unregister_all(&app);
        install_agent_in(exe, &app, base, plist, &identity)?;
        // Only the real install registers with LaunchServices. A registration
        // outlives its directory, and stale ones under the same bundle id make
        // LaunchServices resolve the id to paths that no longer exist, which is
        // how bundles built by the test suite once left Login Items showing `wsctl`.
        let _ = Command::new(LSREGISTER).arg("-f").arg(&app).output();
    }
    let approved = register_all(&app)?;
    let label = agent_label_in(&app, base).context("the job did not land in the bundle")?;
    Ok(Installed {
        plist: agent_plist_in(&app, &label),
        approved,
        signed_as: if identity == AD_HOC {
            "ad hoc".to_string()
        } else {
            identity
        },
    })
}

/// Unregister this job and rebuild the bundle without it, or delete the
/// bundle when it was the last one. The other jobs come back under new
/// labels, for the reason given above.
pub fn remove_agent(base: &str) -> Result<()> {
    let app = app_path();
    if agent_label_in(&app, base).is_none() {
        return Ok(());
    }
    if agents_in(&app).iter().all(|l| base_of(l) == base) {
        return remove();
    }
    unregister_all(&app);
    build_and_swap(
        &executable_in(&app),
        &app,
        None,
        Some(base),
        &signing_identity(),
    )?;
    register_all(&app)?;
    Ok(())
}

/// Unregister every job and delete the bundle.
pub fn remove() -> Result<()> {
    let app = app_path();
    if !app.exists() {
        return Ok(());
    }
    unregister_all(&app);
    let _ = Command::new(LSREGISTER).arg("-u").arg(&app).output();
    std::fs::remove_dir_all(&app).with_context(|| format!("failed to remove {}", app.display()))
}

/// Whether the bundled copy was made from this build of `exe`.
pub fn is_current(exe: &Path) -> bool {
    is_current_in(exe, &app_path())
}

/// Boot out jobs that older versions loaded from `~/Library/LaunchAgents` and
/// delete their plists. Only labels with a plist there are touched, so a job
/// registered from the bundle under the same label is left alone. Waits until
/// each is gone: bootout tears a running job down asynchronously, and a
/// registration that lands before that finishes fails.
pub fn retire_launch_agents(labels: &[&str]) {
    let Ok(uid) = uid() else {
        return;
    };
    let dir = home().join("Library/LaunchAgents");
    for label in labels {
        let plist = dir.join(format!("{label}.plist"));
        if !plist.exists() {
            continue;
        }
        for domain in [format!("gui/{uid}"), format!("user/{uid}")] {
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("{domain}/{label}")])
                .output();
        }
        wait_until_unloaded(label);
        let _ = std::fs::remove_file(&plist);
    }
}

/// Register a job of the app this process runs from. Only meaningful in the
/// bundled copy: the Service Management API registers jobs of the caller's
/// own bundle, which is why `install_agent` runs the copy inside the bundle
/// to call this. Prints `requires approval` when macOS is holding the job
/// for the user to allow.
pub fn register_here(label: &str) -> Result<()> {
    let service = service(label);
    let result: Result<(), Retained<NSError>> =
        unsafe { msg_send![&*service, registerAndReturnError: _] };
    result.map_err(|e| anyhow!("{}", e.localizedDescription()))?;
    let status: isize = unsafe { msg_send![&*service, status] };
    if status == REQUIRES_APPROVAL {
        println!("requires approval");
    }
    Ok(())
}

pub fn unregister_here(label: &str) -> Result<()> {
    let service = service(label);
    let result: Result<(), Retained<NSError>> =
        unsafe { msg_send![&*service, unregisterAndReturnError: _] };
    result.map_err(|e| anyhow!("{}", e.localizedDescription()))
}

/// The job's `SMAppServiceStatus`, as the bundled copy sees it.
pub fn status_here(label: &str) -> isize {
    let service = service(label);
    unsafe { msg_send![&*service, status] }
}

fn service(label: &str) -> Retained<AnyObject> {
    let name = NSString::from_str(&format!("{label}.plist"));
    unsafe { msg_send![class!(SMAppService), agentServiceWithPlistName: &*name] }
}

/// Register every job through the bundled copy, so the registrations are
/// attributed to the app. Returns whether they are all allowed to run right
/// away.
fn register_all(app: &Path) -> Result<bool> {
    let exe = executable_in(app);
    let mut approved = true;
    for label in agents_in(app) {
        approved &= register(&exe, &label)?;
    }
    Ok(approved)
}

fn register(exe: &Path, label: &str) -> Result<bool> {
    let out = Command::new(exe)
        .args(["self", "register-agent", label])
        .output()
        .with_context(|| format!("failed to run {}", exe.display()))?;
    if !out.status.success() {
        bail!(
            "could not register {label}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(!String::from_utf8_lossy(&out.stdout).contains("requires approval"))
}

/// Unregister every job the bundle carries. A job that was never registered,
/// or whose copy is gone, is already in the state we want.
fn unregister_all(app: &Path) {
    let exe = executable_in(app);
    if !exe.exists() {
        return;
    }
    for label in agents_in(app) {
        unregister(&exe, &label);
    }
}

/// Unregister one job and wait until Background Task Management reports it
/// gone and launchd has unloaded it, so the old and new copies of a job
/// never run side by side. Failure is logged rather than returned.
fn unregister(exe: &Path, label: &str) {
    match Command::new(exe)
        .args(["self", "unregister-agent", label])
        .output()
    {
        Ok(out) if !out.status.success() => tracing::warn!(
            "could not unregister {label}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run {}: {e}", exe.display()),
        Ok(_) => {}
    }
    for _ in 0..40 {
        if matches!(status(exe, label), None | Some(NOT_REGISTERED | NOT_FOUND)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    wait_until_unloaded(label);
}

fn status(exe: &Path, label: &str) -> Option<isize> {
    let out = Command::new(exe)
        .args(["self", "agent-status", label])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn wait_until_unloaded(label: &str) {
    let Ok(uid) = uid() else {
        return;
    };
    let target = format!("gui/{uid}/{label}");
    for _ in 0..20 {
        let gone = Command::new("launchctl")
            .args(["print", &target])
            .output()
            .is_ok_and(|o| !o.status.success());
        if gone {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn install_agent_in(
    exe: &Path,
    app: &Path,
    base: &str,
    plist: PlistFor,
    identity: &str,
) -> Result<()> {
    let source = if is_inside(exe, app) {
        executable_in(app)
    } else {
        exe.to_path_buf()
    };
    build_and_swap(&source, app, Some((base, plist)), None, identity)
}

fn build_and_swap(
    source: &Path,
    app: &Path,
    add: Option<(&str, PlistFor)>,
    drop: Option<&str>,
    identity: &str,
) -> Result<()> {
    let parent = app
        .parent()
        .context("bundle path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let staging = parent.join(format!(".{NAME}.app.staging"));
    let _ = std::fs::remove_dir_all(&staging);
    build(source, app, &staging, add, drop)?;
    sign(&staging, identity)?;
    swap(&staging, app)
}

/// Lay out a fresh bundle in `staging` from `source` and the jobs `app`
/// already carries, minus `drop`, plus `add`, every job under a label tagged
/// with this build.
fn build(
    source: &Path,
    app: &Path,
    staging: &Path,
    add: Option<(&str, PlistFor)>,
    drop: Option<&str>,
) -> Result<()> {
    let macos = staging.join("Contents/MacOS");
    let resources = staging.join("Contents/Resources");
    let agents = staging.join(AGENTS);
    for dir in [&macos, &resources, &agents] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::copy(source, macos.join(NAME))
        .with_context(|| format!("failed to copy {}", source.display()))?;
    std::fs::write(resources.join(format!("{NAME}.icns")), ICON)?;
    let stamp = if is_inside(source, app) {
        std::fs::read_to_string(app.join(STAMP)).unwrap_or_default()
    } else {
        stamp(source)?
    };
    std::fs::write(staging.join(STAMP), stamp)?;
    std::fs::write(staging.join("Contents/Info.plist"), info_plist())?;
    let tag = new_tag();
    for old in agents_in(app) {
        let base = base_of(&old);
        if Some(base) == drop || add.is_some_and(|(b, _)| b == base) {
            continue;
        }
        let new = format!("{base}.{tag}");
        let text = std::fs::read_to_string(agent_plist_in(app, &old))?.replace(&old, &new);
        std::fs::write(agent_plist_in(staging, &new), text)?;
    }
    if let Some((base, plist)) = add {
        let label = format!("{base}.{tag}");
        std::fs::write(agent_plist_in(staging, &label), plist(&label))?;
    }
    Ok(())
}

/// Milliseconds since the epoch: unique per build, and it starts with a
/// digit, which no segment of a base label does.
fn new_tag() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string()
}

/// The label without its build tag. A label that was never tagged, from an
/// older install, is its own base.
fn base_of(label: &str) -> &str {
    match label.rsplit_once('.') {
        Some((base, tag)) if !tag.is_empty() && tag.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => label,
    }
}

fn agent_label_in(app: &Path, base: &str) -> Option<String> {
    agents_in(app).into_iter().find(|l| base_of(l) == base)
}

/// No timestamp: that needs Apple's timestamp server, and the bundle only
/// ever runs on this machine.
fn sign(app: &Path, identity: &str) -> Result<()> {
    let out = Command::new("codesign")
        .args(["--force", "--timestamp=none", "--sign", identity])
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

/// Swap `Contents` rather than the whole `.app`, so the app directory itself
/// keeps its identity for anything holding a reference to it.
fn swap(staging: &Path, app: &Path) -> Result<()> {
    std::fs::create_dir_all(app).with_context(|| format!("failed to create {}", app.display()))?;
    let current = app.join("Contents");
    let old = staging.join("Contents.old");
    if current.exists() {
        std::fs::rename(&current, &old)
            .with_context(|| format!("failed to move aside {}", current.display()))?;
    }
    if let Err(e) = std::fs::rename(staging.join("Contents"), &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(e).with_context(|| format!("failed to install {}", app.display()));
    }
    let _ = std::fs::remove_dir_all(staging);
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

/// Labels of the jobs the bundle carries, from their plist names.
fn agents_in(app: &Path) -> Vec<String> {
    let mut labels: Vec<String> = std::fs::read_dir(app.join(AGENTS))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "plist"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    labels.sort();
    labels
}

fn executable_in(app: &Path) -> PathBuf {
    app.join("Contents/MacOS").join(NAME)
}

fn agent_plist_in(app: &Path, label: &str) -> PathBuf {
    app.join(AGENTS).join(format!("{label}.plist"))
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

fn uid() -> Result<String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to determine current uid")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        bail!("could not determine current uid");
    }
    Ok(uid)
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

    fn plist(label: &str) -> String {
        format!("<plist><key>Label</key><string>{label}</string></plist>")
    }

    fn plist_of(app: &Path, label: &str) -> String {
        std::fs::read_to_string(agent_plist_in(app, label)).unwrap()
    }

    fn tag_of(label: &str) -> &str {
        label.rsplit_once('.').unwrap().1
    }

    #[test]
    fn install_builds_a_signed_bundle_with_the_job_inside() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());

        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();

        let label = agent_label_in(&app, "dev.pj.test.one").unwrap();
        assert!(label.starts_with("dev.pj.test.one."), "{label}");
        assert_eq!(base_of(&label), "dev.pj.test.one");
        assert_eq!(plist_of(&app, &label), plist(&label));
        assert!(app.join("Contents/MacOS/Workstation").exists());
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
    fn a_rebuild_gives_every_job_a_new_label_from_the_same_build() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();
        let first = agent_label_in(&app, "dev.pj.test.one").unwrap();

        std::thread::sleep(Duration::from_millis(2));
        install_agent_in(&exe, &app, "dev.pj.test.two", &plist, AD_HOC).unwrap();

        let one = agent_label_in(&app, "dev.pj.test.one").unwrap();
        let two = agent_label_in(&app, "dev.pj.test.two").unwrap();
        assert_ne!(one, first, "a registration pins the old signature");
        assert_eq!(tag_of(&one), tag_of(&two));
        assert_eq!(plist_of(&app, &one), plist(&one), "the Label key follows");
        assert_eq!(agents_in(&app).len(), 2);
        assert!(signed(&app));
        assert!(is_current_in(&exe, &app));
    }

    #[test]
    fn a_reinstall_replaces_the_jobs_plist() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();

        let other = |label: &str| format!("<other>{label}</other>");
        install_agent_in(&exe, &app, "dev.pj.test.one", &other, AD_HOC).unwrap();

        let label = agent_label_in(&app, "dev.pj.test.one").unwrap();
        assert_eq!(plist_of(&app, &label), other(&label));
        assert_eq!(agents_in(&app).len(), 1);
    }

    #[test]
    fn dropping_a_job_keeps_the_others_and_the_build_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();
        install_agent_in(&exe, &app, "dev.pj.test.two", &plist, AD_HOC).unwrap();

        // What `remove_agent` does once the jobs are unregistered: rebuild from the copy.
        build_and_swap(
            &executable_in(&app),
            &app,
            None,
            Some("dev.pj.test.one"),
            AD_HOC,
        )
        .unwrap();

        let bases: Vec<&str> = agents_in(&app)
            .iter()
            .map(|l| base_of(l))
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(bases, ["dev.pj.test.two"]);
        assert!(signed(&app));
        assert!(
            is_current_in(&exe, &app),
            "the stamp must survive a rebuild from the copy"
        );
    }

    #[test]
    fn an_untagged_label_from_an_older_install_gets_tagged() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();
        std::fs::write(
            agent_plist_in(&app, "dev.pj.test.legacy"),
            plist("dev.pj.test.legacy"),
        )
        .unwrap();

        build_and_swap(&executable_in(&app), &app, None, None, AD_HOC).unwrap();

        let label = agent_label_in(&app, "dev.pj.test.legacy").unwrap();
        assert_ne!(label, "dev.pj.test.legacy");
        assert_eq!(plist_of(&app, &label), plist(&label));
        assert!(!agent_plist_in(&app, "dev.pj.test.legacy").exists());
    }

    #[test]
    fn base_of_strips_only_a_numeric_tag() {
        assert_eq!(
            base_of("dev.pj.workstation.display.1756850700123"),
            "dev.pj.workstation.display"
        );
        assert_eq!(
            base_of("dev.pj.workstation.display"),
            "dev.pj.workstation.display"
        );
        assert_eq!(base_of("dev.pj.workstation.t1"), "dev.pj.workstation.t1");
        assert!(new_tag().bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn a_reinstalled_binary_makes_the_bundle_stale() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();

        let later = std::time::SystemTime::now() + Duration::from_secs(60);
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
    fn running_from_inside_the_bundle_rebuilds_from_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Workstation.app");
        let exe = source(dir.path());
        install_agent_in(&exe, &app, "dev.pj.test.one", &plist, AD_HOC).unwrap();

        let inside = executable_in(&app);
        install_agent_in(&inside, &app, "dev.pj.test.two", &plist, AD_HOC).unwrap();

        assert_eq!(agents_in(&app).len(), 2);
        assert!(signed(&app));
        assert!(is_current_in(&exe, &app));
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
    fn signing_identity_is_a_developer_id_or_ad_hoc() {
        let identity = signing_identity();
        assert!(
            identity == AD_HOC || identity.starts_with("Developer ID Application:"),
            "{identity}"
        );
    }

    #[test]
    fn paths_land_in_the_user_applications_folder() {
        assert!(app_path().ends_with("Applications/Workstation.app"));
        assert!(executable().ends_with("Workstation.app/Contents/MacOS/Workstation"));
    }
}
