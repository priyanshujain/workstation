use std::path::PathBuf;
use std::time::SystemTime;

use crate::cleanup::{CleanAction, Measure, Target};
use crate::conventions::{self, Kind, Places};
use crate::projects::Project;

pub const LOG_IDLE_DAYS: u64 = 30;
pub const PROJECT_IDLE_DAYS: u64 = 30;

pub fn offers(
    places: &Places,
    closed: &[PathBuf],
    projects: &[Project],
    now: SystemTime,
) -> Vec<Target> {
    let mut targets: Vec<Target> = conventions::rules(places, closed)
        .into_iter()
        .filter(|rule| rule.path.is_dir())
        .filter_map(|rule| {
            let kind = Kind::from_label(&rule.category)?;
            let target = offer(kind, &rule.label, rule.path)?;
            Some(match places.apps.has_app(&rule.label) {
                true => target.owned_by_app(&rule.label),
                false => target,
            })
        })
        .collect();

    for project in projects {
        if !project.is_idle_for(PROJECT_IDLE_DAYS, now) {
            continue;
        }
        for artifact in &project.artifacts {
            targets.push(Target::new(
                format!(
                    "Build output > {}",
                    conventions::project_name(&project.root)
                ),
                artifact.path.display().to_string(),
                artifact.size,
                CleanAction::RemoveDir(artifact.path.clone()),
            ));
        }
    }

    if on_path("nix-collect-garbage") {
        targets.push(Target::unknown(
            "Nix > garbage",
            "Store paths no generation or GC root uses",
            CleanAction::RunCommand("nix-collect-garbage".into(), Vec::new()),
        ));
    }
    targets
}

fn offer(kind: Kind, owner: &str, path: PathBuf) -> Option<Target> {
    let name = match kind {
        Kind::Trash => kind.label().to_string(),
        _ => format!("{} > {owner}", kind.label()),
    };
    let description = path.display().to_string();
    let target = match kind {
        Kind::Caches | Kind::Trash => Target::pending(
            name,
            description,
            Measure::Tree(path.clone()),
            CleanAction::RemoveContents(path),
        ),
        Kind::SavedState | Kind::RemovedApps => Target::pending(
            name,
            description,
            Measure::Tree(path.clone()),
            CleanAction::RemoveDir(path),
        ),
        Kind::Logs => Target::pending(
            name,
            format!("{description}, entries untouched for {LOG_IDLE_DAYS} days"),
            Measure::IdleChildren {
                dir: path.clone(),
                idle_days: LOG_IDLE_DAYS,
            },
            CleanAction::RemoveIdleChildren {
                dir: path,
                idle_days: LOG_IDLE_DAYS,
            },
        ),
        Kind::AppData
        | Kind::BuildOutput
        | Kind::Packages
        | Kind::Nix
        | Kind::Temp
        | Kind::SourceCode
        | Kind::Toolchains
        | Kind::Applications
        | Kind::DeveloperTools
        | Kind::PersonalFiles
        | Kind::ToolData
        | Kind::System => return None,
    };
    Some(target)
}

fn on_path(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(command).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owners::InstalledApps;
    use crate::projects::{Artifact, ProjectKind};
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    fn places(home: &Path) -> Places {
        Places {
            home: home.to_path_buf(),
            xdg_cache: home.join(".cache"),
            xdg_data: home.join(".local/share"),
            xdg_state: home.join(".local/state"),
            temp: vec![home.join("tmp")],
            package_prefixes: Vec::new(),
            apps: InstalledApps::default(),
            macos: false,
        }
    }

    fn actions(targets: &[Target], home: &Path) -> Vec<(String, String)> {
        targets
            .iter()
            .filter_map(|t| {
                let (verb, path) = match t.action() {
                    CleanAction::RemoveContents(p) => ("empty", p),
                    CleanAction::RemoveDir(p) => ("delete", p),
                    CleanAction::RemoveIdleChildren { dir, .. } => ("idle", dir),
                    _ => return None,
                };
                path.starts_with(home)
                    .then(|| (t.name.clone(), format!("{verb} {}", path.display())))
            })
            .collect()
    }

    #[test]
    fn caches_are_emptied_and_app_data_temp_and_other_folders_are_never_offered() {
        let home = tempfile::tempdir().unwrap();
        for dir in [
            ".cache/tool",
            ".local/share/editor",
            ".local/state/editor",
            "tmp/x",
            "Documents",
        ] {
            fs::create_dir_all(home.path().join(dir)).unwrap();
        }

        let offered = actions(
            &offers(&places(home.path()), &[], &[], SystemTime::now()),
            home.path(),
        );

        assert_eq!(
            offered,
            vec![(
                "Caches > tool".to_string(),
                format!("empty {}", home.path().join(".cache/tool").display())
            )]
        );
    }

    #[test]
    fn a_cache_owned_by_an_installed_app_waits_for_the_app_to_quit() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".cache/Editor")).unwrap();
        let mut places = places(home.path());
        places.apps.add("com.example.Editor", "Editor");

        let targets = offers(&places, &[], &[], SystemTime::now());
        let live = crate::liveness::Liveness::running_from_test(
            "/Applications/Editor.app/Contents/MacOS/Editor",
        );

        let cache = targets
            .iter()
            .find(|t| t.name == "Caches > Editor")
            .unwrap();
        assert!(cache.refuse_if_in_use(&live).is_err());
    }

    #[test]
    fn only_build_output_of_idle_projects_is_offered() {
        let home = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let project = |name: &str, days: u64| Project {
            root: home.path().join(name),
            kind: ProjectKind::Cargo,
            artifacts: vec![Artifact {
                path: home.path().join(name).join("target"),
                size: 10,
            }],
            last_active: Some(now - Duration::from_secs(days * 86_400)),
        };

        let offered = actions(
            &offers(
                &places(home.path()),
                &[],
                &[project("old", 40), project("fresh", 2)],
                now,
            ),
            home.path(),
        );

        assert_eq!(
            offered,
            vec![(
                format!(
                    "Build output > {}/old",
                    home.path().file_name().unwrap().to_string_lossy()
                ),
                format!("delete {}", home.path().join("old/target").display())
            )]
        );
    }

    #[test]
    fn logs_are_cleaned_by_age_and_trash_is_emptied() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".local/share/Trash")).unwrap();
        let mut places = places(home.path());
        places.macos = true;
        fs::create_dir_all(home.path().join("Library/Logs/tool")).unwrap();
        fs::create_dir_all(home.path().join(".Trash")).unwrap();

        let offered = actions(&offers(&places, &[], &[], SystemTime::now()), home.path());

        assert!(offered.contains(&(
            "Logs > tool".to_string(),
            format!("idle {}", home.path().join("Library/Logs/tool").display())
        )));
        assert!(offered.contains(&(
            "Trash".to_string(),
            format!("empty {}", home.path().join(".Trash").display())
        )));
    }
}
