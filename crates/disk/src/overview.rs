use std::path::Path;
use std::process::Command;

/// What the disk is doing, at the granularity that matters for cleanup: the
/// container is the number on the box, the data volume is the only part wsctl
/// can walk or delete from, and the rest is real but not ours.
pub struct DiskOverview {
    /// The whole APFS container.
    pub total: u64,
    /// Free in the container, which is what any volume in it can grow into.
    pub free: u64,
    /// Used by the data volume, the one the sweep measures.
    pub data_used: u64,
    /// System, Preboot, Recovery, VM, Nix Store, anything else sharing the
    /// container. Counted so it stops showing up as an unexplained gap.
    pub other_volumes: u64,
}

impl DiskOverview {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn usage_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used() as f64 / self.total as f64) * 100.0
    }
}

/// Container figures from `diskutil` where they are available, falling back to
/// `df`. They are not interchangeable: `df` reports one volume's slice of a
/// shared container, so on APFS its "total" and "free" describe something
/// narrower than the disk the user is looking at.
pub fn disk_overview() -> Option<DiskOverview> {
    let (df_total, data_used, df_free) = statfs(df_path())?;

    let plist = diskutil_plist(df_path());
    let total = plist
        .as_deref()
        .and_then(|p| plist_u64(p, "APFSContainerSize"))
        .unwrap_or(df_total);
    let free = plist
        .as_deref()
        .and_then(|p| plist_u64(p, "APFSContainerFree"))
        .unwrap_or(df_free);

    Some(DiskOverview {
        total,
        free,
        data_used,
        other_volumes: total.saturating_sub(free).saturating_sub(data_used),
    })
}

fn statfs(path: &str) -> Option<(u64, u64, u64)> {
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
    Some((
        parts[1].parse::<u64>().ok()? * 1024,
        parts[2].parse::<u64>().ok()? * 1024,
        parts[3].parse::<u64>().ok()? * 1024,
    ))
}

fn diskutil_plist(path: &str) -> Option<String> {
    let output = Command::new("diskutil")
        .args(["info", "-plist", path])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pull one integer out of an XML plist. Deliberately not a plist parser: the
/// value has to be the tag immediately after its key, so a key appearing as
/// someone else's string value cannot hijack the lookup.
fn plist_u64(plist: &str, key: &str) -> Option<u64> {
    let needle = format!("<key>{key}</key>");
    let after = &plist[plist.find(&needle)? + needle.len()..];
    let rest = after.trim_start();
    let value = rest.strip_prefix("<integer>")?;
    let end = value.find("</integer>")?;
    value[..end].trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn df_path() -> &'static str {
    if Path::new("/System/Volumes/Data").exists() {
        "/System/Volumes/Data"
    } else {
        "/"
    }
}

#[cfg(not(target_os = "macos"))]
fn df_path() -> &'static str {
    let _ = Path::new("/");
    "/"
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<plist version="1.0">
<dict>
	<key>APFSContainerFree</key>
	<integer>76281798656</integer>
	<key>APFSContainerSize</key>
	<integer>494384795648</integer>
	<key>DeviceIdentifier</key>
	<string>disk3s5</string>
</dict>
</plist>"#;

    #[test]
    fn reads_integers_by_key() {
        assert_eq!(plist_u64(SAMPLE, "APFSContainerFree"), Some(76281798656));
        assert_eq!(plist_u64(SAMPLE, "APFSContainerSize"), Some(494384795648));
        assert_eq!(plist_u64(SAMPLE, "Missing"), None);
    }

    #[test]
    fn refuses_a_key_whose_value_is_not_an_integer() {
        assert_eq!(plist_u64(SAMPLE, "DeviceIdentifier"), None);
    }

    #[test]
    fn other_volumes_absorbs_what_the_data_volume_does_not_hold() {
        // The container holds System, Preboot, Recovery and VM too. Without
        // this row those bytes read as an unexplained gap between the disk
        // size and everything the sweep can see.
        let overview = DiskOverview {
            total: 494_384_795_648,
            free: 76_281_798_656,
            data_used: 385_784_758_272,
            other_volumes: 494_384_795_648u64 - 76_281_798_656 - 385_784_758_272,
        };
        assert_eq!(overview.used(), 418_102_996_992);
        assert_eq!(overview.data_used + overview.other_volumes, overview.used());
        assert!((overview.usage_percent() - 84.6).abs() < 0.5);
    }

    #[test]
    fn usage_percent_is_zero_for_a_zero_sized_disk() {
        let overview = DiskOverview {
            total: 0,
            free: 0,
            data_used: 0,
            other_volumes: 0,
        };
        assert_eq!(overview.usage_percent(), 0.0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_real_disk_adds_up() {
        let overview = disk_overview().expect("df should work");
        assert!(overview.total > 0);
        assert_eq!(
            overview.data_used + overview.other_volumes,
            overview.used(),
            "the parts must equal the whole"
        );
    }
}
