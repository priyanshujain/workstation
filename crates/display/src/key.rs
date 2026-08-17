use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Stable identity for a physical panel, taken from its EDID.
///
/// macOS renumbers `CGDirectDisplayID` on every reconnect, so it cannot be persisted.
/// Vendor, product and serial survive unplugging and are what the display UUID is built
/// from. The serial here is the same value `docs/displays.md` records in hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayKey {
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
}

impl DisplayKey {
    pub fn new(vendor: u32, model: u32, serial: u32) -> Self {
        Self {
            vendor,
            model,
            serial,
        }
    }

    /// EDID serial in the hex form `system_profiler` and `docs/displays.md` use.
    pub fn serial_hex(&self) -> String {
        format!("{:08x}", self.serial)
    }

    /// EDID product id in the hex form `docs/displays.md` uses.
    pub fn model_hex(&self) -> String {
        format!("{:04x}", self.model)
    }

    /// Panels that report no serial cannot be told apart from an identical sibling.
    pub fn is_ambiguous(&self) -> bool {
        self.serial == 0
    }
}

impl fmt::Display for DisplayKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.vendor, self.model, self.serial)
    }
}

impl FromStr for DisplayKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            bail!("expected vendor:model:serial, got {s:?}");
        }
        let mut nums = [0u32; 3];
        for (slot, part) in nums.iter_mut().zip(parts) {
            *slot = part
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{part:?} is not a number in {s:?}"))?;
        }
        Ok(Self::new(nums[0], nums[1], nums[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let key = DisplayKey::new(4268, 53466, 911690818);
        assert_eq!(key.to_string(), "4268:53466:911690818");
        assert_eq!(DisplayKey::from_str("4268:53466:911690818").unwrap(), key);
    }

    #[test]
    fn hex_matches_system_profiler() {
        let key = DisplayKey::new(4268, 53466, 911690818);
        assert_eq!(key.serial_hex(), "36574c42");
        assert_eq!(key.model_hex(), "d0da");
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(DisplayKey::from_str("4268:53466").is_err());
        assert!(DisplayKey::from_str("4268:53466:912:7").is_err());
        assert!(DisplayKey::from_str("dell:p2419h:one").is_err());
        assert!(DisplayKey::from_str("").is_err());
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let key = DisplayKey::from_str(" 4268 : 53466 : 911690818 ").unwrap();
        assert_eq!(key, DisplayKey::new(4268, 53466, 911690818));
    }

    #[test]
    fn zero_serial_is_ambiguous() {
        assert!(DisplayKey::new(4268, 53466, 0).is_ambiguous());
        assert!(!DisplayKey::new(4268, 53466, 1).is_ambiguous());
    }
}
