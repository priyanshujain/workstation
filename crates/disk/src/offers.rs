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
            "Nix store > garbage",
            "Store paths no generation or GC root uses",
            CleanAction::RunCommand("nix-collect-garbage".into(), Vec::new()),
        ));
    }
    targets
}

pub fn ids(targets: &[Target]) -> Vec<String> {
    let names: Vec<String> = targets.iter().map(|t| slug(&t.name)).collect();
    targets
        .iter()
        .zip(&names)
        .map(|(target, name)| {
            let namesakes: Vec<&Target> = targets
                .iter()
                .zip(&names)
                .filter(|(_, other)| *other == name)
                .map(|(t, _)| t)
                .collect();
            match namesakes.len() {
                1 => name.clone(),
                _ => format!("{name}-{}", distinct_tail(target, &namesakes)),
            }
        })
        .collect()
}

fn distinct_tail(target: &Target, namesakes: &[&Target]) -> String {
    let depth = target.path().map_or(0, |p| p.components().count());
    (1..=depth)
        .map(|count| (count, path_tail(target, count)))
        .find(|(count, tail)| {
            namesakes
                .iter()
                .filter(|other| path_tail(other, *count) == *tail)
                .count()
                == 1
        })
        .map_or_else(|| path_tail(target, depth), |(_, tail)| tail)
}

fn path_tail(target: &Target, count: usize) -> String {
    let parts: Vec<String> = target
        .path()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let start = parts.len().saturating_sub(count);
    slug(&parts[start..].join(" "))
}

pub fn slug(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
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
    fn every_offered_target_gets_a_unique_id() {
        let home = tempfile::tempdir().unwrap();
        for dir in [".cache/tool", ".local/share/Trash"] {
            fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        let project = Project {
            root: home.path().join("app"),
            kind: ProjectKind::Gradle,
            artifacts: ["app/build", "lib/build"]
                .iter()
                .map(|dir| Artifact {
                    path: home.path().join("app").join(dir),
                    size: 10,
                })
                .collect(),
            last_active: Some(SystemTime::now() - Duration::from_secs(90 * 86_400)),
        };

        let targets = offers(&places(home.path()), &[], &[project], SystemTime::now());
        let ids = ids(&targets);

        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(ids.iter().all(|id| !id.is_empty()), "{ids:?}");
        assert!(
            ids.iter().any(|id| id.ends_with("-app-lib-build")),
            "{ids:?}"
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
