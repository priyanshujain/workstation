use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use std::path::PathBuf;

pub fn server_socket() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("REX_SERVER") {
        return from_server_env(&value);
    }
    if let Ok(dir) = std::env::var("REX_RUNTIME_DIR") {
        return Ok(PathBuf::from(dir).join("server.sock"));
    }
    Ok(default_dir()?.join("server.sock"))
}

fn from_server_env(value: &str) -> Result<PathBuf> {
    match value.strip_prefix("unix://") {
        Some(encoded) => Ok(PathBuf::from(
            percent_decode_str(encoded).decode_utf8()?.into_owned(),
        )),
        None if value.starts_with('/') => Ok(PathBuf::from(value)),
        None => bail!("REX_SERVER={value} is not a local unix socket"),
    }
}

#[cfg(target_os = "macos")]
fn default_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("no Application Support directory")?
        .join("rex"))
}

#[cfg(not(target_os = "macos"))]
fn default_dir() -> Result<PathBuf> {
    dirs::runtime_dir()
        .map(|dir| dir.join("rex"))
        .or_else(|| dirs::state_dir().map(|dir| dir.join("rex/run")))
        .context("no XDG runtime or state directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_unix_url() {
        assert_eq!(
            from_server_env("unix:///Users/a/Library/Application%20Support/rex/server.sock")
                .unwrap(),
            PathBuf::from("/Users/a/Library/Application Support/rex/server.sock")
        );
    }

    #[test]
    fn accepts_plain_path() {
        assert_eq!(
            from_server_env("/run/rex/server.sock").unwrap(),
            PathBuf::from("/run/rex/server.sock")
        );
    }

    #[test]
    fn refuses_remote_server() {
        assert!(from_server_env("tcp://host:1234").is_err());
    }
}
