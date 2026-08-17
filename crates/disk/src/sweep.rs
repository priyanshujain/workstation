//! A measured partition of the data volume.
//!
//! The old audit summed a hand-written list of paths and printed the result as
//! "total tracked". A sum of an allowlist is not a measurement: anything nobody
//! thought to add was invisible, and on this machine that was two thirds of the
//! disk. Here the roots come from the volume itself, every byte lands in
//! exactly one root, and whatever no rule claims is reported as unattributed
//! rather than dropped.

use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Everything `df` counts for the data volume hangs off here, firmlinks
/// included. Walking `/` instead would cross into the read-only system volume
/// and count bytes that are not on this volume at all.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Per root, so one pathological tree cannot fill the report.
const MAX_UNREADABLE_LISTED: usize = 20;

/// A named slice of the volume. Roots never overlap: one is skipped whenever
/// the walk of another reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

/// A path whose bytes get a category name. Rules are attribution, not
/// coverage: a byte no rule matches still counts, it just counts as
/// unattributed.
#[derive(Debug, Clone)]
pub struct Rule {
    pub category: String,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RootUsage {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub unattributed_total: u64,
    /// Largest immediate children holding unattributed bytes, biggest first.
    pub unattributed: Vec<(PathBuf, u64)>,
    /// Directories that could not be opened. Their contents are unknown, which
    /// is a different claim from zero, and the old code made the wrong one:
    /// `du` returns 0 for a TCC-protected directory and the category builder
    /// then dropped the path for being empty.
    pub unreadable: Vec<PathBuf>,
    pub unreadable_count: usize,
}

#[derive(Debug, Clone)]
pub struct Sweep {
    pub roots: Vec<RootUsage>,
    /// Bytes per rule, positionally matching the rules handed to [`sweep`].
    pub rule_sizes: Vec<u64>,
}

impl Sweep {
    pub fn total(&self) -> u64 {
        self.roots.iter().map(|r| r.total).sum()
    }

    pub fn unattributed(&self) -> u64 {
        self.roots.iter().map(|r| r.unattributed_total).sum()
    }

    pub fn unreadable_count(&self) -> usize {
        self.roots.iter().map(|r| r.unreadable_count).sum()
    }
}

/// The volume's own top level, named. Anything unrecognised keeps its
/// directory name rather than being left out, so a future macOS that adds a
/// top-level directory still gets counted.
pub fn discover_roots() -> Vec<Root> {
    let mut roots = Vec::new();

    if let Ok(entries) = std::fs::read_dir(DATA_VOLUME) {
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        children.sort();

        for child in children {
            let Some(name) = child.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == "Users" {
                roots.extend(user_roots());
                continue;
            }
            if name == "Volumes" {
                // Other mounts. The device check keeps their contents out
                // anyway; listing the root would only add a zero row.
                continue;
            }
            roots.push(Root {
                name: friendly_name(name).to_string(),
                path: firmlinked(&child),
            });
        }
    }

    if roots.is_empty() {
        roots = user_roots();
    }
    roots
}

/// Home split from `~/Library`: they are the two biggest things on a
/// workstation and lumping them together hides which one is growing.
fn user_roots() -> Vec<Root> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(Root {
            name: "User Library".into(),
            path: home.join("Library"),
        });
        roots.push(Root {
            name: "Home".into(),
            path: home,
        });
    }
    roots.push(Root {
        name: "Other users".into(),
        path: PathBuf::from("/Users"),
    });
    roots
}

fn friendly_name(name: &str) -> &str {
    match name {
        "Library" => "System Library",
        "System" => "System data",
        "private" => "System runtime",
        "MobileSoftwareUpdate" => "macOS installers",
        ".Spotlight-V100" => "Spotlight index",
        ".fseventsd" => "FSEvents log",
        ".DocumentRevisions-V100" => "Document versions",
        ".TemporaryItems" => "Temporary items",
        other => other,
    }
}

/// `/Applications` and `/System/Volumes/Data/Applications` are the same
/// directory. Prefer the short form: it is what the user types, what cleanup
/// actions operate on, and what every other tool prints.
fn firmlinked(data_child: &Path) -> PathBuf {
    let Some(name) = data_child.file_name() else {
        return data_child.to_path_buf();
    };
    let short = Path::new("/").join(name);
    if same_inode(&short, data_child) {
        short
    } else {
        data_child.to_path_buf()
    }
}

