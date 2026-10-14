use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub command: String,
    #[serde(default)]
    pub marker: Option<String>,
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("wsctl/agents.json")
}

pub fn load() -> Result<Config> {
    let path = path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}
