use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::owners::{InstalledApps, Owner};
use crate::sweep::{Rule, is_closed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Caches,
    Logs,
    AppData,
    SavedState,
    BuildOutput,
    Packages,
    Nix,
    Temp,
    Trash,
    RemovedApps,
    SourceCode,
    Toolchains,
    Applications,
    DeveloperTools,
    PersonalFiles,
    ToolData,
    System,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Caches => "Caches",
            Kind::Logs => "Logs",
            Kind::AppData => "App data",
            Kind::SavedState => "Saved state",
            Kind::BuildOutput => "Build output",
            Kind::Packages => "Packages",
            Kind::Nix => "Nix store",
            Kind::Temp => "Temp",
            Kind::Trash => "Trash",
            Kind::RemovedApps => "Removed apps",
            Kind::SourceCode => "Source code",
            Kind::Toolchains => "Toolchains",
            Kind::Applications => "Applications",
            Kind::DeveloperTools => "Developer tools",
            Kind::PersonalFiles => "Personal files",
            Kind::ToolData => "Tool data",
            Kind::System => "System files",
        }
    }

    pub fn recovery(self) -> Recovery {
        match self {
            Kind::Caches
            | Kind::Logs
            | Kind::SavedState
            | Kind::BuildOutput
            | Kind::Temp
            | Kind::Trash
            | Kind::RemovedApps => Recovery::Safe,
            Kind::Packages
            | Kind::Nix
            | Kind::Toolchains
            | Kind::Applications
            | Kind::DeveloperTools => Recovery::Refetch,
            Kind::AppData | Kind::SourceCode | Kind::PersonalFiles => Recovery::Keep,
            Kind::System => Recovery::System,
            Kind::ToolData => Recovery::Unknown,
        }
    }

    pub fn from_label(label: &str) -> Option<Kind> {
        ALL.into_iter().find(|kind| kind.label() == label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Safe,
    Refetch,
    Keep,
    System,
    Unknown,
}

impl Recovery {
    pub fn label(self) -> &'static str {
        match self {
            Recovery::Safe => "safe",
            Recovery::Refetch => "refetch",
            Recovery::Keep => "keep",
            Recovery::System => "system",
            Recovery::Unknown => "unknown",
        }
    }
}

pub const ALL: [Kind; 17] = [
    Kind::Caches,
    Kind::Logs,
    Kind::AppData,
    Kind::SavedState,
    Kind::BuildOutput,
    Kind::Packages,
    Kind::Nix,
    Kind::Temp,
    Kind::Trash,
    Kind::RemovedApps,
    Kind::SourceCode,
    Kind::Toolchains,
    Kind::Applications,
    Kind::DeveloperTools,
    Kind::PersonalFiles,
    Kind::ToolData,
    Kind::System,
];

#[derive(Debug, Clone)]
pub struct Places {
    pub home: PathBuf,
    pub xdg_cache: PathBuf,
    pub xdg_data: PathBuf,
    pub xdg_state: PathBuf,
    pub temp: Vec<PathBuf>,
    pub package_prefixes: Vec<PathBuf>,
    pub apps: InstalledApps,
    pub macos: bool,
}

