//! Where the volume's bytes went, drawn as they are measured.
//!
//! Two views of one measurement, as the plain report prints them: the partition,
//! where every byte lands in exactly one root, and the naming layer over the
//! same bytes. `Tab` switches between them.
//!
//! The rows exist before a single byte is counted, so a list has its final shape
//! from the first frame and fills in rather than appearing all at once. Order is
//! left alone while a walk runs: re-sorting on every update swaps rows under the
//! cursor faster than anyone can read them, so the sort happens once, when the
//! numbers are final.
//!
//! A row shows a number only once that row is finished. A running total is a
//! wrong number, and `4.3 GB` for a directory that turns out to hold 184 GB is
//! the same lie as `0 B`.
//!
//! Drilling in never re-walks what something already measured. A sweep sizes
//! every immediate child on its way to a total, so the level below anything it
//! measured is a lookup. Deeper levels get their own streaming walk.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use disk::audit::{self, Audit, attribution_rules};
use disk::overview::{DiskOverview, disk_overview};
use disk::report;
use disk::sweep::{
    self, Denial, Denials, Flow, Progress, Root, RootUsage, Rule, Sweep, Unreadable,
};
use disk::util::format_size;
use ratatui::{prelude::*, widgets::*};

use crate::tui::live::{self, Children};

const KEYS: &str = " [Tab] View  [Enter] Drill  [Esc] Back  [r] Rescan  [o] Finder  [q] Quit";

/// Only what a walk can answer. Nothing else is listened for, so nothing else
/// is offered.
const SCAN_KEYS: &str = " [Tab] View  [Esc] Stop  [q] Quit";

/// The naming layer is a convenience over the partition, so the remainder it
/// cannot name gets a row of its own rather than being dropped.
const UNNAMED: &str = "unnamed, largest first";

/// Browse the volume, from the first frame of the walk.
pub fn run(no_cache: bool) -> Result<()> {
    let mut app = App::new(disk_overview());

    // A walk takes about two minutes, so a usable cache is worth drawing
    // instantly and saying how old it is.
    if !no_cache {
        let now = SystemTime::now();
        if let Some(report) = report::load(now) {
            app.load_cached(&report.to_audit(), report.age(now));
        }
    }

    super::run(|terminal| event_loop(&mut app, terminal))
}

struct App {
    space: Pane,
    naming: Pane,
    section: Section,
    overview: Option<DiskOverview>,
    /// The application Full Disk Access has to be granted to. Read once: it
    /// cannot change while the screen is up.
    terminal: String,
    status: Status,
    /// How old the numbers are. `None` once a walk was stopped part way, because
    /// no age describes half a measurement.
    age: Option<Duration>,
    jobs_left: usize,
    quit: bool,
}

/// The two halves of the report. Both describe the same bytes, so their totals
/// have to agree: measured equals attributed plus unattributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Space,
    Naming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Scanning,
    /// Writing the cache back, which discovers project artifacts on the way and
    /// takes a few seconds.
    Saving,
    Idle,
}

/// One view of the measurement, with its own place in its own drill-down, so
/// switching views keeps both where the user left them.
struct Pane {
    title: &'static str,
    top: Vec<Item>,
    stack: Vec<View>,
}

enum View {
    Top {
        cursor: usize,
    },
    Directory {
        name: String,
        items: Vec<Item>,
        /// What the row above claimed this holds. The only thing that can reveal
        /// a row missing from the listing.
        total: u64,
        /// Set when the directory itself would not open, which is a different
        /// claim from it being empty.
        denied: Option<Denial>,
        cursor: usize,
    },
}

/// One line, in either view.
#[derive(Debug, Clone)]
struct Item {
    name: String,
    /// `None` for a row that is not one path: a category, or the row standing in
    /// for a directory's own files.
    path: Option<PathBuf>,
    /// What the path column says when there is no path to put there. Four
    /// volumes have no single path and still have to say what they are.
    detail: Option<String>,
    size: Size,
    denials: Denials,
    /// The top-level root that counts this row's bytes, when another one does.
    /// The size column says the bytes are elsewhere; this says where.
    counted_as: Option<String>,
    down: Down,
}

/// What lies below a row, when anything does.
#[derive(Debug, Clone)]
enum Down {
    /// Nothing measured it. A row with a path is walked on demand; one without
    /// has nowhere to go.
    Unmeasured,
    /// What a walk already established about the level below, so drilling in is
    /// a lookup rather than a second walk.
    Children(Level),
    /// A listing that is already built, as the naming layer's rows are.
    Rows(Vec<Item>),
}

/// One level down as a measurement rather than as rows. The three lists are
/// different claims and a listing that keeps only the first makes the other two
/// unstateable: a refused child would read as zero bytes, and a child counted
/// under a root of its own would read as unmeasured.
#[derive(Debug, Clone, Default)]
struct Level {
    /// Immediate children with their subtree sizes.
    measured: Vec<(PathBuf, u64)>,
    /// Children whose own directory would not open. Each of these is also in
    /// `measured`, carrying the zero bytes the walk was able to count, so this
    /// list has to be consulted first or the refusal reads as a measurement.
    refused: Vec<Unreadable>,
    /// Children the partition counts under a root of their own, so this level
    /// names them without adding their bytes to the total above it.
    elsewhere: Vec<Elsewhere>,
}

/// A child measured somewhere else. The roots partition the volume and exclude
/// each other, so `~/Library` is missing from `Home`'s children by design, not
/// by failure.
#[derive(Debug, Clone)]
struct Elsewhere {
    path: PathBuf,
    /// What the top-level screen calls it, which is where its bytes are.
    root: String,
    total: u64,
    below: Level,
}

/// What a row can say about its own size. A directory that refused to open is
/// not empty, and a directory still being counted has no size yet: drawing
/// `0 B` for either is a claim no walk ever made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    /// Waiting on a measurement. The bytes are a running total, kept because the
    /// last one reported is the answer, and never shown as one.
    Counting(u64),
    Measured(u64),
    /// Measured, but under a root of its own. The bytes are kept for ordering
    /// and for the level below, and never printed here: the row above excludes
    /// them, so printing them would make this listing overrun its own total.
    Elsewhere(u64),
    Unknown,
}

impl Size {
    fn bytes(self) -> u64 {
        match self {
            Size::Counting(bytes) | Size::Measured(bytes) | Size::Elsewhere(bytes) => bytes,
            Size::Unknown => 0,
        }
    }

    /// The bytes as an answer. `None` while they are still a running total, so
    /// nothing sums a number that is going to change, and `None` for bytes
    /// another row already accounts for.
    fn settled(self) -> Option<u64> {
        match self {
            Size::Measured(bytes) => Some(bytes),
            Size::Counting(_) | Size::Elsewhere(_) | Size::Unknown => None,
        }
    }

    fn label(self) -> String {
        match self {
            Size::Counting(_) => "scanning".to_string(),
            Size::Measured(bytes) => format_size(bytes),
            Size::Elsewhere(_) => "elsewhere".to_string(),
            Size::Unknown => "unknown".to_string(),
        }
    }

    /// The walk is finished with this row.
    ///
    /// Nothing measured plus a refusal is unknown, never zero. Nothing measured
    /// and no refusal is a measurement of an empty directory, which is what the
    /// caller has to be holding: this settles a size the walk reported, not a
    /// running total nobody has confirmed.
    fn settle(self, denials: Denials) -> Size {
        // Not this walk's row to settle: another root measured it.
        if let Size::Elsewhere(_) = self {
            return self;
        }
        let bytes = self.bytes();
        if bytes > 0 {
            return Size::Measured(bytes);
        }
        if denials.total() > 0 {
            return Size::Unknown;
        }
        self
    }
}

impl Item {
    fn drillable(&self) -> bool {
        self.path.is_some() || matches!(self.down, Down::Rows(_))
    }
}

/// What `Enter` should do with the selected row.
enum Drill {
    /// A listing that needs no measuring at all.
    Rows {
        name: String,
        items: Vec<Item>,
        total: u64,
    },
    /// Answerable from what a walk already measured.
    Cached {
        name: String,
        path: PathBuf,
        total: u64,
        below: Level,
    },
    Walk {
        name: String,
        path: PathBuf,
        total: u64,
    },
    Nothing,
}

/// What the keys pressed during a walk add up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Pressed {
    stop: Option<Stop>,
    /// Switching views changes what is drawn and never touches the walk.
    toggle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Abandon this walk and go back to the view it started from.
    Back,
    Quit,
}

impl Pane {
    fn new(title: &'static str) -> Self {
        Pane {
            title,
            top: Vec::new(),
            stack: vec![View::Top { cursor: 0 }],
        }
    }

    fn reset(&mut self, top: Vec<Item>) {
        self.top = top;
        self.stack = vec![View::Top { cursor: 0 }];
    }

    fn current(&self) -> &View {
        self.stack.last().expect("stack never empty")
    }

    fn current_mut(&mut self) -> &mut View {
        self.stack.last_mut().expect("stack never empty")
    }

    fn at_top(&self) -> bool {
        matches!(self.current(), View::Top { .. })
    }

    fn items(&self) -> &[Item] {
        match self.current() {
            View::Top { .. } => &self.top,
            View::Directory { items, .. } => items,
        }
    }

    fn cursor(&self) -> usize {
        match self.current() {
            View::Top { cursor } | View::Directory { cursor, .. } => *cursor,
        }
    }

    fn selected(&self) -> Option<&Item> {
        self.items().get(self.cursor())
    }

    fn breadcrumb(&self) -> String {
        let mut parts = vec![self.title.to_string()];
        for view in &self.stack {
            if let View::Directory { name, .. } = view {
                parts.push(name.clone());
            }
        }
        parts.join(" > ")
    }

    fn move_up(&mut self) {
        match self.current_mut() {
            View::Top { cursor } | View::Directory { cursor, .. } => {
                *cursor = cursor.saturating_sub(1);
            }
        }
    }

    fn move_down(&mut self) {
        let max = self.items().len().saturating_sub(1);
        match self.current_mut() {
            View::Top { cursor } | View::Directory { cursor, .. } => {
                if *cursor < max {
                    *cursor += 1;
                }
            }
        }
    }

