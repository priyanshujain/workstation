use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use console::style;
use serde::Deserialize;

mod workspace;

#[derive(Deserialize)]
struct Rules {
    #[serde(default)]
    types: BTreeMap<String, String>,
    #[serde(default)]
    replace: BTreeMap<String, String>,
}

pub struct Mismatch {
    pub content_type: String,
    pub current: Option<String>,
    pub wanted: String,
}

pub struct Report {
    pub managed: usize,
    pub mismatches: Vec<Mismatch>,
    pub unmapped: Vec<String>,
}

fn rules_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("wsctl/default-apps.json")
}

fn load() -> Result<Option<Rules>> {
    let path = rules_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let rules = serde_json::from_str(&raw)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(rules))
}

fn content_type(key: &str) -> Option<String> {
    if key.contains('.') {
        Some(key.to_string())
    } else {
        workspace::type_for_extension(key)
    }
}

fn same_app(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn declared_types(bundle_id: &str) -> Result<Vec<String>> {
    let Some(app) = workspace::app_path(bundle_id) else {
        return Ok(Vec::new());
    };
    let info = plist::Value::from_file(app.join("Contents/Info.plist"))
        .with_context(|| format!("could not read the Info.plist of {}", app.display()))?;
    let documents = info
        .as_dictionary()
        .and_then(|d| d.get("CFBundleDocumentTypes"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let strings = |document: &plist::Value, key: &str| -> Vec<String> {
        document
            .as_dictionary()
            .and_then(|d| d.get(key))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_string().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut types = Vec::new();
    for document in &documents {
        types.extend(strings(document, "LSItemContentTypes"));
        types.extend(
            strings(document, "CFBundleTypeExtensions")
                .iter()
                .filter(|e| *e != "*")
                .filter_map(|e| workspace::type_for_extension(e)),
        );
    }
    Ok(types)
}

pub fn report() -> Result<Option<Report>> {
    let Some(rules) = load()? else {
        return Ok(None);
    };
    let mut wanted = BTreeMap::new();
    for (from, to) in &rules.replace {
        for declared in declared_types(from)? {
            if workspace::handler(&declared).is_some_and(|h| same_app(&h, from)) {
                wanted.insert(declared, to.clone());
            }
        }
    }
    let mut unmapped = Vec::new();
    for (key, app) in &rules.types {
        match content_type(key) {
            Some(found) => {
                wanted.insert(found, app.clone());
            }
            None => unmapped.push(key.clone()),
        }
    }
    let mismatches = wanted
        .iter()
        .filter_map(|(found, app)| {
            let current = workspace::handler(found);
            let matches = current.as_deref().is_some_and(|c| same_app(c, app));
            (!matches).then(|| Mismatch {
                content_type: found.clone(),
                current,
                wanted: app.clone(),
            })
        })
        .collect();
    Ok(Some(Report {
        managed: wanted.len(),
        mismatches,
        unmapped,
    }))
}

pub fn describe(mismatch: &Mismatch) -> String {
    format!(
        "{}: {} -> {}",
        mismatch.content_type,
        mismatch.current.as_deref().unwrap_or("nothing"),
        mismatch.wanted
    )
}

pub fn run() -> Result<()> {
    let report = report()?
        .with_context(|| format!("no default apps declared in {}", rules_path().display()))?;
    if report.mismatches.is_empty() {
        println!("all {} types open in their declared app", report.managed);
        return Ok(());
    }
    for mismatch in &report.mismatches {
        println!("  {}", describe(mismatch));
    }
    println!(
        "macOS asks once per type; choose the new app in each of the {} dialogs.",
        report.mismatches.len()
    );
    if !confirm("Set them now?")? {
        return Ok(());
    }
    for mismatch in &report.mismatches {
        match workspace::set_handler(&mismatch.content_type, &mismatch.wanted) {
            Ok(())
                if workspace::handler(&mismatch.content_type)
                    .is_some_and(|h| same_app(&h, &mismatch.wanted)) =>
            {
                println!("{} {}", style("✓").green(), mismatch.content_type);
            }
            Ok(()) => println!(
                "{} {} kept its old app",
                style("!").yellow(),
                mismatch.content_type
            ),
            Err(e) => println!("{} {e:#}", style("×").red()),
        }
    }
    Ok(())
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}