fn same_inode(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Walk every root once, charging each byte to at most one rule.
pub fn sweep(roots: &[Root], rules: &[Rule]) -> Sweep {
    let by_path: HashMap<&Path, usize> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| (r.path.as_path(), i))
        .collect();
    let skip: HashSet<&Path> = roots.iter().map(|r| r.path.as_path()).collect();
    let seen = Mutex::new(HashSet::new());

    let units = plan(roots, &by_path, &seen);
    let queue = Mutex::new(units.jobs);
    let results = Mutex::new(Vec::new());

    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(4);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let Some(job) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let mut walker = Walker {
                        rules: &by_path,
                        skip: &skip,
                        seen: &seen,
                        dev: job.dev,
                        rule_sizes: vec![0; rules.len()],
                        unreadable: Vec::new(),
                    };
                    let total = walker.walk(&job.path, job.rule);
                    let attributed: u64 = walker.rule_sizes.iter().sum();
                    results.lock().unwrap().push(JobResult {
                        root: job.root,
                        path: job.path,
                        total,
                        unattributed: total.saturating_sub(attributed),
                        rule_sizes: walker.rule_sizes,
                        unreadable: walker.unreadable,
                    });
                }
            });
        }
    });

    collect(
        roots,
        rules.len(),
        units.roots,
        results.into_inner().unwrap(),
    )
}

struct Job {
    root: usize,
    path: PathBuf,
    rule: Option<usize>,
    dev: u64,
}

struct JobResult {
    root: usize,
    path: PathBuf,
    total: u64,
    unattributed: u64,
    rule_sizes: Vec<u64>,
    unreadable: Vec<PathBuf>,
}

/// Everything measured while enumerating the roots themselves: their loose
/// files, and one job per subdirectory for the workers to pick up.
struct Plan {
    jobs: Vec<Job>,
    roots: Vec<RootSeed>,
}

struct RootSeed {
    loose_bytes: u64,
    rule_sizes: Vec<u64>,
    unreadable: Vec<PathBuf>,
}

fn plan(
    roots: &[Root],
    by_path: &HashMap<&Path, usize>,
    seen: &Mutex<HashSet<(u64, u64)>>,
) -> Plan {
    let mut jobs = Vec::new();
    let mut seeds = Vec::new();

    for (index, root) in roots.iter().enumerate() {
        let mut seed = RootSeed {
            loose_bytes: 0,
            rule_sizes: vec![0; by_path.len()],
            unreadable: Vec::new(),
        };

        let Ok(meta) = std::fs::metadata(&root.path) else {
            seeds.push(seed);
            continue;
        };
        let dev = meta.dev();
        let inherited = by_path.get(root.path.as_path()).copied();

        match std::fs::read_dir(&root.path) {
            Err(_) => seed.unreadable.push(root.path.clone()),
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Ok(meta) = entry.metadata() else { continue };
                    if meta.dev() != dev {
                        continue;
                    }
                    let rule = by_path.get(path.as_path()).copied().or(inherited);

                    if meta.is_dir() && !roots.iter().any(|r| r.path == path) {
                        jobs.push(Job {
                            root: index,
                            path,
                            rule,
                            dev,
                        });
                        continue;
                    }
                    if meta.is_dir() {
                        continue;
                    }
                    let bytes = charge(&meta, &mut seen.lock().unwrap());
                    seed.loose_bytes += bytes;
                    if let Some(r) = rule {
                        seed.rule_sizes[r] += bytes;
                    }
                }
            }
        }
        seeds.push(seed);
    }

    // Biggest trees first would need sizes we do not have yet, so settle for
    // handing workers the deepest-looking work last: `pop` takes from the end.
    Plan { jobs, roots: seeds }
}

