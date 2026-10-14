use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use disk::liveness::Liveness;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    TempClone,
    Worktree { repo: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Leftover {
    pub path: PathBuf,
    pub kind: Kind,
}

pub fn temp_roots() -> Vec<PathBuf> {
    let roots: BTreeSet<PathBuf> = [std::env::temp_dir(), PathBuf::from("/tmp")]
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    roots.into_iter().collect()
}

pub fn find(agent_cwds: &[PathBuf], live: &Liveness) -> Vec<Leftover> {
    let mut found: Vec<Leftover> = temp_roots()
        .iter()
        .flat_map(|root| clones_under(root))
        .filter(|path| !live.is_busy(path))
        .map(|path| Leftover {
            path,
            kind: Kind::TempClone,
        })
        .collect();

    let repos: BTreeSet<PathBuf> = agent_cwds.iter().filter_map(|cwd| main_repo(cwd)).collect();
    for repo in repos {
        for path in linked_worktrees(&repo) {
            if !live.is_busy(&path) && !found.iter().any(|leftover| leftover.path == path) {
                found.push(Leftover {
                    path,
                    kind: Kind::Worktree { repo: repo.clone() },
                });
            }
        }
    }
    found
}

fn clones_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if path.join(".git").exists() {
            found.push(path);
            continue;
        }
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        found.extend(
            children
                .flatten()
                .filter(|child| child.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|child| child.path())
                .filter(|child| child.join(".git").exists()),
        );
    }
    found
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main_repo(cwd: &Path) -> Option<PathBuf> {
    let common = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    PathBuf::from(common).parent().map(Path::to_path_buf)
}

fn linked_worktrees(repo: &Path) -> Vec<PathBuf> {
    git(repo, &["worktree", "list", "--porcelain"])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .skip(1)
        .map(PathBuf::from)
        .collect()
}

pub fn unsaved_work(path: &Path) -> Option<String> {
    let Some(status) = git(path, &["status", "--porcelain"]) else {
        return Some("git status failed".into());
    };
    if !status.is_empty() {
        return Some("has uncommitted changes".into());
    }
    let unpushed = git(
        path,
        &["log", "--branches", "--not", "--remotes", "--oneline"],
    )
    .unwrap_or_default();
    (!unpushed.is_empty()).then(|| "has commits on no remote".into())
}

pub fn remove(leftover: &Leftover) -> Result<(), String> {
    if let Some(reason) = unsaved_work(&leftover.path) {
        return Err(reason);
    }
    match &leftover.kind {
        Kind::TempClone => std::fs::remove_dir_all(&leftover.path).map_err(|err| err.to_string()),
        Kind::Worktree { repo } => {
            let path = leftover.path.to_string_lossy();
            git(repo, &["worktree", "remove", &path])
                .map(|_| ())
                .ok_or_else(|| "git worktree remove failed".into())
        }
    }
}
