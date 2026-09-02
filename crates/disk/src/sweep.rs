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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Everything `df` counts for the data volume hangs off here, firmlinks
/// included. Walking `/` instead would cross into the read-only system volume
/// and count bytes that are not on this volume at all.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Per root, so one pathological tree cannot fill the report.
const MAX_UNREADABLE_LISTED: usize = 20;

/// How often a caller hears from a sweep that has nothing new to report. Slow
/// enough to cost nothing, fast enough that a UI stays responsive to keys.
const TICK: Duration = Duration::from_millis(80);

/// Why a directory would not open. The two look identical through
/// `io::ErrorKind::PermissionDenied` and have completely different fixes, so
/// telling a user to grant Full Disk Access for a root-owned directory is
/// advice that cannot work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Denial {
    /// EPERM. macOS privacy policy: Full Disk Access for the terminal app
    /// running the scan lifts it.
    Protected,
    /// EACCES. Ordinary unix permissions: needs sudo, and no amount of Full
    /// Disk Access will help.
    Forbidden,
    /// Never tried. macOS puts a dialog on the screen before it lets a process
    /// open this directory, and a scan nobody is sitting at has nobody to
    /// answer it, so the walk left it closed. A scan somebody is present for
    /// measures it.
    Unattended,
}

impl Denial {
    /// The one place errno is read. Duplicating this is how the two kinds of
    /// refusal drift apart and start attracting advice that cannot work.
    pub fn of(error: &std::io::Error) -> Self {
        match error.raw_os_error() {
            Some(1) => Denial::Protected,
            _ => Denial::Forbidden,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Unreadable {
    pub path: PathBuf,
    pub denial: Denial,
}

/// The application Full Disk Access has to be granted to, named the way its
/// entry in System Settings is.
///
/// macOS attaches the grant to the responsible process, which is the terminal
/// application owning the session and never the `wsctl` binary, so advice that
/// names anything else is advice that cannot work.
pub fn full_disk_access_app() -> String {
    app_name(std::env::var("TERM_PROGRAM").ok().as_deref())
}

fn app_name(term_program: Option<&str>) -> String {
    let Some(name) = term_program.map(str::trim).filter(|n| !n.is_empty()) else {
        return "your terminal app".to_string();
    };
    let name = name.strip_suffix(".app").unwrap_or(name);
    match name {
        "Apple_Terminal" => "Terminal".to_string(),
        "vscode" => "VS Code".to_string(),
        _ if name.chars().any(char::is_uppercase) => name.to_string(),
        _ => {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => name.to_string(),
            }
        }
    }
}

/// Exact counts, kept separately from the sample in [`RootUsage::unreadable`]
/// because that sample is capped. Counting the sample instead would report a
/// split of the twenty paths that happened to be retained, under a headline
/// number describing all of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Denials {
    pub protected: usize,
    pub forbidden: usize,
    pub unattended: usize,
}

impl Denials {
    pub fn one(denial: Denial) -> Self {
        let mut one = Self::default();
        one.record(denial);
        one
    }

    pub fn total(&self) -> usize {
        self.protected + self.forbidden + self.unattended
    }

    pub fn of(&self, denial: Denial) -> usize {
        match denial {
            Denial::Protected => self.protected,
            Denial::Forbidden => self.forbidden,
            Denial::Unattended => self.unattended,
        }
    }

    pub fn record(&mut self, denial: Denial) {
        match denial {
            Denial::Protected => self.protected += 1,
            Denial::Forbidden => self.forbidden += 1,
            Denial::Unattended => self.unattended += 1,
        }
    }

