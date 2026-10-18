use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::Value;

use super::{Check, Target, output, user};

pub fn check(target: &Target) -> Result<Check> {
    let attr = format!(
        "{}#homeConfigurations.\"{}@{}\".config.dconf.settings",
        target.flake,
        user(),
        target.host
    );
    let declared: BTreeMap<String, BTreeMap<String, Value>> =
        serde_json::from_str(&output("nix", &["eval", "--json", &attr])?)?;

    let mut total = 0;
    let mut drift = Vec::new();
    for (dir, keys) in &declared {
        for (key, value) in keys {
            total += 1;
            let path = format!("/{dir}/{key}");
            let want = gvariant(value)?;
            let have = output("dconf", &["read", &path])?.trim().to_string();
            if have != want {
                let have = if have.is_empty() { "unset" } else { &have };
                drift.push(format!("{path}: want {want}, have {have}"));
            }
        }
    }

    Ok(if drift.is_empty() {
        Check::pass("settings", format!("{total} declared dconf keys match"))
    } else {
        Check::fail(
            "settings",
            format!("{} of {total} declared dconf keys differ", drift.len()),
            drift,
        )
    })
}

fn gvariant(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(gvariant)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        other => bail!("cannot compare dconf value {other}"),
    })
}