    fn pop_or_quit(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            false
        } else {
            true
        }
    }

    fn drill(&self) -> Drill {
        let Some(item) = self.selected() else {
            return Drill::Nothing;
        };
        let name = item.name.clone();
        let total = item.size.bytes();
        if let Down::Rows(rows) = &item.down {
            return Drill::Rows {
                name,
                items: rows.clone(),
                total,
            };
        }
        let Some(path) = item.path.clone() else {
            return Drill::Nothing;
        };
        match &item.down {
            Down::Children(below) => Drill::Cached {
                name,
                path,
                total,
                below: below.clone(),
            },
            _ => Drill::Walk { name, path, total },
        }
    }

    /// Handles `Enter`. `Some` is a directory that still has to be walked;
    /// anything already measured is on screen by the time this returns.
    fn enter(&mut self) -> Option<(String, PathBuf, u64)> {
        match self.drill() {
            Drill::Rows { name, items, total } => {
                self.stack.push(View::Directory {
                    name,
                    items,
                    total,
                    denied: None,
                    cursor: 0,
                });
                None
            }
            Drill::Cached {
                name,
                path,
                total,
                below,
            } => {
                let listing = live::children_as_roots(&path);
                self.stack.push(View::Directory {
                    name,
                    items: measured_items(&listing, &below),
                    total,
                    denied: listing.denied,
                    cursor: 0,
                });
                None
            }
            Drill::Walk { name, path, total } => Some((name, path, total)),
            Drill::Nothing => None,
        }
    }

    fn open_directory(&mut self, name: String, listing: &Children, total: u64) {
        self.stack.push(View::Directory {
            name,
            items: counting_items(listing),
            total,
            denied: listing.denied,
            cursor: 0,
        });
    }
}

impl App {
    fn new(overview: Option<DiskOverview>) -> Self {
        Self {
            space: Pane::new("Where the space is"),
            naming: Pane::new("What it is"),
            section: Section::Space,
            overview,
            terminal: sweep::full_disk_access_app(),
            status: Status::Idle,
            age: None,
            jobs_left: 0,
            quit: false,
        }
    }

    fn of(&self, section: Section) -> &Pane {
        match section {
            Section::Space => &self.space,
            Section::Naming => &self.naming,
        }
    }

    fn of_mut(&mut self, section: Section) -> &mut Pane {
        match section {
            Section::Space => &mut self.space,
            Section::Naming => &mut self.naming,
        }
    }

    fn pane(&self) -> &Pane {
        self.of(self.section)
    }

    fn pane_mut(&mut self) -> &mut Pane {
        self.of_mut(self.section)
    }

    /// Both views describe the same measurement, so switching is only ever a
    /// change of what is drawn.
    fn toggle(&mut self) {
        self.section = match self.section {
            Section::Space => Section::Naming,
            Section::Naming => Section::Space,
        };
    }

    fn scanning(&self) -> bool {
        self.status == Status::Scanning
    }

    /// Everything the rows account for, or `None` while a walk is running. A
    /// total missing most of the volume is not a total, and the row rule applies
    /// just as much to the sum of the rows.
    ///
    /// This is the sum of the top rows and nothing besides, so the header cannot
    /// state bytes no row shows. The volumes the walk cannot open are one of
    /// those rows rather than an addition made here.
    fn accounted(&self) -> Option<u64> {
        if self.scanning() {
            return None;
        }
        Some(self.space.top.iter().filter_map(|r| r.size.settled()).sum())
    }

    /// What the rows on screen add up to.
    fn listed(&self) -> u64 {
        self.pane()
            .items()
            .iter()
            .filter_map(|i| i.size.settled())
            .sum()
    }

    /// Refusals explain bytes neither view can show, so both views report them.
    /// A listing reports its own.
    fn denials(&self) -> Denials {
        match self.pane().current() {
            View::Directory { items, denied, .. } => denials_in(items, *denied),
            View::Top { .. } => denials_in(&self.space.top, None),
        }
    }

    fn load_cached(&mut self, audit: &Audit, age: Duration) {
        let overview = self.overview.as_ref();
        let (space, naming) = (
            root_items(&audit.roots, overview),
            group_items(audit, overview),
        );
        self.space.reset(space);
        self.naming.reset(naming);
        self.age = Some(age);
        self.status = Status::Idle;
    }

    fn begin_scan(&mut self, rules: &[Rule]) {
        // The rule paths are known before anything is measured, so the naming
        // view has its rows from the first frame too.
        self.space.reset(Vec::new());
        self.naming.reset(pending_groups(rules));
        self.status = Status::Scanning;
    }

    fn apply_top(&mut self, progress: &Progress) {
        match progress {
            Progress::Started { roots, jobs } => {
                self.space.top = roots.iter().map(pending_item).collect();
                self.jobs_left = *jobs;
            }
            Progress::Scanned { remaining, .. } => self.jobs_left = *remaining,
            _ => {}
        }
        update(&mut self.space.top, progress);
    }

    fn apply_drill(&mut self, owner: Section, progress: &Progress) {
        match progress {
            Progress::Started { jobs, .. } => self.jobs_left = *jobs,
            Progress::Scanned { remaining, .. } => self.jobs_left = *remaining,
            _ => {}
        }
        if let View::Directory { items, .. } = self.of_mut(owner).current_mut() {
            update(items, progress);
        }
    }

    fn finish(&mut self, audit: &Audit) {
        let overview = self.overview.as_ref();
        let (space, naming) = (
            root_items(&audit.roots, overview),
            group_items(audit, overview),
        );
        self.space.reset(space);
        self.naming.reset(naming);
        self.stop();
    }

    /// Every immediate child was its own sweep root, so the final numbers land
    /// on the rows the same way the progress did.
    fn finish_directory(&mut self, owner: Section, swept: Sweep) {
        self.stop();
        if let View::Directory { items, .. } = self.of_mut(owner).current_mut() {
            for (item, usage) in items.iter_mut().zip(&swept.roots) {
                item.size = Size::Measured(usage.total).settle(usage.denials);
                item.denials = usage.denials;
                // Siblings never nest, so none of these is counted under
                // another and `level_of` finds nothing to put elsewhere.
                item.down = Down::Children(level_of(usage, &swept.roots));
            }
            items.sort_by_key(|i| std::cmp::Reverse(i.size.bytes()));
        }
    }

    /// Stopping a drill-down abandons the listing it was building. The view it
    /// came from is still worth looking at, so this must not read as quitting.
    fn cancel_drill(&mut self, owner: Section) {
        self.stop();
        self.of_mut(owner).pop_or_quit();
    }

    fn stop(&mut self) {
        self.status = Status::Idle;
        self.jobs_left = 0;
    }
}

fn pending_item(root: &Root) -> Item {
    Item {
        name: root.name.clone(),
        path: Some(root.path.clone()),
        detail: None,
        size: Size::Counting(0),
        denials: Denials::default(),
        counted_as: None,
        down: Down::Unmeasured,
    }
}

/// One row per root, dropping the ones that hold nothing and refused nothing: an
/// empty row is neither somewhere to explore nor anything to reclaim. The
/// volumes no walk can open get a row too, because the header counts them.
fn root_items(roots: &[RootUsage], overview: Option<&DiskOverview>) -> Vec<Item> {
    let mut items: Vec<Item> = roots
        .iter()
        .filter(|r| r.total > 0 || r.denials.total() > 0)
        .map(|r| Item {
            name: r.name.clone(),
            path: Some(r.path.clone()),
            detail: None,
            size: Size::Measured(r.total).settle(r.denials),
            denials: r.denials,
            counted_as: None,
            down: Down::Children(level_of(r, roots)),
        })
        .collect();

    // Where its own size puts it, rather than pinned to an end. The audit
    // decides the order of the rows it produced, so this reads that order
    // instead of imposing one.
    if let Some(row) = other_volumes_item(overview) {
        let at = items
            .iter()
            .position(|i| i.size.bytes() < row.size.bytes())
            .unwrap_or(items.len());
        items.insert(at, row);
    }
    items
}

/// The rest of the APFS container: System, Preboot, Recovery, VM. The container
/// figures count these bytes and no walk can enumerate them, so they are part of
/// what the header accounts for while no root row can ever show them. A total
/// with no row behind it is exactly the sort of number this screen exists to
/// stop stating.
///
/// It is a statement and nothing else. There is no single path, so the column
/// that holds one says what the volumes are instead, and `Enter` has nowhere to
/// go: nothing here can look inside them.
fn other_volumes_item(overview: Option<&DiskOverview>) -> Option<Item> {
    let bytes = overview.map_or(0, |o| o.other_volumes);
    (bytes > 0).then(|| Item {
        name: "Other volumes".to_string(),
        path: None,
        detail: Some("System, Preboot, Recovery, VM".to_string()),
        size: Size::Measured(bytes),
        denials: Denials::default(),
        counted_as: None,
        down: Down::Unmeasured,
    })
}

/// What a sweep knows about one root's immediate children, the ones it
/// deliberately did not measure included: a root nested inside another is
/// skipped by the outer walk, and a listing that cannot say so has to call the
/// biggest directory in a home folder unknown.
///
/// The recursion terminates because a parent path is strictly shorter than its
/// child, so the roots it descends through cannot repeat.
fn level_of(usage: &RootUsage, roots: &[RootUsage]) -> Level {
    Level {
        measured: usage.children.clone(),
        refused: usage.refused_children.clone(),
        elsewhere: roots
            .iter()
            .filter(|other| other.path.parent() == Some(usage.path.as_path()))
            .map(|other| Elsewhere {
                path: other.path.clone(),
                root: other.name.clone(),
                total: other.total,
                below: level_of(other, roots),
            })
            .collect(),
    }
}

/// The naming layer over a finished measurement, and the remainder it could not
/// name. Together these two are the partition again, which is the only reason
/// the section is worth printing.
fn group_items(audit: &Audit, overview: Option<&DiskOverview>) -> Vec<Item> {
    let mut items: Vec<Item> = audit
        .categories
        .iter()
        .map(|category| Item {
            name: category.name.clone(),
            path: None,
            detail: None,
            size: Size::Measured(category.total_size),
            denials: Denials::default(),
            counted_as: None,
            down: Down::Rows(
                category
                    .paths
                    .iter()
                    .map(|path| Item {
                        name: path.label.clone(),
                        path: Some(path.path.clone()),
                        detail: None,
                        size: Size::Measured(path.size),
                        denials: Denials::default(),
                        counted_as: None,
                        down: Down::Unmeasured,
                    })
                    .collect(),
            ),
        })
        .collect();

    let unattributed = audit.unattributed();
    if unattributed > 0 {
        items.push(Item {
            name: UNNAMED.to_string(),
            path: None,
            detail: None,
            size: Size::Measured(unattributed),
            denials: Denials::default(),
            counted_as: None,
            down: Down::Rows(
                audit
                    .largest_unattributed(8)
                    .into_iter()
                    .map(|(path, size)| Item {
                        name: path.display().to_string(),
                        path: Some(path.clone()),
                        detail: None,
                        size: Size::Measured(size),
                        denials: Denials::default(),
                        counted_as: None,
                        down: Down::Unmeasured,
                    })
                    .collect(),
            ),
        });
    }

    // Last, one step further out than the remainder above it: that row is bytes
    // the rules did not name, these are bytes nothing on this machine can look
    // inside, so no rule could ever name them. Both belong after the categories,
    // and the unnameable one belongs after the unnamed one.
    items.extend(other_volumes_item(overview));
    items
}