    pub fn merge(&mut self, other: Denials) {
        self.protected += other.protected;
        self.forbidden += other.forbidden;
        self.unattended += other.unattended;
    }
}

/// What a sweep says while it is still running, so a caller can show rows
/// filling in instead of a blank screen for two minutes.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Always first. The rows to draw, in the order they were discovered.
    Started { roots: Vec<Root>, jobs: usize },
    /// One immediate child of a root finished. `root_bytes` is that root's
    /// running total, already including its loose files.
    Scanned {
        root: usize,
        path: PathBuf,
        root_bytes: u64,
        remaining: usize,
    },
    /// Every job for this root is in. Its number is final, and so is the
    /// count of what refused to open: a root that finished at zero bytes with
    /// refusals is unknown, not empty, and a UI has to be able to say so
    /// while the rest of the walk is still running.
    RootDone {
        root: usize,
        denials: Denials,
        /// The root's total, loose files included. Carried here because jobs are
        /// per subdirectory, so a root that has none never gets a `Scanned`
        /// event and a live consumer would otherwise have no number for it until
        /// the whole sweep ended.
        bytes: u64,
    },
    /// Nothing new. Emitted every `TICK` so a UI can poll for keys.
    Tick,
}

/// A caller's answer to each [`Progress`]. Returning `Cancel` stops the walk;
/// workers notice between directories, so it takes effect within milliseconds
/// rather than at the end of whatever subtree is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Cancel,
}

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
    /// Every immediate child with its subtree size, biggest first. The walk
    /// already measures these to reach the root total, so keeping them turns
    /// the first level of drill-down into a lookup instead of a second walk.
    pub children: Vec<(PathBuf, u64)>,
    /// The immediate children whose own directory would not open. Their size is
    /// neither zero nor merely understated, it is unknown: nothing ever looked
    /// inside them. `children` cannot say so, because a refused job still
    /// reports the zero bytes it managed to count, and `unreadable` below is a
    /// capped sample of refusals at any depth. Immediate children are few, so
    /// this one is exact and uncapped.
    pub refused_children: Vec<Unreadable>,
    pub unattributed_total: u64,
    /// Largest immediate children holding unattributed bytes, biggest first.
    pub unattributed: Vec<(PathBuf, u64)>,
    /// A capped sample of the directories that could not be opened. Their
    /// contents are unknown, which is a different claim from zero, and the old
    /// code made the wrong one: `du` returns 0 for a TCC-protected directory
    /// and the category builder then dropped the path for being empty.
    pub unreadable: Vec<Unreadable>,
    /// Exact, unlike the length of the sample above.
    pub denials: Denials,
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

    pub fn denials(&self) -> Denials {
        let mut total = Denials::default();
        for root in &self.roots {
            total.merge(root.denials);
        }
        total
    }

    pub fn unreadable_count(&self) -> usize {
        self.denials().total()
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
    roots.sort_by_key(|r| (display_rank(&r.name), r.name.clone()));
    roots
}

/// Discovery order is the volume's own, which puts `cores`, `mnt` and `pkg`
/// above the home directory. Anything reading top to bottom wants the
/// opposite, so rank the ones worth looking at first and let the rest fall in
/// alphabetically behind them.
fn display_rank(name: &str) -> u8 {
    match name {
        "Home" => 0,
        "User Library" => 1,
        "Applications" => 2,
        "System Library" => 3,
        "opt" => 4,
        "System data" => 5,
        "System runtime" => 6,
        "usr" => 7,
        "Other users" => 8,
        _ => 9,
    }
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
    sweep_unattended(roots, rules, &[])
}

/// The same walk with `closed` left unopened, each recorded as
/// [`Denial::Unattended`] so its bytes read as unknown rather than zero.
///
/// This is the walk for a scan nobody is sitting at. Opening a directory
/// macOS asks about first puts a dialog on the screen and blocks the open
/// until somebody answers, so a scheduled scan that tries is a scan that
/// nags, then hangs.
pub fn sweep_unattended(roots: &[Root], rules: &[Rule], closed: &[PathBuf]) -> Sweep {
    sweep_unattended_streaming(roots, rules, closed, &mut |_| Flow::Continue)
        .expect("a sweep that is never cancelled always finishes")
}

/// The same walk, reporting as it goes.
///
/// `on_progress` runs on the calling thread, never on a worker, so it can draw
/// to the terminal without a lock. `None` means the caller cancelled.
pub fn sweep_streaming(
    roots: &[Root],
    rules: &[Rule],
    on_progress: &mut dyn FnMut(Progress) -> Flow,
) -> Option<Sweep> {
    sweep_unattended_streaming(roots, rules, &[], on_progress)
}

