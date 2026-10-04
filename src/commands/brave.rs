use std::path::Path;

use anyhow::Result;

pub fn debloat(path: &Path) -> Result<()> {
    brave::write_profile(path)?;
    println!("Wrote {}", path.display());
    println!(
        "Install the profile in System Settings > General > Device Management, then restart Brave and check brave://policy."
    );
    Ok(())
}