/// The naming layer before anything has been measured. Only a finished sweep can
/// size a category, but the names are in the rules, so the rows can be there
/// from the start instead of the view being blank for two minutes.
fn pending_groups(rules: &[Rule]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for rule in rules {
        let row = Item {
            name: rule.label.clone(),
            path: Some(rule.path.clone()),
            detail: None,
            size: Size::Counting(0),
            denials: Denials::default(),
            counted_as: None,
            down: Down::Unmeasured,
        };
        match items.iter_mut().find(|i| i.name == rule.category) {
            Some(category) => {
                if let Down::Rows(rows) = &mut category.down {
                    rows.push(row);
                }
            }
            None => items.push(Item {
                name: rule.category.clone(),
                path: None,
                detail: None,
                size: Size::Counting(0),
                denials: Denials::default(),
                counted_as: None,
                down: Down::Rows(vec![row]),
            }),
        }
    }
    items
}

/// The listing of a directory nothing has measured yet: subdirectories in name
/// order, since no size is known to order them by.
fn counting_items(listing: &Children) -> Vec<Item> {
    let mut items: Vec<Item> = listing
        .roots
        .iter()
        .map(|root| Item {
            name: root.name.clone(),
            path: Some(root.path.clone()),
            detail: None,
            size: Size::Counting(0),
            denials: Denials::default(),
            counted_as: None,
            down: Down::Unmeasured,
        })
        .collect();
    items.extend(loose_item(listing));
    items
}

/// The listing of a directory a walk already sized. The subdirectory sizes are a
/// lookup, and the loose files were read while listing.
fn measured_items(listing: &Children, below: &Level) -> Vec<Item> {
    let mut items: Vec<Item> = listing
        .roots
        .iter()
        .map(|root| child_item(&root.name, &root.path, below))
        .collect();

    // A path the walk measured that the listing never saw. Dropping it would
    // hide bytes the row above has already counted.
    for (path, bytes) in &below.measured {
        if !items
            .iter()
            .any(|i| i.path.as_deref() == Some(path.as_path()))
        {
            items.push(Item {
                name: file_name(path),
                path: Some(path.clone()),
                detail: None,
                size: Size::Measured(*bytes),
                denials: Denials::default(),
                counted_as: None,
                down: Down::Unmeasured,
            });
        }
    }

    items.extend(loose_item(listing));
    items.sort_by_key(|i| std::cmp::Reverse(i.size.bytes()));
    items
}

/// One row of a listing whose parent a walk already measured.
///
/// The order of these three questions is the point of the whole function. A
/// child whose own directory refused to open is still reported with the bytes
/// the walk managed to count, which is none of them, so asking for a size first
/// prints `0 B` over a directory nobody has ever been allowed to look inside.
fn child_item(name: &str, path: &Path, below: &Level) -> Item {
    let row = |size, denials, counted_as, down| Item {
        name: name.to_string(),
        path: Some(path.to_path_buf()),
        detail: None,
        size,
        denials,
        counted_as,
        down,
    };

    if let Some(refusal) = below.refused.iter().find(|entry| entry.path == path) {
        return row(
            Size::Unknown,
            one_refusal(refusal.denial),
            None,
            Down::Unmeasured,
        );
    }
    if let Some(other) = below.elsewhere.iter().find(|entry| entry.path == path) {
        return row(
            Size::Elsewhere(other.total),
            Denials::default(),
            Some(other.root.clone()),
            Down::Children(other.below.clone()),
        );
    }
    // Absent from all three lists: this sweep never saw it, so it has appeared
    // since. Unmeasured is exactly what unknown means.
    let size = match below.measured.iter().find(|(child, _)| child == path) {
        Some((_, bytes)) => Size::Measured(*bytes),
        None => Size::Unknown,
    };
    row(size, Denials::default(), None, Down::Unmeasured)
}

/// One refusal as a count, so a row carries its own reason into the advice
/// underneath it. Each kind has a different fix.
fn one_refusal(denial: Denial) -> Denials {
    Denials::one(denial)
}

/// The directory's own files, as one row. A walk reports one child per
/// subdirectory and counts loose files into the total without naming them, so
/// without this row a listing quietly adds up to less than the row above it.
fn loose_item(listing: &Children) -> Option<Item> {
    if listing.loose_files == 0 {
        return None;
    }
    let name = if listing.loose_files == 1 {
        "1 loose file".to_string()
    } else {
        format!("{} loose files", listing.loose_files)
    };
    Some(Item {
        name,
        path: None,
        detail: None,
        size: Size::Measured(listing.loose_bytes),
        denials: Denials::default(),
        counted_as: None,
        down: Down::Unmeasured,
    })
}

fn update(items: &mut [Item], progress: &Progress) {
    match progress {
        Progress::Scanned {
            root, root_bytes, ..
        } => {
            if let Some(item) = items.get_mut(*root) {
                item.size = Size::Counting(*root_bytes);
            }
        }
        Progress::RootDone {
            root,
            denials,
            bytes,
        } => {
            if let Some(item) = items.get_mut(*root) {
                // The row is finished, which is the moment a number is allowed
                // to be shown. The event carries it, so a root made of nothing
                // but loose files settles here instead of reading as still
                // scanning until the whole sweep ends.
                item.size = Size::Measured(*bytes).settle(*denials);
                item.denials = *denials;
            }
        }
        _ => {}
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Exact counts, taken from the counters rather than from the capped sample of
/// paths, whose length would report a split of the twenty that happened to be
/// kept.
fn denials_in(items: &[Item], denied: Option<Denial>) -> Denials {
    let mut total = items.iter().fold(Denials::default(), |mut total, item| {
        total.merge(item.denials);
        total
    });
    if let Some(denial) = denied {
        total.record(denial);
    }
    total
}

/// Two reasons a directory refuses to open, two fixes that are not
/// interchangeable: Full Disk Access lifts a privacy refusal and does nothing
/// whatsoever for a unix one.
///
/// `terminal` is the application the grant attaches to, which is never `wsctl`,
/// so naming it is the difference between advice that works and advice that
/// cannot.
fn denial_notes(denials: Denials, terminal: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if denials.protected > 0 {
        notes.push(format!(
            "  {} unreadable: give {terminal} Full Disk Access in System Settings.",
            directories(denials.protected)
        ));
    }
    if denials.forbidden > 0 {
        notes.push(format!(
            "  {} unreadable: these need sudo, not Full Disk Access.",
            directories(denials.forbidden)
        ));
    }
    if denials.unattended > 0 {
        notes.push(format!(
            "  {} left closed: the scheduled refresh does not open what macOS asks \
             about first. Press r to measure them now.",
            directories(denials.unattended)
        ));
    }
    notes
}

/// A listing that adds up to less than the row above it is holding bytes back.
/// Saying nothing would make them look as though they do not exist.
fn unlisted_note(app: &App) -> Option<String> {
    if app.scanning() {
        return None;
    }
    let View::Directory { total, .. } = app.pane().current() else {
        return None;
    };
    let missing = total.saturating_sub(app.listed());
    (missing > 0).then(|| format!("  {} here is not listed above", format_size(missing)))
}

/// A row with no size of its own reads as a row the tool failed on. Naming the
/// root that holds its bytes says where they went, and why this listing is right
/// not to add them to a total that excludes them.
fn elsewhere_notes(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| {
            let root = item.counted_as.as_ref()?;
            Some(format!(
                "  {} is counted under {root}, not in this total",
                item.name
            ))
        })
        .collect()
}

fn notes(app: &App) -> Vec<String> {
    let mut notes = denial_notes(app.denials(), &app.terminal);
    notes.extend(elsewhere_notes(app.pane().items()));
    notes.extend(unlisted_note(app));
    notes
}

fn directories(count: usize) -> String {
    if count == 1 {
        "1 directory".to_string()
    } else {
        format!("{count} directories")
    }
}

/// The container, as the plain report leads with it: what the box holds, and how
/// much of it this walk could actually reach.
///
/// A walk still running contributes nothing here. Its running total would read
/// as a measurement, and the shortfall computed from it would name most of the
/// volume as unreachable when it is only unfinished.
///
/// `accounted` arrives already summed from the rows. Adding anything to it here
/// is how the header came to claim 30.7 GB that no row on screen showed.
fn figures(overview: Option<&DiskOverview>, accounted: Option<u64>) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    if let Some(overview) = overview {
        out.push((format_size(overview.used()), "used"));
        out.push((format_size(overview.total), "total"));
        out.push((format_size(overview.free), "free"));
    }

    let Some(accounted) = accounted else {
        return out;
    };
    out.push((format_size(accounted), "accounted for"));

    if let Some(overview) = overview {
        let shortfall = overview.used().saturating_sub(accounted);
        // Under a percent is rounding between two ways of counting, not a gap
        // worth a line of its own.
        if shortfall > overview.used() / 100 {
            out.push((format_size(shortfall), "not reachable by this scan"));
        }
    }
    out
}

/// Drops whole figures off the end rather than letting the last one be sliced
/// mid-word. Four figures a narrow terminal can read beat five it cannot.
fn fit(mut figures: Vec<(String, &'static str)>, width: u16) -> Vec<(String, &'static str)> {
    while figures.len() > 1 && figures_width(&figures) > width as usize {
        figures.pop();
    }
    figures
}

/// Two spaces of margin, then two between each pair, so every figure costs two
/// spaces whatever its position.
fn figures_width(figures: &[(String, &'static str)]) -> usize {
    figures
        .iter()
        .map(|(size, label)| 2 + size.chars().count() + 1 + label.chars().count())
        .sum()
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    if app.space.top.is_empty() {
        sweep_live(app, terminal)?;
    }

    while !app.quit {
        terminal.draw(|f| render(f, app))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Tab | KeyCode::BackTab => app.toggle(),
            KeyCode::Up | KeyCode::Char('k') => app.pane_mut().move_up(),
            KeyCode::Down | KeyCode::Char('j') => app.pane_mut().move_down(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => drill_in(app, terminal)?,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') if app.pane_mut().pop_or_quit() => {
                return Ok(());
            }
            KeyCode::Char('r') => sweep_live(app, terminal)?,
            KeyCode::Char('o') => open_in_finder(app),
            _ => {}
        }
    }
    Ok(())
}

/// The sweep calls back on this thread, so drawing and key handling happen
/// inside the walk rather than alongside it. A tick arrives even when nothing
/// finishes, which is what keeps the screen answering keys.
fn sweep_live(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let roots = sweep::discover_roots();
    let rules = attribution_rules();

    app.begin_scan(&rules);
    let mut throttle = live::Throttle::new(live::REDRAW);
    let mut failure: Option<anyhow::Error> = None;

    let swept = sweep::sweep_streaming(&roots, &rules, &mut |progress| {
        app.apply_top(&progress);
        flow(app, terminal, &mut throttle, &mut failure, &mut pressed)
    });

    if let Some(e) = failure {
        return Err(e);
    }
    let Some(swept) = swept else {
        // Half a measurement, so nothing on screen has an age.
        app.stop();
        app.age = None;
        return Ok(());
    };

    let audit = audit::from_sweep(swept, &rules);
    app.finish(&audit);

    // An interactive walk leaves the fast path better off than it found it,
    // which is the contract report::load_or_refresh keeps for every other
    // caller.
    app.status = Status::Saving;
    throttle.force();
    terminal.draw(|f| render(f, app))?;
    save(audit);

    app.status = Status::Idle;
    app.age = Some(Duration::ZERO);
    Ok(())
}

/// One redraw and one look at the keyboard, from inside a walk. Shared so a
/// drill-down costs the same as the sweep it drills into.
fn flow<B>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    throttle: &mut live::Throttle,
    failure: &mut Option<anyhow::Error>,
    interrupt: &mut dyn FnMut() -> io::Result<Pressed>,
) -> Flow
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let keys = match interrupt() {
        Ok(keys) => keys,
        Err(e) => {
            *failure = Some(e.into());
            return Flow::Cancel;
        }
    };
    if keys.toggle {
        app.toggle();
        throttle.force();
    }

    if throttle.ready()
        && let Err(e) = terminal.draw(|f| render(f, app))
    {
        *failure = Some(e.into());
        return Flow::Cancel;
    }

    match keys.stop {
        None => Flow::Continue,
        Some(Stop::Back) => Flow::Cancel,
        Some(Stop::Quit) => {
            app.quit = true;
            Flow::Cancel
        }
    }
}