pub fn sweep_unattended_streaming(
    roots: &[Root],
    rules: &[Rule],
    closed: &[PathBuf],
    on_progress: &mut dyn FnMut(Progress) -> Flow,
) -> Option<Sweep> {
    let by_path: HashMap<&Path, usize> = rules
        .iter()
        .enumerate()
        .map(|(i, r)| (r.path.as_path(), i))
        .collect();
    let skip: HashSet<&Path> = roots.iter().map(|r| r.path.as_path()).collect();
    let closed: HashSet<&Path> = closed.iter().map(PathBuf::as_path).collect();
    let seen = Mutex::new(HashSet::new());

    let units = plan(roots, &by_path, &closed, &seen);

    // Loose files are already measured, so a root with nothing but files is
    // final before a single worker starts.
    let mut root_bytes: Vec<u64> = units.roots.iter().map(|s| s.loose_bytes).collect();
    let mut root_denials: Vec<Denials> = units
        .roots
        .iter()
        .map(|s| count_denials(&s.unreadable))
        .collect();
    let mut root_pending = vec![0usize; roots.len()];
    for job in &units.jobs {
        root_pending[job.root] += 1;
    }

    if on_progress(Progress::Started {
        roots: roots.to_vec(),
        jobs: units.jobs.len(),
    }) == Flow::Cancel
    {
        return None;
    }
    for (index, pending) in root_pending.iter().enumerate() {
        if *pending == 0
            && on_progress(Progress::RootDone {
                root: index,
                denials: root_denials[index],
                bytes: root_bytes[index],
            }) == Flow::Cancel
        {
            return None;
        }
    }

    let mut remaining = units.jobs.len();
    let queue = Mutex::new(units.jobs);
    let cancel = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();

    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(4);

    let results = std::thread::scope(|scope| {
        for _ in 0..threads {
            let tx = tx.clone();
            let queue = &queue;
            let cancel = &cancel;
            let (by_path, skip, closed, seen) = (&by_path, &skip, &closed, &seen);
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) {
                    let Some(job) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let mut walker = Walker {
                        rules: by_path,
                        skip,
                        closed,
                        seen,
                        cancel,
                        dev: job.dev,
                        rule_sizes: vec![0; rules.len()],
                        unreadable: Vec::new(),
                    };
                    let total = walker.walk(&job.path, job.rule);
                    let attributed: u64 = walker.rule_sizes.iter().sum();
                    let refused = walker.refusal_at(&job.path);
                    // A closed receiver means the caller cancelled and went
                    // away; there is nothing useful left to do.
                    if tx
                        .send(JobResult {
                            root: job.root,
                            path: job.path,
                            total,
                            refused,
                            unattributed: total.saturating_sub(attributed),
                            rule_sizes: walker.rule_sizes,
                            unreadable: walker.unreadable,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        // The workers hold the only senders now, so the channel closes exactly
        // when the last of them stops.
        drop(tx);

        let mut results = Vec::new();
        loop {
            let flow = match rx.recv_timeout(TICK) {
                Ok(result) => {
                    remaining -= 1;
                    root_bytes[result.root] += result.total;
                    root_denials[result.root].merge(count_denials(&result.unreadable));
                    root_pending[result.root] -= 1;
                    let finished = root_pending[result.root] == 0;
                    let progress = Progress::Scanned {
                        root: result.root,
                        path: result.path.clone(),
                        root_bytes: root_bytes[result.root],
                        remaining,
                    };
                    let root = result.root;
                    results.push(result);
                    match on_progress(progress) {
                        Flow::Cancel => Flow::Cancel,
                        Flow::Continue if finished => on_progress(Progress::RootDone {
                            root,
                            denials: root_denials[root],
                            bytes: root_bytes[root],
                        }),
                        flow => flow,
                    }
                }
                Err(RecvTimeoutError::Timeout) => on_progress(Progress::Tick),
                Err(RecvTimeoutError::Disconnected) => break Some(results),
            };
            if flow == Flow::Cancel {
                cancel.store(true, Ordering::Relaxed);
                break None;
            }
        }
    })?;

    Some(collect(roots, rules.len(), units.roots, results))
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
    /// Set when this child's own directory would not open, as opposed to
    /// something deeper inside it refusing.
    refused: Option<Denial>,
    unattributed: u64,
    rule_sizes: Vec<u64>,
    unreadable: Vec<Unreadable>,
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
    unreadable: Vec<Unreadable>,
}

fn plan(
    roots: &[Root],
    by_path: &HashMap<&Path, usize>,
    closed: &HashSet<&Path>,
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

        let meta = match std::fs::metadata(&root.path) {
            Ok(meta) => meta,
            Err(e) => {
                // This returns before the read_dir below, which is the only
                // other place a refusal gets recorded, so a root nobody is
                // allowed to stat would otherwise arrive with no bytes AND no
                // denials: exactly how an empty directory arrives.
                seed.unreadable.extend(refusal(&root.path, &e));
                seeds.push(seed);
                continue;
            }
        };
        let dev = meta.dev();
        let inherited = by_path.get(root.path.as_path()).copied();

        if closed.contains(root.path.as_path()) {
            seed.unreadable.push(unattended(&root.path));
            seeds.push(seed);
            continue;
        }

        match std::fs::read_dir(&root.path) {
            Err(e) => seed.unreadable.extend(refusal(&root.path, &e)),
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
                children: Vec::new(),
                refused_children: Vec::new(),
                unattributed_total: seed
                    .loose_bytes
                    .saturating_sub(seed.rule_sizes.iter().sum::<u64>()),
                unattributed: Vec::new(),
                denials: count_denials(&seed.unreadable),
                unreadable: seed.unreadable,
            }
        })
        .collect();

    for result in results {
        let Some(root) = usage.get_mut(result.root) else {
            continue;
        };
        if let Some(denial) = result.refused {
            root.refused_children.push(Unreadable {
                path: result.path.clone(),
                denial,
            });
        }
        root.total += result.total;
        root.children.push((result.path.clone(), result.total));
        root.unattributed_total += result.unattributed;
        if result.unattributed > 0 {
            root.unattributed.push((result.path, result.unattributed));
        }
        root.denials.merge(count_denials(&result.unreadable));
        for entry in result.unreadable {
            if root.unreadable.len() < MAX_UNREADABLE_LISTED {
                root.unreadable.push(entry);
            }
        }
        for (i, bytes) in result.rule_sizes.iter().enumerate() {
            rule_sizes[i] += bytes;
        }
    }

    for root in usage.iter_mut() {
        root.children
            .sort_by_key(|(_, size)| std::cmp::Reverse(*size));
        root.unattributed
            .sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    }
    Sweep {
        roots: usage,
        rule_sizes,
    }
}

/// Only a refusal is a refusal. `metadata` and `read_dir` fail the same way for
/// a path that is gone (ENOENT) and for one that is not a directory (ENOTDIR),
/// and calling either of those a permission problem sends the user off to try
/// sudo on something sudo cannot fix. A root that is not there holds nothing;
/// a root that will not open holds an unknown amount.
fn refusal(path: &Path, error: &std::io::Error) -> Option<Unreadable> {
    (error.kind() == std::io::ErrorKind::PermissionDenied).then(|| Unreadable {
        path: path.to_path_buf(),
        denial: Denial::of(error),
    })
}

fn unattended(path: &Path) -> Unreadable {
    Unreadable {
        path: path.to_path_buf(),
        denial: Denial::Unattended,
    }
}

fn count_denials(entries: &[Unreadable]) -> Denials {
    let mut denials = Denials::default();
    for entry in entries {
        denials.record(entry.denial);
    }
    denials
}

struct Walker<'a> {
    rules: &'a HashMap<&'a Path, usize>,
    skip: &'a HashSet<&'a Path>,
    /// Never opened. Checked before every `read_dir`, because the dialog is
    /// raised by the open itself and nothing after it can take it back.
    closed: &'a HashSet<&'a Path>,
    seen: &'a Mutex<HashSet<(u64, u64)>>,
    cancel: &'a AtomicBool,
    dev: u64,
    rule_sizes: Vec<u64>,
    unreadable: Vec<Unreadable>,
}