impl Places {
    pub fn current() -> Option<Places> {
        let home = dirs::home_dir()?;
        let xdg = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        Some(Places {
            xdg_cache: xdg("XDG_CACHE_HOME", ".cache"),
            xdg_data: xdg("XDG_DATA_HOME", ".local/share"),
            xdg_state: xdg("XDG_STATE_HOME", ".local/state"),
            temp: temp_dirs(),
            package_prefixes: std::env::var_os("HOMEBREW_PREFIX")
                .map(PathBuf::from)
                .into_iter()
                .collect(),
            apps: if cfg!(target_os = "macos") {
                InstalledApps::scan()
            } else {
                InstalledApps::default()
            },
            macos: cfg!(target_os = "macos"),
            home,
        })
    }
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [std::env::temp_dir(), "/tmp".into(), "/var/tmp".into()]
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

pub fn rules(places: &Places, closed: &[PathBuf]) -> Vec<Rule> {
    let home = &places.home;
    let mut rules = home_folders(places, closed);
    let mut each_child = |kind: Kind, parent: &Path| {
        rules.extend(
            children(parent, closed)
                .into_iter()
                .map(|child| owned_rule(&places.apps, kind, child)),
        );
    };

    each_child(Kind::Caches, &places.xdg_cache);
    each_child(Kind::AppData, &places.xdg_data);
    each_child(Kind::AppData, &places.xdg_state);

    if places.macos {
        let library = home.join("Library");
        each_child(Kind::Caches, &library.join("Caches"));
        each_child(Kind::Logs, &library.join("Logs"));
        each_child(Kind::AppData, &library.join("Application Support"));
        each_child(Kind::SavedState, &library.join("Saved Application State"));
        each_child(Kind::AppData, &library.join("Containers"));
        each_child(Kind::AppData, &library.join("Group Containers"));
        each_child(Kind::Caches, Path::new("/Library/Caches"));
        each_child(Kind::Logs, Path::new("/Library/Logs"));
        each_child(Kind::Logs, Path::new("/private/var/log"));
    } else {
        each_child(Kind::Caches, Path::new("/var/cache"));
        each_child(Kind::Logs, Path::new("/var/log"));
    }

    if places.macos {
        for container in children(&home.join("Library/Containers"), closed) {
            let folder = owner_of(&container);
            let data = container.join("Data");
            rules.push(owned_by(
                &places.apps,
                Kind::Caches,
                &folder,
                data.join("Library/Caches"),
            ));
            rules.push(owned_by(
                &places.apps,
                Kind::Temp,
                &folder,
                data.join("tmp"),
            ));
        }
    }

    for tagged in tagged_caches(home, closed)
        .into_iter()
        .filter(|tagged| !tagged.starts_with(&places.xdg_cache))
    {
        rules.push(rule(Kind::Caches, project_name(&tagged), tagged));
    }

    for dir in &places.temp {
        rules.push(rule(Kind::Temp, owner_of(dir), dir.clone()));
    }
    let trash = if places.macos {
        home.join(".Trash")
    } else {
        places.xdg_data.join("Trash")
    };
    rules.push(rule(Kind::Trash, "Trash".into(), trash));
    rules.extend(toolchains(home));
    if places.macos {
        rules.extend(macos_developer_and_apps(home, closed));
    }
    rules.push(rule(Kind::Nix, "store".into(), PathBuf::from("/nix/store")));
    for prefix in &places.package_prefixes {
        rules.push(rule(Kind::Packages, owner_of(prefix), prefix.clone()));
    }

    rules.retain(|r| !is_closed(&r.path, closed));
    last_rule_per_path(rules)
}

fn home_folders(places: &Places, closed: &[PathBuf]) -> Vec<Rule> {
    let library = places.home.join("Library");
    children(&places.home, closed)
        .into_iter()
        .filter(|dir| *dir != library)
        .map(|dir| {
            let name = owner_of(&dir);
            let kind = if name.starts_with('.') {
                Kind::ToolData
            } else {
                Kind::PersonalFiles
            };
            rule(kind, name, dir)
        })
        .collect()
}

const TOOLCHAIN_HOMES: &[(&str, &str, &str)] = &[
    ("rustup", "RUSTUP_HOME", ".rustup"),
    ("cargo", "CARGO_HOME", ".cargo"),
    ("elan", "ELAN_HOME", ".elan"),
    ("ghcup", "GHCUP_INSTALL_BASE_PREFIX", ".ghcup"),
    ("cabal", "CABAL_DIR", ".cabal"),
    ("opam", "OPAMROOT", ".opam"),
    ("nvm", "NVM_DIR", ".nvm"),
    ("kotlin native", "KONAN_DATA_DIR", ".konan"),
    ("bun", "BUN_INSTALL", ".bun"),
    ("pyenv", "PYENV_ROOT", ".pyenv"),
    ("sdkman", "SDKMAN_DIR", ".sdkman"),
    ("deno", "DENO_DIR", ".deno"),
    ("go", "GOPATH", "go/pkg"),
    ("gradle", "GRADLE_USER_HOME", ".gradle/wrapper"),
];

fn toolchains(home: &Path) -> Vec<Rule> {
    let mut rules: Vec<Rule> = TOOLCHAIN_HOMES
        .iter()
        .map(|(tool, var, default)| {
            let first = std::env::var_os(var).and_then(|v| std::env::split_paths(&v).next());
            let path = match first {
                Some(dir) if *var == "GHCUP_INSTALL_BASE_PREFIX" => dir.join(".ghcup"),
                Some(dir) if *var == "GOPATH" => dir.join("pkg"),
                Some(dir) if *var == "GRADLE_USER_HOME" => dir.join("wrapper"),
                Some(dir) => dir,
                None => home.join(default),
            };
            rule(Kind::Toolchains, tool.to_string(), path)
        })
        .filter(|rule| rule.path.is_dir())
        .collect();
    let go_modules = std::env::var_os("GOMODCACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("go/pkg/mod"));
    if go_modules.is_dir() {
        rules.push(rule(Kind::Caches, "go modules".into(), go_modules));
    }
    let npm = home.join(".npm");
    if npm.is_dir() {
        rules.push(rule(Kind::Caches, "npm".into(), npm));
    }
    rules
}

fn macos_developer_and_apps(home: &Path, closed: &[PathBuf]) -> Vec<Rule> {
    let mut rules = Vec::new();
    for applications in [PathBuf::from("/Applications"), home.join("Applications")] {
        for app in children(&applications, closed) {
            let name = owner_of(&app);
            let name = name.strip_suffix(".app").unwrap_or(&name).to_string();
            rules.push(rule(Kind::Applications, name, app));
        }
    }
    for developer in [
        PathBuf::from("/Library/Developer"),
        home.join("Library/Developer"),
    ] {
        for tool in children(&developer, closed) {
            rules.push(rule(Kind::DeveloperTools, owner_of(&tool), tool));
        }
    }
    let derived = home.join("Library/Developer/Xcode/DerivedData");
    if derived.is_dir() {
        rules.push(rule(Kind::BuildOutput, "Xcode".into(), derived));
    }
    let pnpm = std::env::var_os("PNPM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Library/pnpm"));
    if pnpm.is_dir() {
        rules.push(rule(Kind::Toolchains, "pnpm".into(), pnpm));
    }
    rules
}

fn last_rule_per_path(rules: Vec<Rule>) -> Vec<Rule> {
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<Rule> = rules
        .into_iter()
        .rev()
        .filter(|r| seen.insert(r.path.clone()))
        .collect();
    kept.reverse();
    kept
}

const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
const TAG_DEPTH: usize = 4;

fn tagged_caches(home: &Path, closed: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for top in children(home, closed) {
        let hidden = top
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if !hidden {
            continue;
        }
        let mut walk = walkdir::WalkDir::new(&top)
            .follow_links(false)
            .max_depth(TAG_DEPTH)
            .into_iter();
        while let Some(Ok(entry)) = walk.next() {
            if !entry.file_type().is_dir() {
                continue;
            }
            if is_closed(entry.path(), closed) {
                walk.skip_current_dir();
                continue;
            }
            if has_cachedir_tag(entry.path()) {
                found.push(entry.into_path());
                walk.skip_current_dir();
            }
        }
    }
    found
}

fn has_cachedir_tag(dir: &Path) -> bool {
    use std::io::Read;
    let mut signature = [0u8; CACHEDIR_SIGNATURE.len()];
    std::fs::File::open(dir.join("CACHEDIR.TAG"))
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok_and(|_| signature == CACHEDIR_SIGNATURE)
}

pub fn build_output_rule(project_root: &Path, artifact: PathBuf) -> Rule {
    rule(Kind::BuildOutput, project_name(project_root), artifact)
}

pub fn source_code_rule(repository: PathBuf) -> Rule {
    rule(Kind::SourceCode, project_name(&repository), repository)
}

pub fn system_rule(name: String, path: PathBuf) -> Rule {
    rule(Kind::System, name, path)
}

pub fn project_name(root: &Path) -> String {
    let mut parts: Vec<String> = root
        .components()
        .rev()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.reverse();
    parts.join("/")
}

fn owned_rule(apps: &InstalledApps, kind: Kind, path: PathBuf) -> Rule {
    let folder = owner_of(&path);
    owned_by(apps, kind, &folder, path)
}

fn owned_by(apps: &InstalledApps, kind: Kind, folder: &str, path: PathBuf) -> Rule {
    match apps.resolve(folder) {
        Owner::App(name) => rule(kind, name, path),
        Owner::Removed if !changed_within(&path, REMOVED_AFTER) => {
            rule(Kind::RemovedApps, folder.to_string(), path)
        }
        Owner::Removed => rule(kind, folder.to_string(), path),
        Owner::Unresolved => rule(kind, folder.to_string(), path),
    }
}

const REMOVED_AFTER: Duration = Duration::from_secs(30 * 86_400);

fn changed_within(path: &Path, age: Duration) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_none_or(|elapsed| elapsed < age)
}