fn save(audit: Audit) {
    let report = report::from_audit(
        audit,
        &disk::projects::default_roots(),
        disk::projects::MAX_DEPTH,
        SystemTime::now(),
    );
    if let Err(e) = report::save(&report) {
        tracing::warn!("could not write disk report cache: {e}");
    }
}

/// Drains the queue rather than returning on the first hit, so a key held down
/// through a walk does not also act on the view the walk returns to.
fn pressed() -> io::Result<Pressed> {
    let mut pressed = Pressed::default();
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => pressed.stop = Some(Stop::Quit),
            KeyCode::Esc if pressed.stop != Some(Stop::Quit) => pressed.stop = Some(Stop::Back),
            KeyCode::Tab | KeyCode::BackTab => pressed.toggle = !pressed.toggle,
            _ => {}
        }
    }
    Ok(pressed)
}

fn drill_in(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    match app.pane_mut().enter() {
        Some((name, path, total)) => {
            sweep_directory(app, terminal, name, path, total, &mut pressed)
        }
        None => Ok(()),
    }
}

/// One streaming walk of the directory being entered, each subdirectory its own
/// root so it finishes and reports on its own.
fn sweep_directory<B>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    name: String,
    path: PathBuf,
    total: u64,
    interrupt: &mut dyn FnMut() -> io::Result<Pressed>,
) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // The loose files are read here and never reported by a root, so the listing
    // starts with them already counted.
    let listing = live::children_as_roots(&path);
    let owner = app.section;
    app.of_mut(owner).open_directory(name, &listing, total);
    app.status = Status::Scanning;
    app.jobs_left = 0;

    let mut throttle = live::Throttle::new(live::REDRAW);
    let mut failure: Option<anyhow::Error> = None;

    let swept = sweep::sweep_streaming(&listing.roots, &[], &mut |progress| {
        app.apply_drill(owner, &progress);
        flow(app, terminal, &mut throttle, &mut failure, interrupt)
    });

    if let Some(e) = failure {
        return Err(e);
    }
    match swept {
        Some(swept) => app.finish_directory(owner, swept),
        None => app.cancel_drill(owner),
    }
    Ok(())
}

/// Reveal rather than open: a plain `open` on a file launches whatever app
/// claims it, which is not what a disk browser should do to a stray 4 GB blob.
fn open_in_finder(app: &App) {
    if let Some(path) = app.pane().selected().and_then(|i| i.path.as_ref()) {
        let _ = Command::new("open").arg("-R").arg(path).status();
    }
}

fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    f.render_widget(header(app, chunks[0].width), chunks[0]);
    render_body(f, chunks[1], app);
    f.render_widget(footer(app), chunks[2]);
}

fn header(app: &App, width: u16) -> Paragraph<'static> {
    let state = match app.status {
        Status::Scanning => Span::styled("scanning", Style::default().fg(Color::Yellow)),
        Status::Saving => Span::styled("saving", Style::default().fg(Color::Yellow)),
        Status::Idle => Span::styled(
            match app.age {
                Some(age) => report::humanize_age(age),
                None => "partial".to_string(),
            },
            Style::default().fg(Color::DarkGray),
        ),
    };

    let mut totals = vec![Span::raw("  ")];
    for (size, label) in fit(figures(app.overview.as_ref(), app.accounted()), width) {
        if totals.len() > 1 {
            totals.push(Span::raw("  "));
        }
        totals.push(Span::styled(size, Style::default().fg(Color::Yellow)));
        totals.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "  wsctl disk audit",
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw("  "),
            Span::styled(app.pane().breadcrumb(), Style::default().fg(Color::White)),
            Span::raw("  "),
            state,
        ]),
        Line::from(totals),
    ])
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let notes = notes(app);
    let height = (notes.len() as u16).min(area.height.saturating_sub(1));
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).split(area);

    render_rows(f, chunks[0], app);

    let lines: Vec<Line> = notes
        .into_iter()
        .map(|note| Line::from(Span::styled(note, Style::default().fg(Color::DarkGray))))
        .collect();
    f.render_widget(Paragraph::new(lines).block(body_block()), chunks[1]);
}

fn render_rows(f: &mut Frame, area: Rect, app: &App) {
    let pane = app.pane();
    let items = pane.items();
    if items.is_empty() {
        let (text, color) = empty_state(app);
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(color))
                .block(body_block()),
            area,
        );
        return;
    }

    // Every row in a listing shares a parent, so only the partition shows paths.
    let with_path = pane.at_top() && app.section == Section::Space;
    let cursor = pane.cursor();
    let rows: Vec<Row> = items
        .iter()
        .enumerate()
        .map(|(i, item)| item_row(item, i == cursor, with_path))
        .collect();

    let widths: Vec<Constraint> = if with_path {
        vec![
            Constraint::Length(3),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Min(1),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(10),
        ]
    };

    // TableState only scrolls the viewport. The visible cursor is the row
    // background, so no highlight style is set.
    let mut state = TableState::default().with_selected(Some(cursor));
    f.render_stateful_widget(
        Table::new(rows, widths).block(body_block()),
        area,
        &mut state,
    );
}

fn item_row(item: &Item, cursor: bool, with_path: bool) -> Row<'_> {
    let size_color = match item.size {
        Size::Unknown => Color::Red,
        // Not a size, so not the colour sizes get: the bytes are on another row.
        Size::Elsewhere(_) => Color::DarkGray,
        _ => Color::Yellow,
    };
    let name_color = if item.path.is_some() {
        Color::Cyan
    } else {
        Color::White
    };

    let mut cells = vec![
        Cell::from(if item.drillable() { "📁" } else { "  " }),
        Cell::from(item.name.as_str()).style(Style::default().fg(readable(name_color, cursor))),
        Cell::from(Text::from(item.size.label()).right_aligned())
            .style(Style::default().fg(readable(size_color, cursor))),
    ];
    if with_path {
        // A row that is not one path says what it is instead. Leaving the column
        // blank there reads as a row the tool gave up on.
        let about = item
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .or_else(|| item.detail.clone())
            .unwrap_or_default();
        cells.push(Cell::from(about).style(Style::default().fg(readable(Color::DarkGray, cursor))));
    }

    Row::new(cells).style(if cursor {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    })
}

/// The cursor is a DarkGray background, so a DarkGray cell on that row is text
/// drawn in the colour behind it: the path of the selected row was being
/// rendered and could not be read. Secondary text lifts to White under the
/// cursor and is left alone everywhere else, which keeps the cursor a background
/// and nothing else.
fn readable(color: Color, cursor: bool) -> Color {
    match (cursor, color) {
        (true, Color::DarkGray) => Color::White,
        _ => color,
    }
}

/// A directory that refused to open is not an empty one, so it does not get to
/// borrow the empty one's wording.
fn empty_state(app: &App) -> (&'static str, Color) {
    if app.scanning() {
        return ("  Scanning...", Color::Yellow);
    }
    if app.denials().total() > 0 {
        return ("  unknown", Color::Red);
    }
    ("  (empty)", Color::DarkGray)
}

fn footer(app: &App) -> Line<'static> {
    match app.status {
        Status::Scanning => Line::from(vec![
            Span::styled(
                format!(" Scanning: {} left ", app.jobs_left),
                Style::default().fg(Color::Yellow).bold(),
            ),
            Span::styled(SCAN_KEYS, Style::default().fg(Color::DarkGray)),
        ]),
        // Nothing is listened for while the report is written, so no key is
        // offered either.
        Status::Saving => Line::from(Span::styled(
            " Saving the report, measuring build artifacts ",
            Style::default().fg(Color::Yellow).bold(),
        )),
        Status::Idle => Line::from(Span::styled(KEYS, Style::default().fg(Color::DarkGray))),
    }
}