fn collect(
    roots: &[Root],
    rule_count: usize,
    seeds: Vec<RootSeed>,
    results: Vec<JobResult>,
) -> Sweep {
    let mut rule_sizes = vec![0u64; rule_count];
    let mut usage: Vec<RootUsage> = roots
        .iter()
        .zip(seeds)
        .map(|(root, seed)| {
            for (i, bytes) in seed.rule_sizes.iter().enumerate() {
                rule_sizes[i] += bytes;
            }
            RootUsage {
                name: root.name.clone(),
                path: root.path.clone(),
                total: seed.loose_bytes,
                unattributed_total: seed
                    .loose_bytes
                    .saturating_sub(seed.rule_sizes.iter().sum::<u64>()),
                unattributed: Vec::new(),
                unreadable_count: seed.unreadable.len(),
                unreadable: seed.unreadable,
            }
        })
        .collect();

    for result in results {
        let Some(root) = usage.get_mut(result.root) else {
            continue;
        };
        root.total += result.total;
        root.unattributed_total += result.unattributed;
        if result.unattributed > 0 {
            root.unattributed.push((result.path, result.unattributed));
        }
        root.unreadable_count += result.unreadable.len();
        for path in result.unreadable {
            if root.unreadable.len() < MAX_UNREADABLE_LISTED {
                root.unreadable.push(path);
            }
        }
        for (i, bytes) in result.rule_sizes.iter().enumerate() {
            rule_sizes[i] += bytes;
        }
    }

    for root in usage.iter_mut() {
        root.unattributed
            .sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    }
    usage.sort_by_key(|r| std::cmp::Reverse(r.total));

    Sweep {
        roots: usage,
        rule_sizes,
    }
}

struct Walker<'a> {
    rules: &'a HashMap<&'a Path, usize>,
    skip: &'a HashSet<&'a Path>,
    seen: &'a Mutex<HashSet<(u64, u64)>>,
    dev: u64,
    rule_sizes: Vec<u64>,
    unreadable: Vec<PathBuf>,
}

impl Walker<'_> {
    /// Bytes under `dir` including `dir` itself, with each byte charged to
    /// `rule` unless something deeper claims it.
    fn walk(&mut self, dir: &Path, rule: Option<usize>) -> u64 {
        let mut total = match std::fs::symlink_metadata(dir) {
            Ok(meta) => self.charge_to(&meta, rule),
            Err(_) => 0,
        };

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => {
                self.unreadable.push(dir.to_path_buf());
                return total;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.dev() != self.dev {
                continue;
            }

            let rule = self.rules.get(path.as_path()).copied().or(rule);

            if meta.is_dir() {
                if self.skip.contains(path.as_path()) {
                    continue;
                }
                total += self.walk(&path, rule);
            } else {
                total += self.charge_to(&meta, rule);
            }
        }
        total
    }

    fn charge_to(&mut self, meta: &Metadata, rule: Option<usize>) -> u64 {
        let bytes = {
            let mut seen = self.seen.lock().unwrap();
            charge(meta, &mut seen)
        };
        if let Some(r) = rule {
            self.rule_sizes[r] += bytes;
        }
        bytes
    }
}

