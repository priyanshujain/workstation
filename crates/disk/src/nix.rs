use std::path::Path;
use std::process::Command;

const DEFAULT_PROFILE_STORE: &str = "/nix/var/nix/profiles/default/bin/nix-store";

pub fn garbage_bytes() -> Option<u64> {
    let nix_store = nix_store()?;
    let dead = Command::new(&nix_store)
        .args(["--gc", "--print-dead"])
        .output()
        .ok()?;
    if !dead.status.success() {
        return None;
    }
    let paths: Vec<String> = String::from_utf8_lossy(&dead.stdout)
        .lines()
        .filter(|line| line.starts_with('/'))
        .map(String::from)
        .collect();
    if paths.is_empty() {
        return Some(0);
    }
    let sizes = Command::new(&nix_store)
        .args(["--query", "--size"])
        .args(&paths)
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&sizes.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .sum(),
    )
}

fn nix_store() -> Option<String> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("nix-store"))
            .find(|candidate| candidate.is_file())
    });
    on_path
        .or_else(|| Some(Path::new(DEFAULT_PROFILE_STORE).to_path_buf()))
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string())
}
