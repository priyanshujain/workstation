use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use plist::Value;

use super::{Check, Target, output};

const SCRIPTS: &[&str] = &["userDefaults", "defaults", "postActivation"];

struct Setting {
    current_host: bool,
    domain: String,
    key: String,
    value: Value,
}

pub fn check(target: &Target) -> Result<Check> {
    let mut settings = Vec::new();
    for script in SCRIPTS {
        let attr = format!(
            "{}#darwinConfigurations.{}.config.system.activationScripts.{script}.text",
            target.flake, target.host
        );
        let text = output("nix", &["eval", "--raw", &attr])?;
        for words in statements(&text) {
            if let Some(setting) = setting(&words)? {
                settings.push(setting);
            }
        }
    }

    let mut domains: BTreeMap<(bool, String), Value> = BTreeMap::new();
    let mut drift = Vec::new();
    for setting in &settings {
        let scope = (setting.current_host, setting.domain.clone());
        if !domains.contains_key(&scope) {
            domains.insert(
                scope.clone(),
                exported(setting.current_host, &setting.domain)?,
            );
        }
        let live = domains[&scope]
            .as_dictionary()
            .and_then(|d| d.get(&setting.key));
        if !live.is_some_and(|live| matches(&setting.value, live)) {
            let host = if setting.current_host {
                " (currentHost)"
            } else {
                ""
            };
            drift.push(format!(
                "{}{host} {}: want {}, have {}",
                setting.domain,
                setting.key,
                show(&setting.value),
                live.map_or("unset".to_string(), show)
            ));
        }
    }

    Ok(if drift.is_empty() {
        Check::pass(
            "settings",
            format!("{} declared defaults match", settings.len()),
        )
    } else {
        Check::fail(
            "settings",
            format!(
                "{} of {} declared defaults differ",
                drift.len(),
                settings.len()
            ),
            drift,
        )
    })
}

fn exported(current_host: bool, domain: &str) -> Result<Value> {
    let mut args = Vec::new();
    if current_host {
        args.push("-currentHost");
    }
    args.extend(["export", domain, "-"]);
    let xml = output("defaults", &args)?;
    Value::from_reader_xml(xml.as_bytes()).with_context(|| format!("could not read {domain}"))
}

fn setting(words: &[String]) -> Result<Option<Setting>> {
    let Some(start) = words.iter().position(|w| w == "defaults") else {
        return Ok(None);
    };
    let mut rest = words[start + 1..].iter().map(String::as_str);
    let mut next = rest.next();
    let current_host = next == Some("-currentHost");
    if current_host {
        next = rest.next();
    }
    if next != Some("write") {
        return Ok(None);
    }
    let (Some(domain), Some(key)) = (rest.next(), rest.next()) else {
        bail!("incomplete defaults write: {}", words.join(" "));
    };
    if !current_host && !domain.starts_with('/') && words[0] == "defaults" && domain != "-g" {
        bail!(
            "{domain} {key} is written as root to root's own preferences, use /Library/Preferences/{domain}"
        );
    }
    if domain.contains("/ByHost/") {
        bail!(
            "{domain} {key}: macOS reads ByHost files named with the host UUID, use defaults -currentHost"
        );
    }
    let domain = if domain == "-g" {
        "NSGlobalDomain"
    } else {
        domain
    };
    let value = match (rest.next(), rest.next()) {
        (Some("-int" | "-integer"), Some(v)) => Value::Integer(v.parse::<i64>()?.into()),
        (Some("-float"), Some(v)) => Value::Real(v.parse()?),
        (Some("-bool" | "-boolean"), Some(v)) => Value::Boolean(matches!(v, "true" | "yes" | "1")),
        (Some("-string"), Some(v)) => Value::String(v.to_string()),
        (Some(xml), None) => Value::from_reader_xml(xml.as_bytes())
            .with_context(|| format!("could not parse the value of {domain} {key}"))?,
        _ => bail!("unsupported defaults write: {}", words.join(" ")),
    };
    Ok(Some(Setting {
        current_host,
        domain: domain.to_string(),
        key: key.to_string(),
        value,
    }))
}

fn statements(script: &str) -> Vec<Vec<String>> {
    let mut statements = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
            },
            ' ' | '\t' | '\n' | ';' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if c != ' ' && c != '\t' && !words.is_empty() {
                    statements.push(std::mem::take(&mut words));
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        statements.push(words);
    }
    statements
}

fn matches(want: &Value, have: &Value) -> bool {
    match (want, have) {
        (Value::Dictionary(want), Value::Dictionary(have)) if want.contains_key("_CFURLString") => {
            url_path(want) == url_path(have)
        }
        (Value::Dictionary(want), Value::Dictionary(have)) => want
            .iter()
            .all(|(k, v)| have.get(k).is_some_and(|h| matches(v, h))),
        (Value::Array(want), Value::Array(have)) => {
            want.len() == have.len() && want.iter().zip(have).all(|(w, h)| matches(w, h))
        }
        (Value::Integer(_) | Value::Real(_), Value::Integer(_) | Value::Real(_)) => {
            number(want) == number(have)
        }
        (Value::Boolean(_) | Value::Integer(_) | Value::String(_), _) if truth(want).is_some() => {
            truth(want) == truth(have)
        }
        _ => want == have,
    }
}

fn truth(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(b) => Some(*b),
        Value::Integer(i) => match i.as_signed() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(s) => match s.as_str() {
            "YES" | "true" => Some(true),
            "NO" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn url_path(url: &plist::Dictionary) -> Option<String> {
    let raw = url.get("_CFURLString")?.as_string()?;
    let path = raw.strip_prefix("file://").unwrap_or(raw);
    let mut decoded = Vec::new();
    let mut bytes = path.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hex: String = bytes.by_ref().take(2).map(char::from).collect();
                decoded.push(u8::from_str_radix(&hex, 16).ok()?);
            }
            b => decoded.push(b),
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let path = std::fs::canonicalize(&path).map_or(path, |p| p.display().to_string());
    Some(path.trim_end_matches('/').to_string())
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => i.as_signed().map(|i| i as f64),
        Value::Real(r) => Some(*r),
        _ => None,
    }
}

fn show(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(show).collect::<Vec<_>>().join(", ")),
        Value::Dictionary(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{k}: {}", show(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}