impl Walker<'_> {
    /// Bytes under `dir` including `dir` itself, with each byte charged to
    /// `rule` unless something deeper claims it.
    fn walk(&mut self, dir: &Path, rule: Option<usize>) -> u64 {
        let mut total = match std::fs::symlink_metadata(dir) {
            Ok(meta) => self.charge_to(&meta, rule),
            Err(_) => 0,
        };

        if self.closed.contains(dir) {
            self.unreadable.push(unattended(dir));
            return total;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.unreadable.push(Unreadable {
                    path: dir.to_path_buf(),
                    denial: Denial::of(&e),
                });
                return total;
            }
        };

        // Checked per directory rather than per file: cheap enough to be free
        // at this rate, frequent enough that cancelling feels immediate.
        if self.cancel.load(Ordering::Relaxed) {
            return total;
        }

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

    /// Why `dir` itself would not open, if it would not.
    ///
    /// Only the outermost [`Walker::walk`] can record a refusal at its own
    /// starting directory; every deeper one records a strict descendant. That is
    /// what tells "this child is unknown" apart from "this child is measured,
    /// and understated by something inside it".
    fn refusal_at(&self, dir: &Path) -> Option<Denial> {
        self.unreadable
            .iter()
            .find(|entry| entry.path == dir)
            .map(|entry| entry.denial)
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
    fn every_immediate_child_is_kept_with_its_size() {
        // Drilling one level should not need a second walk: the sweep already
        // measured each child to arrive at the root total.
        let dir = tempdir().unwrap();
        for (name, size) in [("big", 512 * 1024), ("small", 16 * 1024)] {
            let sub = dir.path().join(name);
            fs::create_dir(&sub).unwrap();
            fs::write(sub.join("f.bin"), vec![0u8; size]).unwrap();
        }

        let sweep = sweep(&[root("test", dir.path())], &[]);
        let children = &sweep.roots[0].children;

        assert_eq!(children.len(), 2);
        assert!(children[0].0.ends_with("big"), "not biggest first");
        assert!(children[0].1 >= 512 * 1024);
        assert_eq!(
            children.iter().map(|(_, s)| s).sum::<u64>(),
            sweep.roots[0].total,
            "the children must account for the root"
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
        assert_eq!(sweep.roots[0].unreadable[0].path, locked);
        // chmod 000 is a unix refusal, not a privacy one. Telling the user to
        // grant Full Disk Access here would be advice that cannot work.
        assert_eq!(sweep.roots[0].unreadable[0].denial, Denial::Forbidden);
        assert_eq!(sweep.denials().forbidden, 1);
        assert_eq!(sweep.denials().protected, 0);
    }

    #[test]
    fn a_refused_immediate_child_is_named_as_refused() {
        use std::os::unix::fs::PermissionsExt;

        // The zero in `children` is the whole bug: a job whose directory would
        // not open still reports the bytes it could count, which is none of
        // them, and nothing else in the root said why.
        let dir = tempdir().unwrap();
        let locked = dir.path().join("locked");
        let open = dir.path().join("open");
        fs::create_dir(&locked).unwrap();
        fs::create_dir(&open).unwrap();
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(open.join("plain.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        let usage = &sweep.roots[0];
        assert_eq!(usage.refused_children.len(), 1);
        assert_eq!(usage.refused_children[0].path, locked);
        assert_eq!(usage.refused_children[0].denial, Denial::Forbidden);

        let sized = usage
            .children
            .iter()
            .find(|(path, _)| *path == locked)
            .expect("a refused child is still a child");
        assert!(
            sized.1 < 64 * 1024,
            "children reports what the walk could count, which is why it cannot \
             be the only thing a reader consults: {}",
            sized.1
        );
    }

    #[test]
    fn a_refusal_deeper_down_leaves_the_child_measured() {
        use std::os::unix::fs::PermissionsExt;

        // The child opened, so its number is real and merely understated.
        // Calling it unknown would throw away a measurement.
        let dir = tempdir().unwrap();
        let child = dir.path().join("child");
        let locked = child.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(child.join("plain.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let sweep = sweep(&[root("test", dir.path())], &[]);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        let usage = &sweep.roots[0];
        assert!(
            usage.refused_children.is_empty(),
            "the refusal is a grandchild: {:?}",
            usage.refused_children
        );
        assert_eq!(usage.children.len(), 1);
        assert!(usage.children[0].1 >= 64 * 1024);
        assert_eq!(sweep.denials().forbidden, 1, "still reported as a refusal");
    }

    #[test]
    fn a_closed_directory_is_left_unopened_and_reads_as_unknown() {
        // The dialog is raised by the open, so the only way to not raise it is
        // to not open. Nothing inside gets counted, and the row says why.
        let dir = tempdir().unwrap();
        let gated = dir.path().join("Documents");
        let open = dir.path().join("open");
        fs::create_dir(&gated).unwrap();
        fs::create_dir(&open).unwrap();
        fs::write(gated.join("secret.bin"), vec![0u8; 256 * 1024]).unwrap();
        fs::write(open.join("plain.bin"), vec![0u8; 64 * 1024]).unwrap();

        let swept = sweep_unattended(
            &[root("test", dir.path())],
            &[],
            std::slice::from_ref(&gated),
        );

        let usage = &swept.roots[0];
        assert!(
            swept.total() < 256 * 1024,
            "the closed directory was opened: {}",
            swept.total()
        );
        assert!(swept.total() >= 64 * 1024, "the open one was not measured");
        assert_eq!(usage.refused_children.len(), 1);
        assert_eq!(usage.refused_children[0].path, gated);
        assert_eq!(usage.refused_children[0].denial, Denial::Unattended);
        assert_eq!(
            swept.denials(),
            Denials {
                unattended: 1,
                ..Denials::default()
            }
        );
    }

    #[test]
    fn a_closed_directory_deeper_down_is_left_unopened_too() {
        // The check is per open, not per root child, or a gated directory
        // two levels down would still raise its dialog.
        let dir = tempdir().unwrap();
        let child = dir.path().join("child");
        let gated = child.join("Photos Library.photoslibrary");
        fs::create_dir_all(&gated).unwrap();
        fs::write(child.join("plain.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::write(gated.join("secret.bin"), vec![0u8; 256 * 1024]).unwrap();

        let swept = sweep_unattended(
            &[root("test", dir.path())],
            &[],
            std::slice::from_ref(&gated),
        );

        let usage = &swept.roots[0];
        assert!(swept.total() < 256 * 1024, "{}", swept.total());
        assert!(usage.refused_children.is_empty(), "the child itself opened");
        assert_eq!(usage.unreadable.len(), 1);
        assert_eq!(usage.unreadable[0].path, gated);
        assert_eq!(usage.unreadable[0].denial, Denial::Unattended);
    }

    #[test]
    fn a_closed_root_is_unknown_not_empty() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("f.bin"), vec![0u8; 64 * 1024]).unwrap();

        let swept = sweep_unattended(
            &[root("gated", dir.path())],
            &[],
            &[dir.path().to_path_buf()],
        );

        assert_eq!(swept.total(), 0);
        assert_eq!(swept.denials().unattended, 1);
        assert_eq!(swept.roots[0].unreadable[0].denial, Denial::Unattended);
    }

    #[test]
    fn an_attended_sweep_opens_everything() {
        // The list is the only thing that closes a directory. With nobody
        // handing one in, the walk is the walk it always was.
        let dir = tempdir().unwrap();
        let gated = dir.path().join("Documents");
        fs::create_dir(&gated).unwrap();
        fs::write(gated.join("secret.bin"), vec![0u8; 256 * 1024]).unwrap();

        let swept = sweep(&[root("test", dir.path())], &[]);

        assert!(swept.total() >= 256 * 1024, "{}", swept.total());
        assert_eq!(swept.denials().total(), 0);
    }

    #[test]
    fn the_terminal_named_in_the_advice_is_the_one_running_the_scan() {
        // Full Disk Access attaches to the terminal application owning the
        // session, so naming anything else is advice that cannot work.
        assert_eq!(app_name(Some("ghostty")), "Ghostty");
        assert_eq!(app_name(Some("Apple_Terminal")), "Terminal");
        assert_eq!(app_name(Some("iTerm.app")), "iTerm");
        assert_eq!(app_name(Some("vscode")), "VS Code");
        assert_eq!(app_name(Some("WezTerm")), "WezTerm");
    }

    #[test]
    fn advice_stays_generic_when_the_terminal_is_unknown() {
        assert_eq!(app_name(None), "your terminal app");
        assert_eq!(app_name(Some("   ")), "your terminal app");
        assert!(
            !full_disk_access_app().is_empty(),
            "the advice always names something"
        );
    }

    #[test]
    fn denial_counts_survive_the_sample_cap() {
        use std::os::unix::fs::PermissionsExt;

        // The sample is capped so one bad tree cannot fill the report, which
        // means counting the sample would understate the truth. Anything
        // printed as a count has to come from the counters instead.
        let dir = tempdir().unwrap();
        let holder = dir.path().join("holder");
        fs::create_dir(&holder).unwrap();
        let locked_count = MAX_UNREADABLE_LISTED + 7;
        for i in 0..locked_count {
            let locked = holder.join(format!("locked{i}"));
            fs::create_dir(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        }

        let sweep = sweep(&[root("test", dir.path())], &[]);
        for i in 0..locked_count {
            let _ = fs::set_permissions(
                holder.join(format!("locked{i}")),
                fs::Permissions::from_mode(0o755),
            );
        }

        assert_eq!(sweep.roots[0].unreadable.len(), MAX_UNREADABLE_LISTED);
        assert_eq!(sweep.unreadable_count(), locked_count);
        assert_eq!(sweep.denials().forbidden, locked_count);
    }

    #[test]
    fn streaming_announces_roots_then_finishes_every_one() {
        let dir = tempdir().unwrap();
        for name in ["a", "b"] {
            let sub = dir.path().join(name);
            fs::create_dir(&sub).unwrap();
            fs::write(sub.join("f.bin"), vec![0u8; 128 * 1024]).unwrap();
        }

        let mut events = Vec::new();
        let result = sweep_streaming(&[root("test", dir.path())], &[], &mut |p| {
            events.push(p);
            Flow::Continue
        });

        let swept = result.expect("not cancelled");
        assert!(matches!(events.first(), Some(Progress::Started { .. })));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Progress::RootDone { .. }))
                .count(),
            1,
            "each root must be finished exactly once"
        );
        let scanned: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Progress::Scanned { root_bytes, .. } => Some(*root_bytes),
                _ => None,
            })
            .collect();
        assert_eq!(scanned.len(), 2, "one event per immediate child");
        assert!(
            scanned.windows(2).all(|w| w[0] <= w[1]),
            "the running total must only grow: {scanned:?}"
        );
        assert_eq!(*scanned.last().unwrap(), swept.total());
    }

    #[test]
    fn a_root_with_no_subdirectories_is_finished_before_any_worker_runs() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("only.bin"), vec![0u8; 64 * 1024]).unwrap();

        let mut events = Vec::new();
        let swept = sweep_streaming(&[root("test", dir.path())], &[], &mut |p| {
            events.push(p);
            Flow::Continue
        })
        .unwrap();

        let Some(Progress::RootDone { root: 0, bytes, .. }) = events.get(1) else {
            panic!(
                "a root with only loose files never gets a job, so nothing else \
                 would ever mark it done: {events:?}"
            );
        };
        // Jobs are per subdirectory, so this root emits no Scanned event at all.
        // Without the bytes on this event a live consumer has nothing to draw
        // until the whole sweep ends, and had to read the directory again to
        // find out.
        assert!(*bytes >= 64 * 1024, "loose files went missing: {bytes}");
        assert_eq!(*bytes, swept.total());
        assert!(
            !events.iter().any(|e| matches!(e, Progress::Scanned { .. })),
            "nothing was ever scanned, which is the point: {events:?}"
        );
    }

    #[test]
    fn a_root_reports_the_same_bytes_when_it_finishes_as_the_sweep_does() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("loose.bin"), vec![0u8; 32 * 1024]).unwrap();
        for name in ["a", "b"] {
            let sub = dir.path().join(name);
            fs::create_dir(&sub).unwrap();
            fs::write(sub.join("f.bin"), vec![0u8; 128 * 1024]).unwrap();
        }

        let mut done = Vec::new();
        let swept = sweep_streaming(&[root("test", dir.path())], &[], &mut |p| {
            if let Progress::RootDone { bytes, .. } = p {
                done.push(bytes);
            }
            Flow::Continue
        })
        .unwrap();

        assert_eq!(done, vec![swept.roots[0].total]);
        assert!(done[0] >= 32 * 1024 + 256 * 1024, "{done:?}");
    }

    #[test]
    fn a_root_that_cannot_be_stated_is_unknown_rather_than_empty() {
        use std::os::unix::fs::PermissionsExt;

        // Stat is refused when a parent directory cannot be traversed, and it is
        // the first thing the planner does, so this refusal used to be dropped
        // before the code that records one ever ran. Zero bytes and zero denials
        // is exactly how an empty directory arrives.
        let dir = tempdir().unwrap();
        let closed = dir.path().join("closed");
        let inside = closed.join("inside");
        fs::create_dir_all(&inside).unwrap();
        fs::write(inside.join("f.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();

        let mut done = Vec::new();
        let swept = sweep_streaming(&[root("inside", &inside)], &[], &mut |p| {
            if let Progress::RootDone { denials, bytes, .. } = p {
                done.push((denials, bytes));
            }
            Flow::Continue
        });
        let _ = fs::set_permissions(&closed, fs::Permissions::from_mode(0o755));

        let swept = swept.expect("not cancelled");
        assert_eq!(swept.roots[0].total, 0);
        assert_eq!(swept.denials().total(), 1, "the refusal must be recorded");
        assert_eq!(done, vec![(swept.roots[0].denials, 0)]);
    }

    #[test]
    fn a_root_that_is_not_there_is_not_a_refusal() {
        // Nothing to measure and nothing to grant: advice about sudo or Full
        // Disk Access here would be advice about a path that does not exist.
        let dir = tempdir().unwrap();
        let swept = sweep(&[root("gone", &dir.path().join("gone"))], &[]);

        assert_eq!(swept.total(), 0);
        assert_eq!(swept.denials().total(), 0);
    }

    #[test]
    fn cancelling_stops_the_walk_and_returns_nothing() {
        let dir = tempdir().unwrap();
        for i in 0..8 {
            let sub = dir.path().join(format!("sub{i}"));
            fs::create_dir(&sub).unwrap();
            fs::write(sub.join("f.bin"), vec![0u8; 64 * 1024]).unwrap();
        }

        let mut seen = 0;
        let result = sweep_streaming(&[root("test", dir.path())], &[], &mut |p| {
            if matches!(p, Progress::Scanned { .. }) {
                seen += 1;
                if seen == 1 {
                    return Flow::Cancel;
                }
            }
            Flow::Continue
        });

        assert!(result.is_none(), "a cancelled sweep has no result to give");
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

    #[test]
    #[cfg(target_os = "macos")]
    fn the_roots_worth_reading_come_first() {
        // The volume lists cores, mnt and pkg before Users. Anything reading
        // top to bottom wants the home directory first.
        let names: Vec<String> = discover_roots().into_iter().map(|r| r.name).collect();
        assert_eq!(names[0], "Home");
        assert_eq!(names[1], "User Library");
        let home = names.iter().position(|n| n == "Home").unwrap();
        for trivial in ["cores", "mnt", "pkg"] {
            if let Some(at) = names.iter().position(|n| n == trivial) {
                assert!(at > home, "{trivial} ranked above Home");
            }
        }
    }
}
