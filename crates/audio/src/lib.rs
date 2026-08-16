//! Installs the virtual audio devices that carry audio to and from the bridge.
//!
//! The drivers are Core Audio HAL plug-ins, which `coreaudiod` only loads from
//! /Library/Audio/Plug-Ins/HAL. They cannot run inside this process, so the
//! bundles are embedded here and written out to that directory on install.

mod aec;
pub mod bridge;
pub mod calibrate;
pub mod correction;
mod denoise;
pub mod device;
mod probe;
mod resample;
mod ring;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const DRIVERS_TGZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/drivers.tar.gz"));
const INSTALL_SH: &str = include_str!("../../../audio/driver/install.sh");
const UNINSTALL_SH: &str = include_str!("../../../audio/driver/uninstall.sh");

const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";
const BUNDLES: [&str; 2] = ["WSSpeaker", "WSMicrophone"];

/// The four devices the two plug-ins publish, in the order a signal meets them.
pub const DEVICES: [&str; 4] = [
    "Workstation Speaker",
    "Workstation Speaker Tap",
    "Workstation Mic Feed",
    "Workstation Mic",
];

pub struct Status {
    pub installed: Vec<String>,
    pub missing: Vec<String>,
    pub live_devices: Vec<String>,
}

impl Status {
    pub fn is_installed(&self) -> bool {
        self.missing.is_empty()
    }

    /// Installed on disk but not published by coreaudiod, which normally means
    /// the plug-in was rejected. `log stream --predicate 'eventMessage
    /// CONTAINS "plug-in named"'` shows why.
    pub fn is_loaded(&self) -> bool {
        DEVICES
            .iter()
            .all(|d| self.live_devices.iter().any(|l| l == d))
    }
}

pub fn status() -> Result<Status> {
    let (mut installed, mut missing) = (Vec::new(), Vec::new());
    for b in BUNDLES {
        let p = PathBuf::from(HAL_DIR).join(format!("{b}.driver"));
        if p.is_dir() {
            installed.push(b.to_string());
        } else {
            missing.push(b.to_string());
        }
    }
    Ok(Status {
        installed,
        missing,
        live_devices: live_devices()?,
    })
}

/// Device names Core Audio is currently publishing that belong to us.
fn live_devices() -> Result<Vec<String>> {
    let out = Command::new("system_profiler")
        .arg("SPAudioDataType")
        .output()
        .context("could not run system_profiler")?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(DEVICES
        .iter()
        .filter(|d| text.contains(&format!("{d}:")))
        .map(|d| d.to_string())
        .collect())
}

pub fn install() -> Result<()> {
    let staged = stage()?;
    let script = staged.join("install.sh");
    write_script(&script, INSTALL_SH)?;
    sudo(&script, Some(&staged))
}

pub fn uninstall() -> Result<()> {
    let staged = stage()?;
    let script = staged.join("uninstall.sh");
    write_script(&script, UNINSTALL_SH)?;
    sudo(&script, None)
}

/// Unpacks the embedded bundles into a scratch directory as the current user.
/// Only the copy into /Library needs root, so the extraction stays unprivileged.
fn stage() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("wsctl-audio");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;

    let tgz = dir.join("drivers.tar.gz");
    std::fs::write(&tgz, DRIVERS_TGZ)?;

    let status = Command::new("tar")
        .arg("xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(&dir)
        .status()
        .context("could not run tar")?;
    if !status.success() {
        bail!("could not unpack the embedded drivers");
    }
    Ok(dir)
}

fn write_script(path: &Path, body: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn sudo(script: &Path, arg: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("sudo");
    cmd.arg(script);
    if let Some(a) = arg {
        cmd.arg(a);
    }
    let status = cmd.status().context("could not run sudo")?;
    if !status.success() {
        bail!("{} exited with {status}", script.display());
    }
    Ok(())
}