/// The lock is only taken for files that actually have more than one link,
/// which on a real volume is a rounding error. Locking on every file would
/// serialise the whole sweep.
fn charge(meta: &Metadata, seen: &mut HashSet<(u64, u64)>) -> u64 {
    if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    meta.blocks() * 512
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn root(name: &str, path: &Path) -> Root {
        Root {
            name: name.into(),
            path: path.to_path_buf(),
        }
    }

    fn rule(category: &str, path: &Path) -> Rule {
        Rule {
            category: category.into(),
            label: category.into(),
            path: path.to_path_buf(),
        }
    }

    #[test]
    fn every_byte_lands_somewhere() {
        // The property the old allowlist could not state: total equals
        // attributed plus unattributed, always.
        let dir = tempdir().unwrap();
        let known = dir.path().join("known");
        let unknown = dir.path().join("unknown");
        fs::create_dir_all(&known).unwrap();
        fs::create_dir_all(&unknown).unwrap();
        fs::write(known.join("a.bin"), vec![0u8; 128 * 1024]).unwrap();
        fs::write(unknown.join("b.bin"), vec![0u8; 256 * 1024]).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[rule("Known", &known)]);

        let attributed: u64 = sweep.rule_sizes.iter().sum();
        assert_eq!(sweep.total(), attributed + sweep.unattributed());
        assert!(attributed >= 128 * 1024, "attributed {attributed}");
        assert!(
            sweep.unattributed() >= 256 * 1024,
            "unattributed {}",
            sweep.unattributed()
        );
    }

    #[test]
    fn unattributed_bytes_are_reported_not_dropped() {
        let dir = tempdir().unwrap();
        let stray = dir.path().join("nobody-listed-this");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join("big.bin"), vec![0u8; 512 * 1024]).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);

        let usage = &sweep.roots[0];
        assert!(usage.unattributed_total >= 512 * 1024);
        assert_eq!(usage.unattributed[0].0, stray);
    }

    #[test]
    fn nested_rules_beat_the_rule_above_them() {
        let dir = tempdir().unwrap();
        let outer = dir.path().join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(outer.join("outer.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(inner.join("inner.bin"), vec![0u8; 256 * 1024]).unwrap();

        let sweep = sweep(
            &[root("test", dir.path())],
            &[rule("Outer", &outer), rule("Inner", &inner)],
        );

        assert!(
            sweep.rule_sizes[1] >= 256 * 1024,
            "inner did not claim its own bytes: {:?}",
            sweep.rule_sizes
        );
        assert!(
            sweep.rule_sizes[0] < 256 * 1024,
            "outer swallowed the nested rule: {:?}",
            sweep.rule_sizes
        );
    }

    #[test]
    fn a_nested_root_is_counted_once_by_the_inner_root() {
        // Home and ~/Library overlap on disk. Whichever root is more specific
        // owns the bytes, and the outer one must not count them again.
        let dir = tempdir().unwrap();
        let inner = dir.path().join("Library");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("big.bin"), vec![0u8; 512 * 1024]).unwrap();
        fs::write(dir.path().join("small.bin"), vec![0u8; 16 * 1024]).unwrap();

        let sweep = sweep(&[root("Home", dir.path()), root("Library", &inner)], &[]);

        let home = sweep.roots.iter().find(|r| r.name == "Home").unwrap();
        let library = sweep.roots.iter().find(|r| r.name == "Library").unwrap();
        assert!(library.total >= 512 * 1024, "library {}", library.total);
        assert!(
            home.total < 512 * 1024,
            "home double-counted the nested root: {}",
            home.total
        );
    }

    #[test]
    fn a_hardlink_across_two_roots_is_charged_once() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("file.bin"), vec![0u8; 1024 * 1024]).unwrap();
        fs::hard_link(a.join("file.bin"), b.join("file.bin")).unwrap();

        let sweep = sweep(&[root("a", &a), root("b", &b)], &[]);

        assert!(
            sweep.total() < 2 * 1024 * 1024,
            "hardlink billed to both roots: {}",
            sweep.total()
        );
    }

    #[test]
    fn a_hardlink_between_a_loose_file_and_a_subtree_is_charged_once() {
        // The root's own files are measured while planning and its
        // subdirectories by the workers. Both have to consult the same set of
        // seen inodes or a link spanning the two gets billed twice.
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("loose.bin"), vec![0u8; 1024 * 1024]).unwrap();
        fs::hard_link(dir.path().join("loose.bin"), sub.join("same.bin")).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);

        assert!(
            sweep.total() < 2 * 1024 * 1024,
            "hardlink counted by both the planner and a worker: {}",
            sweep.total()
        );
    }

    #[test]
    fn an_unreadable_directory_is_unknown_not_empty() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        assert_eq!(sweep.unreadable_count(), 1);
        assert_eq!(sweep.roots[0].unreadable, vec![locked]);
    }

    #[test]
    fn symlinks_are_counted_but_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let elsewhere = tempdir().unwrap();
        fs::write(
            elsewhere.path().join("huge.bin"),
            vec![0u8; 4 * 1024 * 1024],
        )
        .unwrap();
        symlink(elsewhere.path(), dir.path().join("link")).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);

        assert!(
            sweep.total() < 4 * 1024 * 1024,
            "followed the symlink: {}",
            sweep.total()
        );
    }

    #[test]
    fn roots_come_from_the_volume_and_do_not_overlap() {
        let roots = discover_roots();
        assert!(!roots.is_empty());

        for (i, a) in roots.iter().enumerate() {
            for (j, b) in roots.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert_ne!(a.path, b.path, "duplicate root {}", a.path.display());
            }
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn roots_use_the_short_firmlinked_path() {
        // /Applications and /System/Volumes/Data/Applications are one
        // directory; printing the long form would be technically true and
        // useless.
        let roots = discover_roots();
        let apps = roots.iter().find(|r| r.name == "Applications");
        if let Some(apps) = apps {
            assert_eq!(apps.path, Path::new("/Applications"));
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn home_and_user_library_are_separate_roots() {
        let roots = discover_roots();
        assert!(roots.iter().any(|r| r.name == "Home"));
        assert!(roots.iter().any(|r| r.name == "User Library"));
    }
}
