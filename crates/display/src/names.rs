use std::collections::HashMap;
use std::process::Command;

/// Friendly panel names keyed by `CGDirectDisplayID`.
///
/// CoreGraphics has no cheap way to read the EDID product name, so this shells out to
/// `system_profiler`. It is best-effort: an empty map only costs the name column.
pub fn lookup() -> HashMap<u32, String> {
    let Ok(out) = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
    else {
        return HashMap::new();
    };
    if !out.status.success() {
        return HashMap::new();
    }
    parse(&String::from_utf8_lossy(&out.stdout))
}

fn parse(raw: &str) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return out;
    };
    let Some(gpus) = root.get("SPDisplaysDataType").and_then(|g| g.as_array()) else {
        return out;
    };

    for gpu in gpus {
        let Some(panels) = gpu.get("spdisplays_ndrvs").and_then(|p| p.as_array()) else {
            continue;
        };
        for panel in panels {
            let id = panel
                .get("_spdisplays_displayID")
                .and_then(|i| i.as_str())
                .and_then(|i| i.trim().parse::<u32>().ok());
            let name = panel.get("_name").and_then(|n| n.as_str());
            if let (Some(id), Some(name)) = (id, name) {
                out.insert(id, name.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "SPDisplaysDataType": [
        {
          "_name": "Apple M3 Pro",
          "spdisplays_ndrvs": [
            {
              "_name": "DELL P2419H",
              "_spdisplays_displayID": "2",
              "_spdisplays_display-serial-number": "36574c42"
            },
            {
              "_name": "DELL P2419H",
              "_spdisplays_displayID": "3",
              "_spdisplays_display-serial-number": "35434e42"
            }
          ]
        }
      ]
    }"#;

    #[test]
    fn reads_names_by_display_id() {
        let names = parse(SAMPLE);
        assert_eq!(names.get(&2).map(String::as_str), Some("DELL P2419H"));
        assert_eq!(names.get(&3).map(String::as_str), Some("DELL P2419H"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn malformed_json_yields_no_names() {
        assert!(parse("{ not json").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn missing_keys_are_skipped_rather_than_panicking() {
        let raw = r#"{"SPDisplaysDataType":[{"spdisplays_ndrvs":[{"_name":"No ID"},{"_spdisplays_displayID":"7"}]}]}"#;
        assert!(parse(raw).is_empty());
    }

    #[test]
    fn unexpected_shape_yields_no_names() {
        assert!(parse(r#"{"SPDisplaysDataType": "not an array"}"#).is_empty());
        assert!(parse(r#"{"SomethingElse": []}"#).is_empty());
    }
}