fn rule(kind: Kind, label: String, path: PathBuf) -> Rule {
    Rule {
        category: kind.label().to_string(),
        label,
        path,
    }
}

fn owner_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn children(parent: &Path, closed: &[PathBuf]) -> Vec<PathBuf> {
    if is_closed(parent, closed) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| !is_closed(path, closed))
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn places(home: &Path, macos: bool) -> Places {
        Places {
            home: home.to_path_buf(),
            xdg_cache: home.join(".cache"),
            xdg_data: home.join(".local/share"),
            xdg_state: home.join(".local/state"),
            temp: Vec::new(),
            package_prefixes: Vec::new(),
            apps: InstalledApps::default(),
            macos,
        }
    }

    fn find<'a>(rules: &'a [Rule], path: &Path) -> Option<&'a Rule> {
        rules.iter().find(|r| r.path == path)
    }

    #[test]
    fn each_child_of_a_conventional_folder_is_owned_by_its_own_name() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(".cache/some-tool");
        let data = home.path().join(".local/share/other-tool");
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir_all(&data).unwrap();

        let rules = rules(&places(home.path(), false), &[]);

        let cache = find(&rules, &cache).unwrap();
        assert_eq!(
            (cache.category.as_str(), cache.label.as_str()),
            ("Caches", "some-tool")
        );
        let data = find(&rules, &data).unwrap();
        assert_eq!(
            (data.category.as_str(), data.label.as_str()),
            ("App data", "other-tool")
        );
    }

    #[test]
    fn library_folders_are_read_by_convention_on_macos() {
        let home = tempfile::tempdir().unwrap();
        let library = home.path().join("Library");
        for dir in [
            "Caches/com.example.App",
            "Logs/DiagnosticReports",
            "Application Support/Example",
            "Saved Application State/com.example.App.savedState",
            "Containers/com.example.App",
        ] {
            fs::create_dir_all(library.join(dir)).unwrap();
        }

        let mut places = places(home.path(), true);
        places.apps.add("com.example.App", "App");
        let rules = rules(&places, &[]);
        let kind = |p: &str| find(&rules, &library.join(p)).map(|r| r.category.clone());

        assert_eq!(kind("Caches/com.example.App").as_deref(), Some("Caches"));
        assert_eq!(kind("Logs/DiagnosticReports").as_deref(), Some("Logs"));
        assert_eq!(
            kind("Application Support/Example").as_deref(),
            Some("App data")
        );
        assert_eq!(
            kind("Saved Application State/com.example.App.savedState").as_deref(),
            Some("Saved state")
        );
        assert_eq!(
            kind("Containers/com.example.App").as_deref(),
            Some("App data")
        );
        assert_eq!(
            kind("Containers/com.example.App/Data/Library/Caches").as_deref(),
            Some("Caches")
        );
    }

    #[test]
    fn nothing_closed_is_listed_or_turned_into_a_rule() {
        let home = tempfile::tempdir().unwrap();
        let containers = home.path().join("Library/Containers");
        fs::create_dir_all(containers.join("com.example.App")).unwrap();

        let rules = rules(
            &places(home.path(), true),
            std::slice::from_ref(&containers),
        );

        assert!(
            rules.iter().all(|r| !r.path.starts_with(&containers)),
            "{rules:?}"
        );
    }

    #[test]
    fn library_is_not_read_on_linux() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("Library/Caches/thing");
        fs::create_dir_all(&cache).unwrap();

        assert!(find(&rules(&places(home.path(), false), &[]), &cache).is_none());
    }

    #[test]
    fn the_trash_inside_xdg_data_is_trash_not_app_data() {
        let home = tempfile::tempdir().unwrap();
        let trash = home.path().join(".local/share/Trash");
        fs::create_dir_all(&trash).unwrap();

        let rules = rules(&places(home.path(), false), &[]);

        let matching: Vec<_> = rules.iter().filter(|r| r.path == trash).collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].category, "Trash");
    }

    #[test]
    fn folders_of_installed_apps_carry_the_app_name_and_removed_ones_their_own_kind() {
        let home = tempfile::tempdir().unwrap();
        let library = home.path().join("Library");
        fs::create_dir_all(library.join("Caches/com.example.Editor")).unwrap();
        fs::create_dir_all(library.join("Application Support/net.gone.Player")).unwrap();
        fs::create_dir_all(library.join("Containers/net.gone.Player")).unwrap();
        let old = filetime::FileTime::from_unix_time(1_000_000_000, 0);
        for gone in [
            "Application Support/net.gone.Player",
            "Containers/net.gone.Player/Data/Library/Caches",
        ] {
            fs::create_dir_all(library.join(gone)).unwrap();
            filetime::set_file_mtime(library.join(gone), old).unwrap();
        }
        let mut places = places(home.path(), true);
        places.apps = InstalledApps::from_bundles(Vec::new());
        places.apps.add("com.example.Editor", "Editor");

        let rules = rules(&places, &[]);
        let at = |p: &str| {
            let r = find(&rules, &library.join(p)).unwrap();
            (r.category.as_str(), r.label.as_str())
        };

        assert_eq!(at("Caches/com.example.Editor"), ("Caches", "Editor"));
        assert_eq!(
            at("Application Support/net.gone.Player"),
            ("Removed apps", "net.gone.Player")
        );
        assert_eq!(
            at("Containers/net.gone.Player/Data/Library/Caches"),
            ("Removed apps", "net.gone.Player")
        );
    }

    #[test]
    fn a_removed_app_folder_changed_this_month_is_not_called_removed() {
        let home = tempfile::tempdir().unwrap();
        let fresh = home
            .path()
            .join("Library/Application Support/net.gone.Player");
        fs::create_dir_all(&fresh).unwrap();

        let mut places = places(home.path(), true);
        places.apps = InstalledApps::from_bundles(Vec::new());
        let rules = rules(&places, &[]);

        assert_eq!(find(&rules, &fresh).unwrap().category, "App data");
    }

    #[test]
    fn a_project_is_named_by_its_folder_and_the_one_holding_it() {
        assert_eq!(
            project_name(Path::new("/home/u/app/src-tauri")),
            "app/src-tauri"
        );
        assert_eq!(project_name(Path::new("/tool")), "tool");
    }

    #[test]
    fn a_folder_carrying_a_valid_cachedir_tag_is_a_cache() {
        let home = tempfile::tempdir().unwrap();
        let tagged = home.path().join(".tool/caches");
        let nested = tagged.join("inner");
        let forged = home.path().join(".tool/notes");
        let visible = home.path().join("work/caches");
        for dir in [&nested, &forged, &visible] {
            fs::create_dir_all(dir).unwrap();
        }
        let signed = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache\n";
        fs::write(tagged.join("CACHEDIR.TAG"), signed).unwrap();
        fs::write(nested.join("CACHEDIR.TAG"), signed).unwrap();
        fs::write(visible.join("CACHEDIR.TAG"), signed).unwrap();
        fs::write(forged.join("CACHEDIR.TAG"), "not a tag").unwrap();

        let rules = rules(&places(home.path(), false), &[]);
        let caches: Vec<&Path> = rules
            .iter()
            .filter(|r| r.category == "Caches" && r.path.starts_with(home.path()))
            .map(|r| r.path.as_path())
            .collect();

        assert_eq!(caches, vec![tagged.as_path()]);
        assert_eq!(find(&rules, &tagged).unwrap().label, ".tool/caches");
    }

    #[test]
    fn every_kind_round_trips_through_its_label() {
        for kind in ALL {
            assert_eq!(Kind::from_label(kind.label()), Some(kind));
        }
    }
}
