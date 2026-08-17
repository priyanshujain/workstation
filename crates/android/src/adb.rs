use std::process::Command;

use anyhow::{Result, anyhow, bail};

use crate::device::{Device, parse_devices, resolve};

/// Every device adb currently sees, in whatever state.
pub fn devices() -> Result<Vec<Device>> {
    let output = Command::new("adb")
        .args(["devices", "-l"])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow!("adb is not installed, run `brew install android-platform-tools`")
            }
            _ => anyhow!(e).context("failed to run adb"),
        })?;

    if !output.status.success() {
        bail!(
            "adb devices failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
}

/// The device to drive, honouring an explicit `--device` serial when given.
pub fn target(wanted: Option<&str>) -> Result<Device> {
    let devices = devices()?;
    resolve(&devices, wanted).cloned()
}