fn body_block() -> Block<'static> {
    Block::default()
        .borders(Borders::NONE)
        .padding(Padding::horizontal(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use disk::audit::{Category, CategoryPath};
    use ratatui::backend::TestBackend;
    use std::fs;

    /// The binary crate has no dev-dependencies, so scratch trees are built by
    /// hand rather than with `tempfile`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("wsctl-audit-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        }

        fn file(&self, name: &str, size: usize) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, vec![0u8; size]).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Decimal, matching `format_size`, so a fixture written as `40 * GB` is
    /// the same 40 GB the screen draws and `drawn_sizes` reads back.
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;

    /// Fixed here on purpose: what matters is that the advice names the
    /// application the grant attaches to, not which one this machine reports.
    const TERMINAL: &str = "Ghostty";

    fn blank(overview: Option<DiskOverview>) -> App {
        let mut app = App::new(overview);
        app.terminal = TERMINAL.to_string();
        app
    }

    fn protected(count: usize) -> Denials {
        Denials {
            protected: count,
            ..Denials::default()
        }
    }

    fn forbidden(count: usize) -> Denials {
        Denials {
            forbidden: count,
            ..Denials::default()
        }
    }

    fn root(name: &str) -> Root {
        Root {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
        }
    }

    fn usage(name: &str, total: u64, denials: Denials) -> RootUsage {
        RootUsage {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            total,
            children: Vec::new(),
            refused_children: Vec::new(),
            unattributed_total: 0,
            unattributed: Vec::new(),
            // Deliberately shorter than the counts above: the real sample is
            // capped at twenty per root, so its length is not a count.
            unreadable: vec![Unreadable {
                path: PathBuf::from("/tmp/locked"),
                denial: Denial::Protected,
            }],
            denials,
        }
    }

    fn audit_of(roots: Vec<RootUsage>) -> Audit {
        Audit {
            roots,
            categories: Vec::new(),
        }
    }

    fn started(names: &[&str]) -> App {
        let mut app = blank(None);
        app.begin_scan(&[]);
        app.apply_top(&Progress::Started {
            roots: names.iter().copied().map(root).collect(),
            jobs: names.len(),
        });
        app
    }

    fn scanned(app: &mut App, index: usize, bytes: u64) {
        app.apply_top(&Progress::Scanned {
            root: index,
            path: PathBuf::from("/tmp/child"),
            root_bytes: bytes,
            remaining: 1,
        });
    }

    fn root_done(app: &mut App, index: usize, bytes: u64, denials: Denials) {
        app.apply_top(&Progress::RootDone {
            root: index,
            denials,
            bytes,
        });
    }

    fn cached(roots: Vec<RootUsage>) -> App {
        let mut app = blank(None);
        app.load_cached(&audit_of(roots), Duration::from_secs(4 * 3600));
        app
    }

    fn category(name: &str, total: u64) -> Category {
        Category {
            name: name.to_string(),
            total_size: total,
            paths: Vec::new(),
        }
    }

    /// A container with volumes no walk can open, which is every Mac. Round
    /// numbers throughout, so a test can add up what the screen prints without
    /// rounding deciding the answer.
    fn container(other_volumes: u64) -> DiskOverview {
        DiskOverview {
            total: 500 * GB,
            free: 100 * GB,
            data_used: 372 * GB,
            other_volumes,
        }
    }

    /// One measurement with something to draw in both views: three roots, two
    /// named categories and the 100 GB remainder no rule named. 372 GB measured,
    /// so the container's 400 GB used is fully accounted for once the volumes the
    /// walk cannot open are counted.
    fn both_views(other_volumes: u64) -> App {
        let mut home = usage("Home", 300 * GB, Denials::default());
        home.unattributed_total = 100 * GB;
        home.unattributed = vec![(PathBuf::from("/tmp/Home/stray"), 100 * GB)];

        let audit = Audit {
            roots: vec![
                home,
                usage("User Library", 60 * GB, Denials::default()),
                usage("opt", 12 * GB, Denials::default()),
            ],
            categories: vec![category("Rust", 200 * GB), category("Xcode", 72 * GB)],
        };

        let mut app = blank(Some(container(other_volumes)));
        app.load_cached(&audit, Duration::ZERO);
        app
    }

    fn drawn(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().to_string()
    }

    /// The figure the header states, taken from the call the header makes.
    fn accounted_figure(app: &App) -> String {
        figures(app.overview.as_ref(), app.accounted())
            .into_iter()
            .find(|(_, label)| *label == "accounted for")
            .expect("a finished measurement states a total")
            .0
    }

    /// Every size the frame prints, parsed back out of the rendered text, so a
    /// test adds up what the screen says rather than what the model holds.
    fn drawn_sizes(screen: &str) -> Vec<u64> {
        let mut sizes = Vec::new();
        for line in screen.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            for pair in words.windows(2) {
                if let (Ok(value), Some(unit)) = (pair[0].parse::<f64>(), unit(pair[1])) {
                    sizes.push((value * unit as f64) as u64);
                }
            }
        }
        sizes
    }

    fn unit(name: &str) -> Option<u64> {
        match name {
            "B" => Some(1),
            "KB" => Some(1_000),
            "MB" => Some(MB),
            "GB" => Some(GB),
            "TB" => Some(1_000 * GB),
            _ => None,
        }
    }

    /// Any glyph the frame draws in the colour of the background behind it, which
    /// is a cell nobody can read. The cursor is a background, so this is what
    /// keeps a row from losing a cell the moment it is selected.
    fn unreadable_cells(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| {
                cell.bg != Color::Reset && cell.fg == cell.bg && !cell.symbol().trim().is_empty()
            })
            .map(|cell| format!("{:?} in {:?}", cell.symbol(), cell.fg))
            .collect()
    }

    fn cursor_on(app: &mut App, row: usize) {
        while app.pane().cursor() > row {
            app.pane_mut().move_up();
        }
        while app.pane().cursor() < row {
            app.pane_mut().move_down();
        }
    }

    /// Everything below the header, so a test about what a row says cannot be
    /// satisfied by a figure in the header instead.
    fn body_of(app: &App, width: u16, height: u16) -> String {
        drawn(app, width, height)
            .lines()
            .skip(3)
            .collect::<Vec<&str>>()
            .join("\n")
    }

    fn walked(app: &mut App, name: &str, path: &Path, total: u64, keys: Pressed) {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        sweep_directory(
            app,
            &mut terminal,
            name.to_string(),
            path.to_path_buf(),
            total,
            &mut || Ok(keys),
        )
        .unwrap();
    }

    fn labels(app: &App) -> Vec<(String, String)> {
        app.pane()
            .items()
            .iter()
            .map(|i| (i.name.clone(), i.size.label()))
            .collect()
    }

    fn names(app: &App) -> Vec<String> {
        app.pane().items().iter().map(|i| i.name.clone()).collect()
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_row_that_has_only_been_started_says_scanning() {
        // The shipped bug: a root that had been announced but had reported
        // nothing sat at a running total of zero and drew "0 B" for the whole
        // two minutes of the walk.
        let app = started(&["opt", "macOS installers"]);

        assert_eq!(
            labels(&app),
            vec![
                ("opt".to_string(), "scanning".to_string()),
                ("macOS installers".to_string(), "scanning".to_string()),
            ]
        );

        let screen = body_of(&app, 100, 12);
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn a_row_part_way_through_being_measured_still_says_scanning() {
        // 4.3 GB of a directory that turns out to hold 184 GB is as wrong as
        // zero, so a running total never reaches the size column.
        let mut app = started(&["Home"]);
        scanned(&mut app, 0, 4 * GB + 300 * MB);

        assert_eq!(labels(&app)[0].1, "scanning");

        let screen = body_of(&app, 100, 12);
        assert!(!screen.contains("4.3 GB"), "{screen}");
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn a_row_shows_a_number_once_its_own_walk_is_finished() {
        let mut app = started(&["Home", "User Library"]);
        scanned(&mut app, 0, 40 * GB);
        root_done(&mut app, 0, 40 * GB, Denials::default());

        assert_eq!(
            labels(&app),
            vec![
                ("Home".to_string(), "40.0 GB".to_string()),
                ("User Library".to_string(), "scanning".to_string()),
            ]
        );
        assert_eq!(
            app.accounted(),
            None,
            "a walk still running has no total to report either"
        );

        let done = cached(vec![
            usage("Home", 40 * GB, Denials::default()),
            usage("User Library", 2 * GB, Denials::default()),
        ]);
        assert_eq!(done.accounted(), Some(42 * GB));
    }

    #[test]
    fn a_root_of_nothing_but_loose_files_settles_the_moment_it_is_done() {
        // Jobs are per subdirectory, so a root with none never gets a Scanned
        // event. Its bytes ride on RootDone instead, which is the moment the row
        // is finished and therefore the moment a number is allowed to be shown.
        let mut app = started(&["cores", "Home"]);
        root_done(&mut app, 0, 8 * 1024, Denials::default());

        assert_eq!(
            labels(&app),
            vec![
                ("cores".to_string(), "8 KB".to_string()),
                ("Home".to_string(), "scanning".to_string()),
            ]
        );
        assert!(body_of(&app, 100, 10).contains("8 KB"));
    }

    #[test]
    fn a_finished_root_that_measured_nothing_and_refused_nothing_is_empty() {
        // Zero bytes with no refusal, once the row is finished, is a
        // measurement: the directory opened and held nothing.
        let mut app = started(&["cores"]);
        root_done(&mut app, 0, 0, Denials::default());

        assert_eq!(labels(&app)[0].1, "0 B");
    }

    #[test]
    fn a_size_still_being_counted_is_not_a_size() {
        assert_eq!(Size::Counting(0).label(), "scanning");
        assert_eq!(Size::Counting(4096).label(), "scanning");
        assert_eq!(Size::Counting(4096).settled(), None);
        // Measured and empty really is empty, and says so.
        assert_eq!(Size::Measured(0).label(), "0 B");
    }

    #[test]
    fn a_refusal_settles_as_unknown_and_measured_bytes_do_not() {
        assert_eq!(Size::Counting(0).settle(protected(1)), Size::Unknown);
        assert_eq!(
            Size::Counting(4096).settle(protected(1)),
            Size::Measured(4096),
            "bytes were counted before the refusal, so the number stands"
        );
        assert_eq!(
            Size::Counting(0).settle(Denials::default()),
            Size::Counting(0),
            "nothing reported is not the same as nothing there"
        );
    }

    #[test]
    fn a_root_that_refused_reads_unknown_while_the_scan_is_still_running() {
        let mut app = started(&["Home", "System data"]);
        scanned(&mut app, 0, 40 * GB);
        root_done(&mut app, 0, 40 * GB, Denials::default());
        root_done(&mut app, 1, 0, protected(3));

        assert_eq!(labels(&app)[1].1, "unknown");

        let screen = body_of(&app, 100, 12);
        assert!(screen.contains("unknown"), "{screen}");
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn a_root_that_refused_still_reads_unknown_once_the_scan_is_done() {
        let app = cached(vec![
            usage("Home", 40 * GB, Denials::default()),
            usage("System data", 0, protected(3)),
        ]);

        assert_eq!(
            labels(&app),
            vec![
                ("Home".to_string(), "40.0 GB".to_string()),
                ("System data".to_string(), "unknown".to_string()),
            ]
        );

        let screen = body_of(&app, 100, 12);
        assert!(screen.contains("unknown"), "{screen}");
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn a_readable_empty_root_is_dropped_rather_than_drawn_as_zero() {
        let app = cached(vec![
            usage("Empty", 0, Denials::default()),
            usage("Real", 4096, Denials::default()),
        ]);

        assert_eq!(names(&app), vec!["Real"]);
    }

    #[test]
    fn the_two_refusals_get_advice_that_matches_them() {
        let notes = denial_notes(protected(3), TERMINAL);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("Full Disk Access"), "{notes:?}");
        assert!(!notes[0].contains("sudo"), "{notes:?}");
        assert!(
            notes[0].contains(TERMINAL),
            "the grant attaches to the terminal application, so the advice has \
             to name it: {notes:?}"
        );

        let notes = denial_notes(forbidden(2), TERMINAL);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("sudo"), "{notes:?}");
        assert!(
            notes[0].contains("not Full Disk Access"),
            "the fix that cannot work has to be ruled out: {notes:?}"
        );

        assert_eq!(
            denial_notes(
                Denials {
                    protected: 1,
                    forbidden: 1,
                    ..Denials::default()
                },
                TERMINAL,
            )
            .len(),
            2,
            "one fix cannot stand in for the other"
        );
        assert!(denial_notes(Denials::default(), TERMINAL).is_empty());
    }

    #[test]
    fn a_directory_the_refresh_left_closed_is_fixed_by_a_keypress_not_a_grant() {
        // Nothing refused it: the scheduled scan chose not to ask. So the note
        // must send the user to the key that measures it, and must not send
        // them to System Settings for a grant that would change nothing.
        let notes = denial_notes(
            Denials {
                unattended: 3,
                ..Denials::default()
            },
            TERMINAL,
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("3 directories"), "{notes:?}");
        assert!(notes[0].contains("Press r"), "{notes:?}");
        assert!(!notes[0].contains("Full Disk Access"), "{notes:?}");
        assert!(!notes[0].contains("sudo"), "{notes:?}");
    }

    #[test]
    fn a_child_the_refresh_left_closed_reads_as_unknown_with_its_own_note() {
        // The cache is what the scheduled refresh wrote, so this is the row the
        // user sees for ~/Documents every time they open the screen.
        let scratch = Scratch::new("closed");
        let documents = scratch.dir("Documents");
        let plain = scratch.dir("plain");
        fs::write(plain.join("f.bin"), vec![0u8; 32 * 1024]).unwrap();

        let mut root = usage(
            "Home",
            32 * 1024,
            Denials {
                unattended: 1,
                ..Denials::default()
            },
        );
        root.path = scratch.path().to_path_buf();
        root.children = vec![(plain, 32 * 1024), (documents.clone(), 0)];
        root.refused_children = vec![Unreadable {
            path: documents,
            denial: Denial::Unattended,
        }];
        let mut app = cached(vec![root]);
        app.pane_mut().enter();

        assert_eq!(
            labels(&app),
            vec![
                ("plain".to_string(), "33 KB".to_string()),
                ("Documents".to_string(), "unknown".to_string()),
            ]
        );
        let denials = app.denials();
        assert_eq!(denials.unattended, 1);
        let notes = denial_notes(denials, TERMINAL);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("Press r"), "{notes:?}");
    }

    #[test]
    fn refusals_are_counted_from_the_counters_not_the_capped_sample() {
        // Each root below lists one unreadable path and counts many, which is
        // what a real sweep reports once a tree passes the sample cap.
        let app = cached(vec![
            usage("Home", 4096, forbidden(7)),
            usage("System data", 0, protected(25)),
        ]);

        let denials = app.denials();
        assert_eq!((denials.protected, denials.forbidden), (25, 7));

        let notes = denial_notes(denials, TERMINAL);
        assert!(notes[0].contains("25 directories"), "{notes:?}");
        assert!(notes[1].contains("7 directories"), "{notes:?}");

        let screen = drawn(&app, 100, 14);
        assert!(screen.contains("25 directories"), "{screen}");
    }

    #[test]
    fn one_refusal_is_not_pluralised() {
        assert_eq!(directories(1), "1 directory");
        assert_eq!(directories(2), "2 directories");
    }

    #[test]
    fn drilling_from_the_top_level_reuses_what_the_sweep_measured() {
        // The subdirectory is empty on disk, so a size anywhere near the cached
        // one could only have come from the cache: walking it would report a
        // couple of blocks at most.
        let scratch = Scratch::new("cached");
        let sub = scratch.dir("sub");
        scratch.file("loose.bin", 64 * 1024);

        let mut root = usage("Scratch", 512 * 1024 + 64 * 1024, Denials::default());
        root.path = scratch.path().to_path_buf();
        root.children = vec![(sub, 512 * 1024)];
        let mut app = cached(vec![root]);

        assert!(
            app.pane_mut().enter().is_none(),
            "a level the sweep already measured must not ask for a walk"
        );
        assert_eq!(app.pane().stack.len(), 2);
        assert!(!app.scanning(), "a lookup is not a scan");

        assert_eq!(
            labels(&app),
            vec![
                ("sub".to_string(), "524 KB".to_string()),
                ("1 loose file".to_string(), "66 KB".to_string()),
            ]
        );
    }

    #[test]
    fn the_rows_of_a_measured_directory_account_for_the_total_above_them() {
        // A listing built from the sweep's children alone would drop the loose
        // files, and the rows would quietly add up to less than the parent row.
        let scratch = Scratch::new("addsup");
        let sub = scratch.dir("sub");
        scratch.file("loose.bin", 64 * 1024);
        let loose = live::children_as_roots(scratch.path()).loose_bytes;

        let mut root = usage("Scratch", 512 * 1024 + loose, Denials::default());
        root.path = scratch.path().to_path_buf();
        root.children = vec![(sub, 512 * 1024)];
        let mut app = cached(vec![root]);
        app.pane_mut().enter();

        let View::Directory { total, .. } = app.pane().current() else {
            panic!("not in a directory");
        };
        assert_eq!(app.listed(), *total, "the rows must account for the total");
        assert_eq!(unlisted_note(&app), None);
    }

    #[test]
    fn a_refused_child_reads_unknown_from_the_cache_rather_than_zero() {
        // The shipped bug, one level down: ~/.Trash is TCC-protected, so the
        // walk reported it with the nothing it could count and the listing drew
        // "0 B" over a directory nobody has ever looked inside. Driven from a
        // cached report, so the report itself is the only thing that can say so.
        let scratch = Scratch::new("refused");
        let locked = scratch.dir("locked");
        let plain = scratch.dir("plain");
        fs::write(plain.join("f.bin"), vec![0u8; 32 * 1024]).unwrap();

        let mut root = usage("Scratch", 32 * 1024, protected(1));
        root.path = scratch.path().to_path_buf();
        root.children = vec![(plain, 32 * 1024), (locked.clone(), 0)];
        root.refused_children = vec![Unreadable {
            path: locked,
            denial: Denial::Protected,
        }];
        let mut app = cached(vec![root]);

        assert!(
            app.pane_mut().enter().is_none(),
            "a cached level must answer without walking anything"
        );
        assert!(!app.scanning());

        assert_eq!(
            labels(&app),
            vec![
                ("plain".to_string(), "33 KB".to_string()),
                ("locked".to_string(), "unknown".to_string()),
            ]
        );

        let screen = body_of(&app, 100, 12);
        assert!(screen.contains("unknown"), "{screen}");
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn the_two_kinds_of_refusal_stay_apart_for_a_refused_child() {
        let scratch = Scratch::new("kinds");
        let privacy = scratch.dir("privacy");
        let unix = scratch.dir("unix");

        let mut root = usage(
            "Scratch",
            32 * 1024,
            Denials {
                protected: 1,
                forbidden: 1,
                ..Denials::default()
            },
        );
        root.path = scratch.path().to_path_buf();
        root.children = vec![(privacy.clone(), 0), (unix.clone(), 0)];
        root.refused_children = vec![
            Unreadable {
                path: privacy,
                denial: Denial::Protected,
            },
            Unreadable {
                path: unix,
                denial: Denial::Forbidden,
            },
        ];
        let mut app = cached(vec![root]);
        app.pane_mut().enter();

        let denials = app.denials();
        assert_eq!(
            (denials.protected, denials.forbidden),
            (1, 1),
            "each refused row carries its own reason"
        );

        let notes = denial_notes(denials, TERMINAL);
        assert_eq!(notes.len(), 2, "one fix cannot stand in for the other");
        assert!(notes[0].contains(TERMINAL), "{notes:?}");
        assert!(notes[1].contains("sudo"), "{notes:?}");

        let screen = body_of(&app, 100, 14);
        assert!(!screen.contains("0 B"), "{screen}");
        assert!(screen.contains(TERMINAL), "{screen}");
    }

    #[test]
    fn a_child_with_refusals_deeper_down_keeps_its_understated_number() {
        // The child opened. Its number is real and merely short, and the
        // shortfall is already reported against the volume, so throwing the
        // measurement away would lose more than it protects.
        let scratch = Scratch::new("deeprefusal");
        let child = scratch.dir("child");

        let mut root = usage("Scratch", 64 * 1024, protected(2));
        root.path = scratch.path().to_path_buf();
        root.children = vec![(child, 64 * 1024)];
        let mut app = cached(vec![root]);
        app.pane_mut().enter();

        assert_eq!(
            labels(&app),
            vec![("child".to_string(), "66 KB".to_string())]
        );
        assert!(!body_of(&app, 100, 12).contains("unknown"));
    }

    #[test]
    fn a_readable_empty_child_still_reads_as_zero() {
        // The one case where zero IS the measurement. Pinned here because the
        // fix for the refused child must not swallow it.
        let scratch = Scratch::new("emptychild");
        let empty = scratch.dir("empty");
        let full = scratch.dir("full");
        fs::write(full.join("f.bin"), vec![0u8; 32 * 1024]).unwrap();

        let mut root = usage("Scratch", 32 * 1024, Denials::default());
        root.path = scratch.path().to_path_buf();
        root.children = vec![(full, 32 * 1024), (empty, 0)];
        let mut app = cached(vec![root]);
        app.pane_mut().enter();

        assert_eq!(
            labels(&app),
            vec![
                ("full".to_string(), "33 KB".to_string()),
                ("empty".to_string(), "0 B".to_string()),
            ]
        );
        assert!(body_of(&app, 100, 12).contains("0 B"));
    }

    #[test]
    fn a_child_counted_under_another_root_is_not_unknown() {
        // The roots partition the volume and exclude each other, so ~/Library is
        // absent from Home's children by design. "unknown" is the wrong word for
        // a directory whose size is on the previous screen.
        let scratch = Scratch::new("counted");
        let library = scratch.dir("Library");
        let caches = scratch.dir("Library/Caches");
        let documents = scratch.dir("Documents");
        fs::write(documents.join("f.bin"), vec![0u8; 128 * 1024]).unwrap();
        fs::write(caches.join("f.bin"), vec![0u8; 64 * 1024]).unwrap();
        scratch.file("loose.bin", 8 * 1024);
        let loose = live::children_as_roots(scratch.path()).loose_bytes;

        let mut home = usage("Home", 128 * 1024 + loose, Denials::default());
        home.path = scratch.path().to_path_buf();
        home.children = vec![(documents, 128 * 1024)];
        let mut user_library = usage("User Library", 512 * 1024, Denials::default());
        user_library.path = library;
        user_library.children = vec![(caches, 64 * 1024)];

        let mut app = cached(vec![home, user_library]);
        app.pane_mut().enter();

        assert_eq!(
            labels(&app),
            vec![
                ("Library".to_string(), "elsewhere".to_string()),
                ("Documents".to_string(), "131 KB".to_string()),
                ("1 loose file".to_string(), "8 KB".to_string()),
            ],
            "a root of its own is neither unknown nor part of this total"
        );

        let View::Directory { total, .. } = app.pane().current() else {
            panic!("not in a directory");
        };
        assert_eq!(
            app.listed(),
            *total,
            "the rows still account for the total above them"
        );
        assert_eq!(unlisted_note(&app), None);

        let screen = body_of(&app, 100, 14);
        assert!(!screen.contains("unknown"), "{screen}");
        assert!(screen.contains("counted under User Library"), "{screen}");

        assert!(
            app.pane_mut().enter().is_none(),
            "the other root already measured this level"
        );
        assert!(
            names(&app).contains(&"Caches".to_string()),
            "{:?}",
            names(&app)
        );
    }

    #[test]
    fn a_listing_that_cannot_show_everything_says_how_much_it_left_out() {
        let mut root = usage("Home", 2 * GB, Denials::default());
        root.unattributed_total = 2 * GB;
        root.unattributed = vec![(PathBuf::from("/tmp/one"), 512 * MB)];

        let mut app = blank(None);
        app.load_cached(&audit_of(vec![root]), Duration::ZERO);
        app.toggle();
        assert_eq!(names(&app), vec![UNNAMED.to_string()]);

        app.pane_mut().enter();
        let note = unlisted_note(&app).expect("bytes are missing from this listing");
        assert!(note.contains(&format_size(2 * GB - 512 * MB)), "{note}");
        assert!(note.contains("not listed"), "{note}");
        assert!(drawn(&app, 100, 12).contains("not listed"));
    }

    #[test]
    fn a_walked_directory_lists_its_loose_files_beside_its_subdirectories() {
        let scratch = Scratch::new("walk");
        let sub = scratch.dir("sub");
        fs::write(sub.join("deep.bin"), vec![0u8; 512 * 1024]).unwrap();
        scratch.file("loose.bin", 64 * 1024);

        let mut app = started(&["Home"]);
        walked(&mut app, "scratch", scratch.path(), 0, Pressed::default());

        assert_eq!(
            names(&app),
            vec!["sub", "1 loose file"],
            "biggest first once the numbers are final"
        );
        assert!(app.pane().items()[0].size.bytes() >= 512 * 1024);
        assert!(app.pane().items()[1].size.bytes() >= 64 * 1024);
        assert!(!app.scanning());
    }

    #[test]
    fn a_walked_directory_hands_the_next_level_down_its_own_children() {
        let scratch = Scratch::new("deep");
        let sub = scratch.dir("sub");
        fs::create_dir_all(sub.join("inner")).unwrap();
        fs::write(sub.join("inner").join("f.bin"), vec![0u8; 128 * 1024]).unwrap();

        let mut app = started(&["Home"]);
        walked(&mut app, "scratch", scratch.path(), 0, Pressed::default());

        assert!(
            matches!(app.pane().items()[0].down, Down::Children(_)),
            "the walk measured these, so the level below is a lookup"
        );
        assert!(app.pane_mut().enter().is_none());
        assert_eq!(labels(&app)[0].0, "inner");
    }

    #[test]
    fn stopping_a_drill_down_goes_back_instead_of_quitting() {
        let scratch = Scratch::new("cancel");
        for i in 0..8 {
            let sub = scratch.dir(&format!("sub{i}"));
            fs::write(sub.join("f.bin"), vec![0u8; 64 * 1024]).unwrap();
        }

        let mut app = started(&["Home"]);
        walked(
            &mut app,
            "scratch",
            scratch.path(),
            0,
            Pressed {
                stop: Some(Stop::Back),
                toggle: false,
            },
        );

        assert_eq!(
            app.pane().stack.len(),
            1,
            "stopping returns to the parent view"
        );
        assert!(app.pane().at_top());
        assert!(!app.quit, "Esc inside a drill-down is not a quit");
        assert!(!app.scanning());
    }

    #[test]
    fn quitting_during_a_drill_down_still_quits() {
        let scratch = Scratch::new("quit");
        scratch.dir("sub");

        let mut app = started(&["Home"]);
        walked(
            &mut app,
            "scratch",
            scratch.path(),
            0,
            Pressed {
                stop: Some(Stop::Quit),
                toggle: false,
            },
        );

        assert!(app.quit);
    }

    #[test]
    fn a_directory_that_will_not_open_is_unknown_not_empty() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("locked");
        let locked = scratch.dir("locked");
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let mut app = started(&["Home"]);
        walked(&mut app, "locked", &locked, 0, Pressed::default());
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        assert!(app.pane().items().is_empty());
        assert_eq!(empty_state(&app), ("  unknown", Color::Red));
        let notes = denial_notes(app.denials(), TERMINAL);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("sudo"), "{notes:?}");

        let screen = body_of(&app, 100, 12);
        assert!(screen.contains("unknown"), "{screen}");
        assert!(!screen.contains("(empty)"), "{screen}");
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn a_readable_empty_directory_still_says_empty() {
        let scratch = Scratch::new("empty");
        let empty = scratch.dir("empty");

        let mut app = started(&["Home"]);
        walked(&mut app, "empty", &empty, 0, Pressed::default());

        assert!(app.pane().items().is_empty());
        assert_eq!(empty_state(&app), ("  (empty)", Color::DarkGray));
        assert!(denial_notes(app.denials(), TERMINAL).is_empty());
        assert!(drawn(&app, 100, 12).contains("(empty)"));
    }

    #[test]
    fn the_naming_view_adds_up_to_the_partition() {
        // The invariant the whole report rests on: measured equals attributed
        // plus unattributed. Two views of one measurement must not disagree.
        let scratch = Scratch::new("invariant");
        let named = scratch.dir("named");
        let stray = scratch.dir("stray");
        fs::write(named.join("a.bin"), vec![0u8; 256 * 1024]).unwrap();
        fs::write(stray.join("b.bin"), vec![0u8; 512 * 1024]).unwrap();

        let roots = vec![Root {
            name: "Scratch".into(),
            path: scratch.path().to_path_buf(),
        }];
        let rules = vec![Rule {
            category: "Named".into(),
            label: "named".into(),
            path: named,
        }];
        let audit = audit::scan_with(&roots, &rules);
        let measured = audit.measured();

        let mut app = blank(None);
        app.load_cached(&audit, Duration::ZERO);

        assert_eq!(app.listed(), measured, "the partition");
        app.toggle();
        assert_eq!(app.listed(), measured, "the naming layer over it");
        assert_eq!(
            names(&app),
            vec!["Named".to_string(), UNNAMED.to_string()],
            "the remainder no rule named gets a row of its own"
        );
    }

    #[test]
    fn drilling_into_a_category_lists_the_paths_it_is_made_of() {
        let audit = Audit {
            roots: vec![usage("Home", 3 * 4096, Denials::default())],
            categories: vec![Category {
                name: "Rust".into(),
                total_size: 3 * 4096,
                paths: vec![
                    CategoryPath {
                        label: "cargo registry".into(),
                        path: PathBuf::from("/tmp/cargo"),
                        size: 2 * 4096,
                    },
                    CategoryPath {
                        label: "rustup".into(),
                        path: PathBuf::from("/tmp/rustup"),
                        size: 4096,
                    },
                ],
            }],
        };

        let mut app = blank(None);
        app.load_cached(&audit, Duration::ZERO);
        app.toggle();
        assert_eq!(
            labels(&app),
            vec![("Rust".to_string(), "12 KB".to_string())]
        );

        assert!(app.pane_mut().enter().is_none(), "a category needs no walk");
        assert_eq!(
            labels(&app),
            vec![
                ("cargo registry".to_string(), "8 KB".to_string()),
                ("rustup".to_string(), "4 KB".to_string()),
            ]
        );
        assert_eq!(app.pane().breadcrumb(), "What it is > Rust");
    }

    #[test]
    fn the_naming_view_has_its_rows_before_a_walk_can_size_them() {
        let rules = attribution_rules();
        let mut app = blank(None);
        app.begin_scan(&rules);
        app.toggle();

        assert!(!app.pane().items().is_empty(), "blank for two minutes");
        assert!(
            app.pane()
                .items()
                .iter()
                .all(|i| i.size.label() == "scanning"),
            "only a finished sweep can size a category"
        );

        assert!(drawn(&app, 100, 12).contains("What it is"));
        let screen = body_of(&app, 100, 12);
        assert!(!screen.contains("0 B"), "{screen}");
    }

    #[test]
    fn switching_views_leaves_a_running_scan_alone() {
        let mut app = started(&["Home", "User Library"]);
        scanned(&mut app, 0, 8 * GB);
        let before = labels(&app);

        app.toggle();
        assert_eq!(app.section, Section::Naming);
        assert!(app.scanning(), "switching views is not stopping the walk");
        assert_eq!(app.jobs_left, 1);

        app.toggle();
        assert_eq!(app.section, Section::Space);
        assert_eq!(labels(&app), before, "the walk kept going, nothing reset");
    }

    #[test]
    fn switching_views_during_a_drill_down_does_not_lose_the_listing() {
        let scratch = Scratch::new("toggle");
        let sub = scratch.dir("sub");
        fs::write(sub.join("f.bin"), vec![0u8; 128 * 1024]).unwrap();

        let mut app = started(&["Home"]);
        walked(
            &mut app,
            "scratch",
            scratch.path(),
            0,
            Pressed {
                stop: None,
                toggle: true,
            },
        );

        // The toggling landed somewhere, but the walk it ran through still
        // filled in the listing it was building.
        app.section = Section::Space;
        assert_eq!(names(&app), vec!["sub"]);
        assert!(app.pane().items()[0].size.bytes() >= 128 * 1024);
    }

    #[test]
    fn each_view_keeps_its_own_place() {
        let audit = Audit {
            roots: vec![
                usage("Home", 8192, Denials::default()),
                usage("Applications", 4096, Denials::default()),
            ],
            categories: vec![Category {
                name: "Rust".into(),
                total_size: 4096,
                paths: Vec::new(),
            }],
        };
        let mut app = blank(None);
        app.load_cached(&audit, Duration::ZERO);

        app.pane_mut().move_down();
        assert_eq!(app.pane().cursor(), 1);

        app.toggle();
        assert_eq!(app.pane().cursor(), 0, "a fresh view starts at the top");

        app.toggle();
        assert_eq!(app.pane().cursor(), 1, "and the old one is where it was");
    }

    #[test]
    fn the_footer_offers_stopping_during_a_scan_and_everything_after() {
        let mut app = started(&["Home", "User Library"]);
        app.jobs_left = 42;

        let hints = text(&footer(&app));
        assert_eq!(hints, format!(" Scanning: 42 left {SCAN_KEYS}"));
        assert!(hints.contains("[Tab] View"), "{hints}");
        assert!(!hints.contains("[Enter]"), "{hints}");

        app.stop();
        assert_eq!(text(&footer(&app)), KEYS);
        assert!(KEYS.contains("[Tab] View"));
    }

    #[test]
    fn the_header_says_how_old_the_numbers_it_is_drawing_are() {
        let app = cached(vec![usage("Home", 4096, Denials::default())]);
        let screen = drawn(&app, 100, 10);

        assert!(screen.contains("wsctl disk audit"), "{screen}");
        assert!(screen.contains("Where the space is"), "{screen}");
        assert!(screen.contains("4h ago"), "{screen}");
    }

    #[test]
    fn the_header_admits_a_scan_is_running_and_a_report_is_being_written() {
        let mut app = started(&["Home"]);
        assert!(drawn(&app, 100, 10).contains("scanning"));

        app.status = Status::Saving;
        let screen = drawn(&app, 100, 10);
        assert!(screen.contains("saving"), "{screen}");
        assert!(screen.contains("Saving the report"), "{screen}");
    }

    #[test]
    fn a_stopped_scan_does_not_claim_an_age_it_does_not_have() {
        let mut app = started(&["Home"]);
        app.stop();
        app.age = None;

        assert!(drawn(&app, 100, 10).contains("partial"));
    }

    #[test]
    fn the_breadcrumb_follows_the_stack_down() {
        let scratch = Scratch::new("crumb");
        let sub = scratch.dir("sub");
        fs::create_dir_all(sub.join("inner")).unwrap();

        let mut app = started(&["Home"]);
        walked(&mut app, "scratch", scratch.path(), 0, Pressed::default());
        assert_eq!(app.pane().breadcrumb(), "Where the space is > scratch");

        app.pane_mut().enter();
        assert_eq!(
            app.pane().breadcrumb(),
            "Where the space is > scratch > sub"
        );

        assert!(!app.pane_mut().pop_or_quit());
        assert!(!app.pane_mut().pop_or_quit());
        assert!(app.pane_mut().pop_or_quit(), "Esc at the root leaves");
    }

    #[test]
    fn the_figures_name_what_the_walk_could_not_reach() {
        let overview = DiskOverview {
            total: 500 * GB,
            free: 100 * GB,
            data_used: 380 * GB,
            other_volumes: 20 * GB,
        };

        // 380 GB of roots plus the 20 GB of volumes the walk cannot open, which
        // arrives already summed: every byte in this figure is on a row.
        let reachable = figures(Some(&overview), Some(400 * GB));
        let labelled: Vec<&str> = reachable.iter().map(|(_, label)| *label).collect();
        assert_eq!(labelled, ["used", "total", "free", "accounted for"]);
        assert_eq!(reachable[3].0, format_size(400 * GB));

        // Half the volume unmeasured is a gap the screen has to own up to.
        let missed = figures(Some(&overview), Some(200 * GB));
        assert_eq!(missed[4].1, "not reachable by this scan");
        assert_eq!(missed[4].0, format_size(200 * GB));
    }

    #[test]
    fn an_unfinished_walk_contributes_no_figure_at_all() {
        let overview = DiskOverview {
            total: 500 * GB,
            free: 100 * GB,
            data_used: 380 * GB,
            other_volumes: 20 * GB,
        };

        let running = figures(Some(&overview), None);
        let labelled: Vec<&str> = running.iter().map(|(_, label)| *label).collect();
        assert_eq!(
            labelled,
            ["used", "total", "free"],
            "a part-measured volume is not 'unreachable', it is unfinished"
        );
    }

    #[test]
    fn a_narrow_header_drops_a_whole_figure_rather_than_half_a_word() {
        let overview = DiskOverview {
            total: 500 * GB,
            free: 100 * GB,
            data_used: 380 * GB,
            other_volumes: 20 * GB,
        };
        let all = figures(Some(&overview), Some(180 * GB));
        assert_eq!(all.len(), 5);

        let wide = fit(all.clone(), 200);
        assert_eq!(wide.len(), 5, "nothing to drop when it all fits");
        assert!(figures_width(&wide) <= 200);

        for width in [40u16, 60, 80, 100, 120] {
            let fitted = fit(all.clone(), width);
            assert!(
                figures_width(&fitted) <= width as usize || fitted.len() == 1,
                "{width} columns still overflows: {fitted:?}"
            );
        }

        // Something is always shown, even where nothing can fit.
        assert_eq!(fit(all, 1).len(), 1);
    }

    #[test]
    fn the_figures_survive_having_no_container_to_report_on() {
        let only_measured = figures(None, Some(4096));
        assert_eq!(only_measured.len(), 1);
        assert_eq!(only_measured[0], ("4 KB".to_string(), "accounted for"));
        assert!(figures(None, None).is_empty());
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut app = started(&["Home", "User Library"]);
        app.pane_mut().move_up();
        assert_eq!(app.pane().cursor(), 0);

        app.pane_mut().move_down();
        app.pane_mut().move_down();
        app.pane_mut().move_down();
        assert_eq!(app.pane().cursor(), 1);
    }

    #[test]
    fn a_terminal_too_small_for_the_layout_still_draws() {
        let mut app = cached(vec![usage("Home", 4096, protected(2))]);
        scanned(&mut app, 0, 1024);

        for (width, height) in [(1, 1), (4, 2), (20, 3), (40, 6)] {
            drawn(&app, width, height);
        }
    }

    #[test]
    fn the_rows_on_screen_add_up_to_the_figure_the_header_states() {
        // The shipped bug: the header said 373.7 GB accounted for and the rows
        // came to 343.0 GB, because the volumes the walk cannot open were in the
        // figure and on no row. Both views state that same figure, so the sum of
        // what each one draws has to reach it. Read off the frame rather than off
        // the model: the model agreeing with itself is what shipped.
        let mut app = both_views(28 * GB);
        let stated = accounted_figure(&app);

        for section in [Section::Space, Section::Naming] {
            app.section = section;
            let screen = drawn(&app, 100, 20);
            assert!(
                screen.contains(&format!("{stated} accounted for")),
                "{screen}"
            );

            let rows: u64 = drawn_sizes(&body_of(&app, 100, 20)).iter().sum();
            assert_eq!(
                format_size(rows),
                stated,
                "the rows the {section:?} view draws do not reach the header figure: {screen}"
            );
            assert_eq!(
                app.listed(),
                app.accounted().expect("a finished measurement"),
                "{section:?}"
            );
        }
    }

    #[test]
    fn the_volumes_no_walk_can_open_get_a_row_in_both_views() {
        let mut app = both_views(28 * GB);

        assert_eq!(
            names(&app),
            vec!["Home", "User Library", "Other volumes", "opt"],
            "on its own size, not pinned to an end"
        );
        let screen = body_of(&app, 100, 20);
        assert!(
            screen.contains("System, Preboot, Recovery, VM"),
            "the row says what it is, since there is no path to name: {screen}"
        );

        app.toggle();
        assert_eq!(
            names(&app),
            vec!["Rust", "Xcode", UNNAMED, "Other volumes"],
            "after the remainder no rule named: no rule could ever name these"
        );
    }

    #[test]
    fn a_container_holding_nothing_else_gets_no_such_row() {
        let mut app = both_views(0);

        assert_eq!(names(&app), vec!["Home", "User Library", "opt"]);
        app.toggle();
        assert_eq!(names(&app), vec!["Rust", "Xcode", UNNAMED]);
        assert_eq!(
            app.accounted(),
            Some(372 * GB),
            "the header states what the rows show and nothing more"
        );
    }

    #[test]
    fn enter_on_the_volumes_no_walk_can_open_goes_nowhere() {
        // Nothing on this machine can look inside them, so a drill would be a
        // dead end. The folder glyph is the promise of a level below, and this
        // row must not make it.
        let mut app = both_views(28 * GB);
        cursor_on(&mut app, 2);

        let row = app.pane().selected().expect("a row under the cursor");
        assert_eq!(row.name, "Other volumes");
        assert!(!row.drillable());

        assert!(app.pane_mut().enter().is_none(), "nothing to walk");
        assert_eq!(app.pane().stack.len(), 1, "Enter did not open a level");
        assert!(app.pane().at_top());

        let line = body_of(&app, 100, 20)
            .lines()
            .find(|line| line.contains("Other volumes"))
            .expect("the row is drawn")
            .to_string();
        assert!(!line.contains('\u{1f4c1}'), "{line}");

        app.toggle();
        cursor_on(&mut app, 3);
        assert_eq!(app.pane().selected().unwrap().name, "Other volumes");
        assert!(app.pane_mut().enter().is_none());
        assert_eq!(app.pane().stack.len(), 1);
    }

    #[test]
    fn no_cell_on_the_cursor_row_is_drawn_in_the_colour_behind_it() {
        // The shipped bug: the cursor row is a DarkGray background and the path
        // cell is DarkGray text, so the selected row was the one row whose path
        // could not be read. Every cell of every row is checked rather than the
        // two that were wrong, so a DarkGray cell added later fails here too.
        let mut app = both_views(28 * GB);

        for section in [Section::Space, Section::Naming] {
            app.section = section;
            for row in 0..app.pane().items().len() {
                cursor_on(&mut app, row);
                let hidden = unreadable_cells(&app, 100, 20);
                assert!(hidden.is_empty(), "{section:?} row {row}: {hidden:?}");
            }
        }
    }

    #[test]
    fn the_selected_row_still_shows_its_path() {
        let app = both_views(28 * GB);
        let line = body_of(&app, 100, 20)
            .lines()
            .find(|line| line.contains("Home"))
            .expect("the first row is drawn")
            .to_string();

        assert_eq!(app.pane().cursor(), 0, "the cursor starts on this row");
        assert!(line.contains("/tmp/Home"), "{line}");
        assert!(unreadable_cells(&app, 100, 20).is_empty());
    }

    #[test]
    fn a_size_the_screen_will_not_print_stays_readable_under_the_cursor() {
        // Both of the sizes that are not Yellow, on the row a cursor lands on
        // first: "elsewhere" is DarkGray because it is not a size, and it is the
        // biggest row in a home folder listing.
        let scratch = Scratch::new("cursorcolour");
        let library = scratch.dir("Library");
        let documents = scratch.dir("Documents");
        let locked = scratch.dir("locked");
        fs::write(documents.join("f.bin"), vec![0u8; 128 * 1024]).unwrap();
        scratch.file("loose.bin", 8 * 1024);

        let mut home = usage("Home", 128 * 1024, protected(1));
        home.path = scratch.path().to_path_buf();
        home.children = vec![(documents, 128 * 1024), (locked.clone(), 0)];
        home.refused_children = vec![Unreadable {
            path: locked,
            denial: Denial::Protected,
        }];
        let mut user_library = usage("User Library", 512 * 1024, Denials::default());
        user_library.path = library;

        let mut app = cached(vec![home, user_library]);
        app.pane_mut().enter();

        let drawn = body_of(&app, 100, 16);
        assert!(drawn.contains("elsewhere"), "{drawn}");
        assert!(drawn.contains("unknown"), "{drawn}");

        for row in 0..app.pane().items().len() {
            cursor_on(&mut app, row);
            let hidden = unreadable_cells(&app, 100, 16);
            assert!(hidden.is_empty(), "row {row}: {hidden:?}");
        }
    }
}
