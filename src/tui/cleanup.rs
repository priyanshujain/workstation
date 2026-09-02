//! The cleanup list, measured while you are looking at it.
//!
//! Every size here comes from walking a tree, and the old screen waited for all
//! of that before drawing anything: a blank terminal for tens of seconds at the
//! top level, and again for minutes on a drill-down, with no way to stop it.
//! Now the rows are known from the first frame and each one fills in when its
//! own walk finishes.
//!
//! A row that is not finished says so. A running total is a floor, and printing
//! a floor in the size column claims a measurement that has not happened yet:
//! `4 GB` for something that turns out to be 184 GB is the same lie as `0 B`
//! for something nobody has looked at. Only a finished walk gets a number.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use disk::cleanup::{
    CleanAction, Measure, Size, Target, discover_all_targets, idle_candidates, idle_child,
    measurement_groups,
};
use disk::sweep::{self, Denial, Denials, Flow, Progress, Root};
use disk::util::{allocated, format_size};
use ratatui::{prelude::*, widgets::*};

use crate::tui::live::{self, Throttle};
use crate::tui::widgets::centered_rect;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

const BROWSE_KEYS: &str = " [Space] Mark  [Enter] Drill  [Esc] Back  [q] Quit";
const CLEAN_KEYS: &str = " [Space] Mark  [Enter] Drill  [x] Clean  [Esc] Back  [q] Quit";
/// Drilling and cleaning would both start work on top of a running walk, so
/// neither is offered until it finishes.
const MEASURE_KEYS: &str = " [Space] Mark  [Esc] Stop  [q] Quit";

struct App {
    /// Discovery order, never reordered: a mark and a cursor both name a target
    /// by its position here, and sizes arrive out of order.
    targets: Vec<Target>,
    /// The order the screen draws, re-sorted as sizes land.
    order: Vec<usize>,
    marked_targets: HashSet<usize>,
    marked_paths: HashMap<PathBuf, MarkedPath>,
    stack: Vec<View>,
    listings: HashMap<PathBuf, Listing>,
    mode: Mode,
    measuring: bool,
    quit: bool,
    results: Vec<CleanResult>,
    done_scroll: u16,
}

struct MarkedPath {
    is_dir: bool,
    size: Size,
}

/// One directory, measured live. Entries keep their position for life, so
/// `order` can be thrown away and rebuilt without moving a mark.
struct Listing {
    entries: Vec<Entry>,
    order: Vec<usize>,
    /// Every non-directory entry, hardlinks counted once. The rows show what
    /// each file allocates; this is what the directory is charged for.
    loose_bytes: u64,
    denied: Option<Denial>,
}

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: Size,
}

enum View {
    Top {
        cursor: usize,
        /// False until the user moves the cursor or marks a row. Row zero is
        /// where a cursor starts because it has to start somewhere, and that is
        /// not the same as a row the user chose.
        picked: bool,
    },
    Directory {
        path: PathBuf,
        cursor: usize,
        picked: bool,
        crumb: String,
    },
}

#[derive(PartialEq, Eq)]
enum Mode {
    Browse,
    Confirm,
    Running,
    Done,
}

struct PendingOp {
    label: String,
    size: Size,
    action: CleanAction,
}

struct CleanResult {
    label: String,
    outcome: Result<u64, String>,
    /// False when the bytes are a guess. A command prunes what it decides to
    /// prune and reports nothing back, so what it freed is its own business.
    known: bool,
}

impl App {
    fn new(targets: Vec<Target>) -> Self {
        Self {
            order: (0..targets.len()).collect(),
            targets,
            marked_targets: HashSet::new(),
            marked_paths: HashMap::new(),
            stack: vec![View::Top {
                cursor: 0,
                picked: false,
            }],
            listings: HashMap::new(),
            mode: Mode::Browse,
            measuring: false,
            quit: false,
            results: Vec::new(),
            done_scroll: 0,
        }
    }

    fn current(&self) -> &View {
        self.stack.last().expect("stack never empty")
    }

    fn breadcrumb(&self) -> String {
        let mut parts: Vec<String> = vec!["Workstation".to_string()];
        for v in &self.stack {
            if let View::Directory { crumb, .. } = v {
                parts.push(crumb.clone());
            }
        }
        parts.join(" > ")
    }

    fn listing(&self) -> Option<&Listing> {
        match self.current() {
            View::Top { .. } => None,
            View::Directory { path, .. } => self.listings.get(path),
        }
    }

    fn rows(&self) -> usize {
        match self.current() {
            View::Top { .. } => self.order.len(),
            View::Directory { .. } => self.listing().map_or(0, |l| l.order.len()),
        }
    }

    fn move_up(&mut self) {
        match self.stack.last_mut().expect("stack never empty") {
            View::Top { cursor, picked } | View::Directory { cursor, picked, .. } => {
                *cursor = cursor.saturating_sub(1);
                *picked = true;
            }
        }
    }

    fn move_down(&mut self) {
        let max = self.rows().saturating_sub(1);
        match self.stack.last_mut().expect("stack never empty") {
            View::Top { cursor, picked } | View::Directory { cursor, picked, .. } => {
                if *cursor < max {
                    *cursor += 1;
                }
                *picked = true;
            }
        }
    }

    /// Marking a row is choosing it, so the cursor follows it from here on.
    fn pick(&mut self) {
        match self.stack.last_mut().expect("stack never empty") {
            View::Top { picked, .. } | View::Directory { picked, .. } => *picked = true,
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

    /// The target the cursor is on, by its position in `targets` rather than on
    /// screen: the screen order changes under it.
    fn selected_target(&self) -> Option<usize> {
        match self.current() {
            View::Top { cursor, .. } => self.order.get(*cursor).copied(),
            View::Directory { .. } => None,
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        match self.current() {
            View::Top { .. } => None,
            View::Directory { path, cursor, .. } => {
                let listing = self.listings.get(path)?;
                listing.entries.get(*listing.order.get(*cursor)?)
            }
        }
    }

    fn toggle_mark(&mut self) {
        self.pick();
        if let Some(id) = self.selected_target() {
            if self.marked_targets.contains(&id) {
                self.marked_targets.remove(&id);
            } else {
                self.marked_targets.insert(id);
            }
            return;
        }

        let Some((path, is_dir, size)) = self
            .selected_entry()
            .map(|e| (e.path.clone(), e.is_dir, e.size))
        else {
            return;
        };
        use std::collections::hash_map::Entry as MapEntry;
        match self.marked_paths.entry(path) {
            MapEntry::Occupied(e) => {
                e.remove();
            }
            MapEntry::Vacant(e) => {
                e.insert(MarkedPath { is_dir, size });
            }
        }
    }

    fn marked_count(&self) -> usize {
        self.marked_targets.len() + self.marked_paths.len()
    }

    fn marked_sizes(&self) -> Vec<Size> {
        self.marked_targets
            .iter()
            .filter_map(|&i| self.targets.get(i).map(|t| t.size()))
            .chain(self.marked_paths.values().map(|m| m.size))
            .collect()
    }

    fn pending_ops(&self) -> Vec<PendingOp> {
        let mut ops: Vec<PendingOp> = Vec::new();

        // Collect contents-roots from marked targets so we can dedupe paths
        // already covered by a marked ancestor.
        let mut content_roots: Vec<PathBuf> = Vec::new();
        for &i in &self.marked_targets {
            if let Some(t) = self.targets.get(i)
                && let CleanAction::RemoveContents(p) | CleanAction::RemoveDir(p) = t.action()
            {
                content_roots.push(p.clone());
            }
        }

        for &i in &self.marked_targets {
            let Some(t) = self.targets.get(i) else {
                continue;
            };
            ops.push(PendingOp {
                label: format!("{}: {}", t.name, t.description),
                size: t.size(),
                action: clone_action(t.action()),
            });
        }

        for (path, info) in &self.marked_paths {
            if content_roots.iter().any(|root| path.starts_with(root)) {
                continue;
            }
            let action = if info.is_dir {
                CleanAction::RemoveDir(path.clone())
            } else {
                CleanAction::RemoveFile(path.clone())
            };
            ops.push(PendingOp {
                label: path.display().to_string(),
                size: info.size,
                action,
            });
        }

        ops.sort_by_key(|o| std::cmp::Reverse(o.size.bytes()));
        ops
    }

    fn total_freed(&self) -> (u64, bool) {
        freed_total(&self.results)
    }

    /// Rows of the view being looked at that are still being walked. A
    /// drill-down counts its own children, not the list it came from.
    fn rows_measuring(&self) -> usize {
        let counting = |size: Size| matches!(size, Size::Counting(_));
        match self.current() {
            View::Top { .. } => self.targets.iter().filter(|t| counting(t.size())).count(),
            View::Directory { .. } => self
                .listing()
                .map_or(0, |l| l.entries.iter().filter(|e| counting(e.size)).count()),
        }
    }

    /// The cursor of every view is moved with its row, not left on a line
    /// number: a view further down the stack is still where the user comes back
    /// to.
    fn resort_top(&mut self) {
        let after = size_order(&self.targets.iter().map(|t| t.size()).collect::<Vec<_>>());
        for view in self.stack.iter_mut() {
            if let View::Top { cursor, picked } = view {
                *cursor = keep_cursor(&self.order, &after, *cursor, *picked);
            }
        }
        self.order = after;
    }

    fn resort_listing(&mut self, dir: &Path) {
        let Some(listing) = self.listings.get_mut(dir) else {
            return;
        };
        let after = size_order(&listing.entries.iter().map(|e| e.size).collect::<Vec<_>>());
        for view in self.stack.iter_mut() {
            if let View::Directory {
                path,
                cursor,
                picked,
                ..
            } = view
                && path == dir
            {
                *cursor = keep_cursor(&listing.order, &after, *cursor, *picked);
            }
        }
        listing.order = after;
    }

    /// One immediate child of a directory reported its running total. It is a
    /// floor until the child is done, so it only changes the sort.
    fn child_counting(&mut self, dir: &Path, child: usize, bytes: u64) {
        if let Some(entry) = self
            .listings
            .get_mut(dir)
            .and_then(|l| l.entries.get_mut(child))
        {
            entry.size = Size::Counting(bytes);
        }
    }

    /// Sorting is the caller's call: finishing a whole directory at once should
    /// sort once, not once per row.
    fn child_done(&mut self, dir: &Path, child: usize, size: Size) {
        let Some(entry) = self
            .listings
            .get_mut(dir)
            .and_then(|l| l.entries.get_mut(child))
        else {
            return;
        };
        entry.size = size;
        if let Some(marked) = self.marked_paths.get_mut(&entry.path) {
            marked.size = size;
        }
    }

    /// Nothing is going to measure the rest, so stop saying it is being
    /// measured. What was counted stays, as the floor it always was.
    fn give_up(&mut self) {
        for target in self.targets.iter_mut() {
            if let Size::Counting(bytes) = target.size() {
                target.set_size(Size::Unknown(bytes));
            }
        }
    }

    fn give_up_listing(&mut self, dir: &Path) {
        let Some(listing) = self.listings.get_mut(dir) else {
            return;
        };
        for entry in listing.entries.iter_mut() {
            if let Size::Counting(bytes) = entry.size {
                entry.size = Size::Unknown(bytes);
            }
        }
    }
}

/// What a row says about its own size.
///
/// Only a finished walk gets a number. Everything else is a floor, and a floor
/// in the size column reads as a measurement: that is how a directory nobody
/// has opened ends up printed as `0 B`.
fn size_label(size: Size) -> String {
    match size {
        Size::Counting(_) => "scanning".to_string(),
        Size::Unknown(_) => "unknown".to_string(),
        Size::Measured { bytes, denials } if bytes == 0 && denials.total() > 0 => {
            "unknown".to_string()
        }
        Size::Measured { bytes, .. } => format_size(bytes),
    }
}

/// The secondary cell: what the row is, or why its number is not the whole
/// story. The two refusals never merge, because Full Disk Access does nothing
/// for a root-owned directory and sudo does nothing for a TCC-protected one.
fn note(size: Size, description: &str) -> String {
    let denials = size.denials();
    match (denials.protected, denials.forbidden) {
        (0, 0) => description.to_string(),
        (protected, 0) => format!("{protected} unreadable, needs Full Disk Access"),
        (0, forbidden) => format!("{forbidden} unreadable, needs sudo"),
        (protected, forbidden) => {
            format!("{protected} need Full Disk Access, {forbidden} need sudo")
        }
    }
}

/// What to do about a directory that would not open at all.
fn denial_advice(denial: Denial) -> &'static str {
    match denial {
        Denial::Protected => "Grant the terminal Full Disk Access to read it",
        Denial::Forbidden => "Unix permissions deny it, reading it needs sudo",
        Denial::Unattended => {
            "Left closed: macOS asks before an app opens it, so measure it from here"
        }
    }
}

/// What is marked, and whether that number is the whole of it.
///
/// Anything still counting, refused, or unmeasurable contributes the floor it
/// counted and costs the total its certainty. Ignoring those rows silently
/// would understate a delete that is about to happen.
fn marked_total(sizes: &[Size]) -> (u64, bool) {
    let mut bytes = 0u64;
    let mut complete = true;
    for size in sizes {
        bytes += size.bytes();
        complete &= size.total().is_some();
    }
    (bytes, complete)
}

/// Freed bytes, and whether that is the whole of it. A command's own guess is
/// left out rather than added in: it was never a measurement of anything.
fn freed_total(results: &[CleanResult]) -> (u64, bool) {
    let mut bytes = 0u64;
    let mut complete = true;
    for result in results {
        if let Ok(freed) = result.outcome {
            bytes += if result.known { freed } else { 0 };
            complete &= result.known;
        }
    }
    (bytes, complete)
}

/// What one finished action freed. A command's own number is not ours to
/// report: `brew cleanup` is told to prune and says nothing about how much it
/// pruned, so printing the cache size we measured beforehand credits it with
/// deleting everything it was pointed at.
fn freed_label(bytes: u64, known: bool) -> String {
    if known {
        format_size(bytes)
    } else {
        "unknown".to_string()
    }
}

fn total_label(bytes: u64, complete: bool) -> String {
    if complete {
        format_size(bytes)
    } else if bytes == 0 {
        "size unknown".to_string()
    } else {
        format!("at least {}", format_size(bytes))
    }
}

/// Biggest first, from sizes that are still arriving. Equal sizes keep the
/// order they were discovered in, so nothing shuffles for no reason.
fn size_order(sizes: &[Size]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sizes[i].bytes()));
    order
}

/// Where the cursor lands after a re-sort.
///
/// A row number means nothing across one: the user was looking at an item, not
/// at a line. Until they have picked an item, though, what they are looking at
/// IS the line, and dragging an untouched cursor down the list as sizes arrive
/// walks it away from the rows worth reading.
fn keep_cursor(before: &[usize], after: &[usize], cursor: usize, picked: bool) -> usize {
    let last = after.len().saturating_sub(1);
    if !picked {
        return cursor.min(last);
    }
    let Some(id) = before.get(cursor) else {
        return cursor.min(last);
    };
    after
        .iter()
        .position(|other| other == id)
        .unwrap_or(cursor.min(last))
}

/// A directory's own size, from the rows it just measured. The loose files are
/// part of it: no child root accounts for them, and leaving them out reports
/// the directory as smaller than it is.
fn listing_total(listing: &Listing) -> Size {
    if let Some(denial) = listing.denied {
        return Size::Measured {
            bytes: 0,
            denials: one(denial),
        };
    }

    let mut bytes = listing.loose_bytes;
    let mut denials = Denials::default();
    let mut counting = false;
    let mut unknown = false;
    for entry in &listing.entries {
        bytes += entry.size.bytes();
        denials.merge(entry.size.denials());
        match entry.size {
            Size::Counting(_) => counting = true,
            Size::Unknown(_) => unknown = true,
            Size::Measured { .. } => {}
        }
    }

    if counting {
        Size::Counting(bytes)
    } else if unknown {
        Size::Unknown(bytes)
    } else {
        Size::Measured { bytes, denials }
    }
}

fn listing_measured(listing: &Listing) -> bool {
    listing
        .entries
        .iter()
        .all(|e| matches!(e.size, Size::Measured { .. }))
}

fn one(denial: Denial) -> Denials {
    Denials::one(denial)
}

struct TopRow {
    marked: bool,
    drillable: bool,
    name: String,
    note: String,
    size: String,
}

fn top_rows(targets: &[Target], order: &[usize], marked: &HashSet<usize>) -> Vec<TopRow> {
    order
        .iter()
        .filter_map(|&id| targets.get(id).map(|t| (id, t)))
        .map(|(id, t)| TopRow {
            marked: marked.contains(&id),
            drillable: matches!(
                t.action(),
                CleanAction::RemoveContents(_) | CleanAction::RemoveDir(_)
            ),
            name: t.name.clone(),
            note: note(t.size(), &t.description),
            size: size_label(t.size()),
        })
        .collect()
}

struct DirRow {
    marked: bool,
    is_dir: bool,
    name: String,
    size: String,
}

fn dir_rows(listing: &Listing, marked: &HashMap<PathBuf, MarkedPath>) -> Vec<DirRow> {
    listing
        .order
        .iter()
        .filter_map(|&i| listing.entries.get(i))
        .map(|e| DirRow {
            marked: marked.contains_key(&e.path),
            is_dir: e.is_dir,
            name: e.name.clone(),
            size: size_label(e.size),
        })
        .collect()
}

/// What the body of a directory view has to say. A refusal is not an empty
/// directory, and it is not somewhere to offer a delete either: nothing here
/// could even enumerate what would be removed.
enum DirBody {
    Rows,
    Empty,
    Denied(Denial),
}

fn dir_body(listing: &Listing) -> DirBody {
    match listing.denied {
        Some(denial) => DirBody::Denied(denial),
        None if listing.entries.is_empty() => DirBody::Empty,
        None => DirBody::Rows,
    }
}

fn clone_action(action: &CleanAction) -> CleanAction {
    match action {
        CleanAction::RemoveContents(p) => CleanAction::RemoveContents(p.clone()),
        CleanAction::RemoveDir(p) => CleanAction::RemoveDir(p.clone()),
        CleanAction::RemoveFile(p) => CleanAction::RemoveFile(p.clone()),
        CleanAction::RunCommand(c, a) => CleanAction::RunCommand(c.clone(), a.clone()),
        CleanAction::RemoveByExtension(d, e) => {
            CleanAction::RemoveByExtension(d.clone(), e.clone())
        }
        CleanAction::RemoveIdleChildren { dir, idle_days } => CleanAction::RemoveIdleChildren {
            dir: dir.clone(),
            idle_days: *idle_days,
        },
    }
}

pub fn run() -> Result<()> {
    let mut app = App::new(discover_all_targets());
    super::run(|terminal| {
        measure_top(&mut app, terminal)?;
        event_loop(&mut app, terminal)
    })
}

/// Draw the list, then measure it. Both walks report on this thread, so the
/// screen keeps repainting and keys keep working from inside them.
fn measure_top(app: &mut App, terminal: &mut Term) -> Result<()> {
    app.measuring = true;
    terminal.draw(|f| render(f, app))?;

    let mut going = true;
    for group in measurement_groups(&app.targets) {
        if !going {
            break;
        }
        going = sweep_group(app, terminal, &group)?;
    }
    for id in idle_targets(&app.targets) {
        if !going {
            break;
        }
        going = measure_idle(app, terminal, id)?;
    }

    app.measuring = false;
    app.give_up();
    app.resort_top();
    terminal.draw(|f| render(f, app))?;
    Ok(())
}

fn idle_targets(targets: &[Target]) -> Vec<usize> {
    targets
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t.measure(), Measure::IdleChildren { .. }))
        .map(|(i, _)| i)
        .collect()
}

/// Measure one group of non-overlapping targets. `false` means the user stopped.
fn sweep_group(app: &mut App, terminal: &mut Term, group: &[(usize, Root)]) -> Result<bool> {
    let roots: Vec<Root> = group.iter().map(|(_, root)| root.clone()).collect();
    let mut throttle = Throttle::new(live::REDRAW);
    let mut failure: Option<anyhow::Error> = None;

    let swept = sweep::sweep_streaming(&roots, &[], &mut |progress| {
        match progress {
            Progress::Started { .. } | Progress::Tick => {}
            Progress::Scanned {
                root, root_bytes, ..
            } => {
                if let Some((id, _)) = group.get(root)
                    && let Some(target) = app.targets.get_mut(*id)
                {
                    target.set_size(Size::Counting(root_bytes));
                }
            }
            Progress::RootDone {
                root,
                denials,
                bytes,
            } => {
                if let Some((id, _)) = group.get(root)
                    && let Some(target) = app.targets.get_mut(*id)
                {
                    target.set_size(Size::Measured { bytes, denials });
                    app.resort_top();
                }
            }
        }

        let flow = match measuring_keys(app, &mut throttle) {
            Ok(flow) => flow,
            Err(e) => {
                failure = Some(e.into());
                Flow::Cancel
            }
        };

        if throttle.ready()
            && let Err(e) = terminal.draw(|f| render(f, app))
        {
            failure = Some(e.into());
            return Flow::Cancel;
        }
        flow
    });

    if let Some(e) = failure {
        return Err(e);
    }

    // The finished sweep is the authority: its per-root totals include the
    // loose files, whatever the streaming events managed to say.
    if let Some(swept) = &swept {
        for ((id, _), usage) in group.iter().zip(&swept.roots) {
            if let Some(target) = app.targets.get_mut(*id) {
                target.set_size(Size::Measured {
                    bytes: usage.total,
                    denials: usage.denials,
                });
            }
        }
        app.resort_top();
    }
    Ok(swept.is_some())
}

/// The idle-children target: one walk per child, on a worker, so neither the
/// liveness snapshot nor a big scratch tree can freeze the screen.
fn measure_idle(app: &mut App, terminal: &mut Term, id: usize) -> Result<bool> {
    let Some(Measure::IdleChildren { dir, idle_days }) = app.targets.get(id).map(|t| t.measure())
    else {
        return Ok(true);
    };
    let (dir, idle_days) = (dir.clone(), *idle_days);

    // Off the drawing thread because the liveness snapshot alone shells out to
    // lsof, which takes seconds on a busy machine.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let busy = disk::liveness::Liveness::snapshot();
        let mut bytes = 0u64;
        for child in idle_candidates(&dir) {
            bytes += idle_child(&child, idle_days, &busy).unwrap_or(0);
            if tx.send(bytes).is_err() {
                return;
            }
        }
    });

    let mut throttle = Throttle::new(live::REDRAW);
    let mut bytes = 0u64;
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(counted) => {
                bytes = counted;
                if let Some(target) = app.targets.get_mut(id) {
                    target.set_size(Size::Counting(bytes));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(target) = app.targets.get_mut(id) {
                    target.set_size(Size::known(bytes));
                }
                app.resort_top();
                break;
            }
        }

        let flow = measuring_keys(app, &mut throttle)?;
        if throttle.ready() {
            terminal.draw(|f| render(f, app))?;
        }
        if flow == Flow::Cancel {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Keys that are safe while a walk is running. Moving and marking are; drilling
/// and cleaning would start a second walk or a delete on top of this one.
///
/// A key that changed something forces the next frame: waiting out the redraw
/// interval is what makes a cursor feel stuck.
fn measuring_keys(app: &mut App, throttle: &mut Throttle) -> io::Result<Flow> {
    while event::poll(Duration::ZERO)? {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => {
                app.quit = true;
                return Ok(Flow::Cancel);
            }
            KeyCode::Esc => return Ok(Flow::Cancel),
            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
            KeyCode::Char(' ') => app.toggle_mark(),
            _ => continue,
        }
        throttle.force();
    }
    Ok(Flow::Continue)
}

fn event_loop(app: &mut App, terminal: &mut Term) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| render(f, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.mode {
                Mode::Browse => match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Char(' ') => app.toggle_mark(),
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                        drill_in(app, terminal)?;
                    }
                    KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') if app.pop_or_quit() => {
                        return Ok(());
                    }
                    KeyCode::Char('x') | KeyCode::Char('D') if app.marked_count() > 0 => {
                        app.mode = Mode::Confirm;
                    }
                    _ => {}
                },
                Mode::Confirm => match key.code {
                    KeyCode::Char('y') => {
                        app.mode = Mode::Running;
                        run_ops(app, terminal)?;
                        app.mode = Mode::Done;
                    }
                    _ => {
                        app.mode = Mode::Browse;
                    }
                },
                Mode::Done => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.done_scroll = app.done_scroll.saturating_add(1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.done_scroll = app.done_scroll.saturating_sub(1);
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        app.done_scroll = app.done_scroll.saturating_add(10);
                    }
                    KeyCode::PageUp | KeyCode::Char('b') => {
                        app.done_scroll = app.done_scroll.saturating_sub(10);
                    }
                    KeyCode::Char('g') => {
                        app.done_scroll = 0;
                    }
                    KeyCode::Char('G') => {
                        app.done_scroll = u16::MAX;
                    }
                    _ => {}
                },
                Mode::Running => {}
            }
        }
    }
    Ok(())
}

fn drill_target(app: &App) -> Option<(PathBuf, String)> {
    match app.current() {
        View::Top { .. } => {
            let t = app.targets.get(app.selected_target()?)?;
            match t.action() {
                CleanAction::RemoveContents(p) | CleanAction::RemoveDir(p) => {
                    p.is_dir().then(|| (p.clone(), t.name.clone()))
                }
                _ => None,
            }
        }
        View::Directory { .. } => {
            let entry = app.selected_entry()?;
            entry
                .is_dir
                .then(|| (entry.path.clone(), entry.name.clone()))
        }
    }
}

fn drill_in(app: &mut App, terminal: &mut Term) -> Result<()> {
    let Some((path, crumb)) = drill_target(app) else {
        return Ok(());
    };

    // A listing left half measured by an Esc is worth walking again; a finished
    // one never changes.
    let measured = app.listings.get(&path).is_some_and(listing_measured);
    app.stack.push(View::Directory {
        path: path.clone(),
        cursor: 0,
        picked: false,
        crumb,
    });
    if !measured {
        measure_dir(app, terminal, &path)?;
    }
    Ok(())
}

/// List a directory at once, then measure its children. The listing is in place
/// before the first walk starts, so the view the user asked for is what fills
/// in, and Esc leaves with whatever has been counted.
fn measure_dir(app: &mut App, terminal: &mut Term, dir: &Path) -> Result<()> {
    let children = live::children_as_roots(dir);
    let roots = children.roots.clone();
    // Directories occupy the first `roots.len()` entries in the same order, so
    // a root index from the sweep is an entry index here.
    let mut entries: Vec<Entry> = roots
        .iter()
        .map(|root| Entry {
            name: root.name.clone(),
            path: root.path.clone(),
            is_dir: true,
            size: Size::Counting(0),
        })
        .collect();
    entries.extend(loose_entries(dir));
    app.listings.insert(
        dir.to_path_buf(),
        Listing {
            order: (0..entries.len()).collect(),
            entries,
            loose_bytes: children.loose_bytes,
            denied: children.denied,
        },
    );

    app.measuring = true;
    terminal.draw(|f| render(f, app))?;

    let mut throttle = Throttle::new(live::REDRAW);
    let mut failure: Option<anyhow::Error> = None;

    let swept = sweep::sweep_streaming(&roots, &[], &mut |progress| {
        match progress {
            Progress::Started { .. } | Progress::Tick => {}
            Progress::Scanned {
                root, root_bytes, ..
            } => {
                app.child_counting(dir, root, root_bytes);
            }
            Progress::RootDone {
                root,
                denials,
                bytes,
            } => {
                app.child_done(dir, root, Size::Measured { bytes, denials });
                app.resort_listing(dir);
            }
        }

        let flow = match measuring_keys(app, &mut throttle) {
            Ok(flow) => flow,
            Err(e) => {
                failure = Some(e.into());
                Flow::Cancel
            }
        };

        if throttle.ready()
            && let Err(e) = terminal.draw(|f| render(f, app))
        {
            failure = Some(e.into());
            return Flow::Cancel;
        }
        flow
    });

    app.measuring = false;
    if let Some(e) = failure {
        return Err(e);
    }

    match &swept {
        Some(swept) => {
            for (root, usage) in swept.roots.iter().enumerate() {
                app.child_done(
                    dir,
                    root,
                    Size::Measured {
                        bytes: usage.total,
                        denials: usage.denials,
                    },
                );
            }
        }
        None => app.give_up_listing(dir),
    }
    app.resort_listing(dir);

    // This walk counted every byte the row one level up stands for, loose files
    // included. A stopped walk counted some of them, which is no improvement on
    // whatever that row already says.
    if let Some(total @ Size::Measured { .. }) = app.listings.get(dir).map(listing_total) {
        set_parent_size(app, dir, total);
    }
    terminal.draw(|f| render(f, app))?;
    Ok(())
}

/// Non-directory entries, sized as they allocate. Hardlinks show their full
/// size in a row of their own; only [`Listing::loose_bytes`] dedupes them,
/// because that is the number the directory is charged for.
fn loose_entries(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            (!meta.is_dir()).then(|| Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: e.path(),
                is_dir: false,
                size: Size::known(allocated(&meta, &mut HashSet::new())),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn set_parent_size(app: &mut App, dir: &Path, total: Size) {
    let Some(parent) = app.stack.iter().rev().nth(1) else {
        return;
    };
    match parent {
        View::Top { cursor, .. } => {
            let Some(&id) = app.order.get(*cursor) else {
                return;
            };
            if let Some(target) = app.targets.get_mut(id) {
                target.set_size(total);
            }
            app.resort_top();
        }
        View::Directory { path, .. } => {
            let path = path.clone();
            let index = app
                .listings
                .get(&path)
                .and_then(|l| l.entries.iter().position(|e| e.path == dir));
            if let Some(index) = index {
                app.child_done(&path, index, total);
                app.resort_listing(&path);
            }
        }
    }
}

fn run_ops(app: &mut App, terminal: &mut Term) -> Result<()> {
    let ops = app.pending_ops();
    for op in ops {
        terminal.draw(|f| render(f, app))?;
        let known = !matches!(op.action, CleanAction::RunCommand(_, _));
        let target = Target::new(&op.label, "", op.size.bytes(), op.action);
        let outcome = target.clean();
        app.results.push(CleanResult {
            label: op.label,
            outcome,
            known,
        });
    }
    Ok(())
}

fn render(f: &mut Frame, app: &App) {
    match app.mode {
        Mode::Browse | Mode::Confirm => render_browse(f, app),
        Mode::Running => render_progress(f, app),
        Mode::Done => render_done(f, app),
    }
}

fn render_browse(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "  wsctl disk cleanup",
            Style::default().fg(Color::Cyan).bold(),
        ),
        Span::raw("  "),
        Span::styled(app.breadcrumb(), Style::default().fg(Color::White)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    match app.current() {
        View::Top { cursor, .. } => render_top(f, chunks[1], app, *cursor),
        View::Directory { cursor, .. } => render_directory(f, chunks[1], app, *cursor),
    }

    render_footer(f, chunks[2], app);

    if app.mode == Mode::Confirm {
        render_confirm(f, area, app);
    }
}

/// A cursor past the bottom of the viewport has to bring the viewport with it.
/// The offset is derived from the cursor every frame rather than kept in `App`,
/// which is the only place that knows how tall the body is: ratatui works it out
/// from the area it is handed.
fn scrolled(cursor: usize) -> TableState {
    TableState::default().with_selected(Some(cursor))
}

fn body_block() -> Block<'static> {
    Block::default()
        .borders(Borders::NONE)
        .padding(Padding::horizontal(1))
}

/// The cursor is a DarkGray background, so a DarkGray cell on that row is text
/// drawn in the colour behind it: a selected row was losing its description.
/// Secondary text lifts to White under the cursor and is left alone everywhere
/// else, which keeps the cursor a background and nothing else.
fn readable(color: Color, cursor: bool) -> Color {
    match (cursor, color) {
        (true, Color::DarkGray) => Color::White,
        _ => color,
    }
}

fn render_top(f: &mut Frame, body: Rect, app: &App, cursor: usize) {
    let rows: Vec<Row> = top_rows(&app.targets, &app.order, &app.marked_targets)
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let on_cursor = i == cursor;
            let check = if r.marked { " ✓" } else { " ·" };
            let check_style = if r.marked {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(readable(Color::DarkGray, on_cursor))
            };
            let row_style = if on_cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(if r.drillable { "📁" } else { "  " }),
                Cell::from(r.name).style(Style::default().fg(Color::White)),
                Cell::from(r.note).style(Style::default().fg(readable(Color::DarkGray, on_cursor))),
                Cell::from(r.size).style(Style::default().fg(Color::Yellow)),
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Percentage(35),
            Constraint::Percentage(45),
            Constraint::Length(10),
        ],
    )
    .block(body_block());
    f.render_stateful_widget(table, body, &mut scrolled(cursor));
}

fn render_directory(f: &mut Frame, body: Rect, app: &App, cursor: usize) {
    let Some(listing) = app.listing() else {
        let p = Paragraph::new("  Scanning...")
            .style(Style::default().fg(Color::Yellow))
            .block(body_block());
        f.render_widget(p, body);
        return;
    };

    match dir_body(listing) {
        DirBody::Empty => {
            let p = Paragraph::new("  (empty)")
                .style(Style::default().fg(Color::DarkGray))
                .block(body_block());
            f.render_widget(p, body);
            return;
        }
        DirBody::Denied(denial) => {
            let p = Paragraph::new(vec![
                Line::from(Span::styled(
                    "  unknown: this directory would not open",
                    Style::default().fg(Color::Red),
                )),
                Line::from(Span::styled(
                    format!("  {}", denial_advice(denial)),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .block(body_block());
            f.render_widget(p, body);
            return;
        }
        DirBody::Rows => {}
    }

    let rows: Vec<Row> = dir_rows(listing, &app.marked_paths)
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let on_cursor = i == cursor;
            let check = if r.marked { " ✓" } else { " ·" };
            let check_style = if r.marked {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(readable(Color::DarkGray, on_cursor))
            };
            let name_color = if r.is_dir { Color::Cyan } else { Color::White };
            let row_style = if on_cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(if r.is_dir { "📁" } else { "  " }),
                Cell::from(r.name).style(Style::default().fg(readable(name_color, on_cursor))),
                Cell::from(r.size).style(Style::default().fg(Color::Yellow)),
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(10),
        ],
    )
    .block(body_block());
    f.render_stateful_widget(table, body, &mut scrolled(cursor));
}

fn render_footer(f: &mut Frame, footer: Rect, app: &App) {
    let keys = if app.measuring {
        MEASURE_KEYS
    } else if app.marked_count() > 0 {
        CLEAN_KEYS
    } else {
        BROWSE_KEYS
    };

    let mut spans = Vec::new();
    if app.marked_count() > 0 {
        let (bytes, complete) = marked_total(&app.marked_sizes());
        spans.push(Span::styled(
            format!(
                " Marked: {} ({}) ",
                app.marked_count(),
                total_label(bytes, complete)
            ),
            Style::default().fg(Color::Green).bold(),
        ));
    } else if app.measuring {
        spans.push(Span::styled(
            format!(" Measuring: {} left ", app.rows_measuring()),
            Style::default().fg(Color::Yellow).bold(),
        ));
    }
    spans.push(Span::styled(keys, Style::default().fg(Color::DarkGray)));

    f.render_widget(Line::from(spans), footer);
}

fn render_confirm(f: &mut Frame, area: Rect, app: &App) {
    let ops = app.pending_ops();
    let sizes: Vec<Size> = ops.iter().map(|o| o.size).collect();
    let (bytes, complete) = marked_total(&sizes);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  Clean {} {} ({})?",
                ops.len(),
                if ops.len() == 1 { "item" } else { "items" },
                total_label(bytes, complete)
            ),
            Style::default().fg(Color::Yellow).bold(),
        )),
        Line::from(""),
    ];

    for op in ops.iter().take(15) {
        let action_label = match &op.action {
            CleanAction::RemoveContents(_) => "empty",
            CleanAction::RemoveDir(_) => "delete",
            CleanAction::RemoveFile(_) => "delete file",
            CleanAction::RunCommand(_, _) => "run",
            CleanAction::RemoveByExtension(_, _) => "delete by ext",
            CleanAction::RemoveIdleChildren { .. } => "delete idle",
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{action_label}] "),
                Style::default().fg(Color::Red),
            ),
            Span::styled(op.label.as_str(), Style::default().fg(Color::White)),
            Span::styled(
                format!("  {}", size_label(op.size)),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    if ops.len() > 15 {
        lines.push(Line::from(Span::styled(
            format!("  ... and {} more", ops.len() - 15),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" y ", Style::default().fg(Color::Green).bold()),
        Span::raw("Proceed   "),
        Span::styled(" n ", Style::default().fg(Color::Red).bold()),
        Span::raw("Cancel"),
    ]));

    let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup_area = centered_rect(70, height, area);
    f.render_widget(Clear, popup_area);

    let popup = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm ")
                .border_style(Style::default().fg(Color::Yellow)),
        );
    f.render_widget(popup, popup_area);
}

fn render_progress(f: &mut Frame, app: &App) {
    let area = f.area();
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Cleaning...",
            Style::default().fg(Color::Yellow).bold(),
        )),
        Line::from(""),
    ];
    for r in &app.results {
        lines.extend(result_lines(r));
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Progress ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(paragraph, area);
}

fn render_done(f: &mut Frame, app: &App) {
    let area = f.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" Results ")
        .border_style(Style::default().fg(Color::Green));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(inner);

    let header = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Cleanup Complete",
            Style::default().fg(Color::Green).bold(),
        )),
    ]);
    f.render_widget(header, chunks[0]);

    let mut result_body: Vec<Line<'_>> = Vec::new();
    for r in &app.results {
        result_body.extend(result_lines(r));
    }
    let results = Paragraph::new(result_body)
        .wrap(Wrap { trim: false })
        .scroll((app.done_scroll, 0));
    f.render_widget(results, chunks[1]);

    let footer = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Total freed: ", Style::default().bold()),
            Span::styled(
                {
                    let (bytes, complete) = app.total_freed();
                    total_label(bytes, complete)
                },
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                "    [j/k] Scroll  [g/G] Top/Bottom  [q] Quit",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ]);
    f.render_widget(footer, chunks[2]);
}

fn result_lines(r: &CleanResult) -> Vec<Line<'_>> {
    match &r.outcome {
        Ok(bytes) => vec![Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(Color::Green)),
            Span::styled(r.label.as_str(), Style::default().fg(Color::White)),
            Span::styled(
                format!("  {}", freed_label(*bytes, r.known)),
                Style::default().fg(Color::Green),
            ),
        ])],
        Err(e) => vec![
            Line::from(vec![
                Span::styled("  ✗ ", Style::default().fg(Color::Red)),
                Span::styled(r.label.as_str(), Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::raw("      "),
                Span::styled(e.as_str(), Style::default().fg(Color::Red)),
            ]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::fs;

    /// The binary crate has no dev-dependencies, so scratch trees are built by
    /// hand rather than with `tempfile`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("wsctl-cleanup-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The size a row ends up with from the event stream alone, which is what
    /// the screen draws while a walk is running.
    fn streamed_size(path: &Path) -> Size {
        let root = Root {
            name: "root".to_string(),
            path: path.to_path_buf(),
        };
        let mut size = Size::Counting(0);
        sweep::sweep_streaming(&[root], &[], &mut |progress| {
            match progress {
                Progress::Scanned { root_bytes, .. } => size = Size::Counting(root_bytes),
                Progress::RootDone { denials, bytes, .. } => {
                    size = Size::Measured { bytes, denials };
                }
                _ => {}
            }
            Flow::Continue
        });
        size
    }

    fn target(name: &str, size: Size) -> Target {
        let mut t = Target::pending(
            name,
            "a description",
            Measure::Nothing,
            CleanAction::RemoveContents(PathBuf::from("/tmp").join(name)),
        );
        t.set_size(size);
        t
    }

    fn entry(name: &str, is_dir: bool, size: Size) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            is_dir,
            size,
        }
    }

    fn listing(entries: Vec<Entry>, loose_bytes: u64, denied: Option<Denial>) -> Listing {
        Listing {
            order: (0..entries.len()).collect(),
            entries,
            loose_bytes,
            denied,
        }
    }

    fn drawn(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().to_string()
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

    fn cursor(app: &App) -> usize {
        match app.current() {
            View::Top { cursor, .. } | View::Directory { cursor, .. } => *cursor,
        }
    }

    fn cursor_on(app: &mut App, row: usize) {
        while cursor(app) > row {
            app.move_up();
        }
        while cursor(app) < row {
            app.move_down();
        }
    }

    fn protected(n: usize) -> Denials {
        Denials {
            protected: n,
            ..Denials::default()
        }
    }

    fn forbidden(n: usize) -> Denials {
        Denials {
            forbidden: n,
            ..Denials::default()
        }
    }

    #[test]
    fn a_row_nobody_has_measured_yet_is_not_zero_bytes() {
        // The whole point of the exercise: an unmeasured row used to render as
        // `0 B`, which is a measurement nobody took.
        assert_eq!(size_label(Size::Counting(0)), "scanning");
        assert_ne!(size_label(Size::Counting(0)), format_size(0));
    }

    #[test]
    fn a_running_total_is_not_shown_as_a_size() {
        // A row mid-walk has counted something, and that something is a floor.
        // Drawing 4 MB for a directory that turns out to hold 184 GB is the
        // same class of lie as drawing 0 B for one nobody opened.
        let partial = Size::Counting(4 * 1024 * 1024);
        assert_eq!(size_label(partial), "scanning");
        assert_ne!(size_label(partial), format_size(4 * 1024 * 1024));
        assert_eq!(partial.total(), None, "a floor is not a total");
    }

    #[test]
    fn a_row_only_becomes_a_number_once_its_walk_is_done() {
        let done = Size::Measured {
            bytes: 8 * 1024,
            denials: Denials::default(),
        };
        assert_eq!(size_label(done), "8 KB");
        assert_eq!(done.total(), Some(8 * 1024));
    }

    #[test]
    fn a_measured_empty_row_is_allowed_to_say_zero() {
        // Zero is only honest once something has actually looked.
        assert_eq!(size_label(Size::known(0)), "0 B");
    }

    #[test]
    fn a_stopped_walk_leaves_the_row_unknown_not_partial() {
        assert_eq!(size_label(Size::Unknown(0)), "unknown");
        assert_eq!(size_label(Size::Unknown(64 * 1024)), "unknown");
    }

    #[test]
    fn a_refused_directory_is_unknown_not_empty() {
        let refused = Size::Measured {
            bytes: 0,
            denials: protected(1),
        };
        assert_eq!(size_label(refused), "unknown");
        assert_ne!(size_label(refused), format_size(0));
        assert_eq!(refused.total(), None);
    }

    #[test]
    fn the_two_refusals_never_merge_into_one_message() {
        // Full Disk Access does nothing for a root-owned directory, and sudo
        // does nothing for a TCC-protected one. Telling a user the wrong one
        // sends them off to try something that cannot work.
        let tcc = note(
            Size::Measured {
                bytes: 0,
                denials: protected(3),
            },
            "a description",
        );
        let unix = note(
            Size::Measured {
                bytes: 0,
                denials: forbidden(2),
            },
            "a description",
        );

        assert!(tcc.contains("Full Disk Access"), "{tcc}");
        assert!(!tcc.contains("sudo"), "{tcc}");
        assert!(unix.contains("sudo"), "{unix}");
        assert!(!unix.contains("Full Disk Access"), "{unix}");
        assert_ne!(tcc, unix);

        let both = note(
            Size::Measured {
                bytes: 0,
                denials: Denials {
                    protected: 1,
                    forbidden: 2,
                    ..Denials::default()
                },
            },
            "a description",
        );
        assert!(
            both.contains("Full Disk Access") && both.contains("sudo"),
            "{both}"
        );
    }

    #[test]
    fn a_row_with_nothing_to_report_keeps_its_description() {
        assert_eq!(
            note(Size::known(4096), "Compiled build artifacts"),
            "Compiled build artifacts"
        );
        assert_eq!(
            note(Size::Counting(0), "Compiled build artifacts"),
            "Compiled build artifacts"
        );
    }

    #[test]
    fn advice_for_a_refusal_names_the_fix_that_works() {
        let tcc = denial_advice(Denial::Protected);
        let unix = denial_advice(Denial::Forbidden);
        let closed = denial_advice(Denial::Unattended);
        assert!(tcc.contains("Full Disk Access"), "{tcc}");
        assert!(unix.contains("sudo"), "{unix}");
        assert!(closed.contains("closed"), "{closed}");
        assert!(!closed.contains("Full Disk Access"), "{closed}");
        assert_ne!(tcc, unix);
        assert_ne!(closed, tcc);
    }

    #[test]
    fn the_marked_total_counts_only_finished_rows_and_says_so() {
        // The choice this documents: an unfinished row is NOT refused a mark,
        // it contributes the floor it counted and costs the total its
        // certainty. Silently dropping it would understate a delete, and
        // silently adding it would overstate what is known.
        let mixed = [
            Size::known(8 * 1024),
            Size::Counting(4 * 1024),
            Size::Unknown(0),
        ];
        let (bytes, complete) = marked_total(&mixed);

        assert_eq!(
            bytes,
            12 * 1024,
            "every floor still counts toward the floor"
        );
        assert!(!complete);
        assert_eq!(total_label(bytes, complete), "at least 12 KB");
    }

    #[test]
    fn a_total_of_only_finished_rows_is_stated_plainly() {
        let (bytes, complete) = marked_total(&[Size::known(8 * 1024), Size::known(4 * 1024)]);
        assert!(complete);
        assert_eq!(total_label(bytes, complete), "12 KB");
    }

    #[test]
    fn a_total_that_knows_nothing_does_not_claim_zero() {
        let (bytes, complete) = marked_total(&[Size::Unknown(0), Size::Counting(0)]);
        assert_eq!(total_label(bytes, complete), "size unknown");
        assert_ne!(total_label(bytes, complete), format_size(0));
    }

    #[test]
    fn a_refused_row_costs_the_total_its_certainty() {
        let (bytes, complete) = marked_total(&[Size::Measured {
            bytes: 512 * 1024,
            denials: forbidden(1),
        }]);
        assert_eq!(bytes, 512 * 1024);
        assert!(
            !complete,
            "a walk that was refused part of the tree is a floor"
        );
        assert_eq!(total_label(bytes, complete), "at least 524 KB");
    }

    #[test]
    fn a_resort_keeps_the_cursor_on_the_row_not_on_the_line() {
        let mut targets = vec![
            target("alpha", Size::Counting(0)),
            target("bravo", Size::Counting(0)),
            target("charlie", Size::Counting(0)),
        ];
        let mut order = size_order(&targets.iter().map(|t| t.size()).collect::<Vec<_>>());
        let mut cursor = 1;
        let watching = top_rows(&targets, &order, &HashSet::new())[cursor]
            .name
            .clone();
        assert_eq!(watching, "bravo");

        // charlie finishes first and jumps to the top.
        targets[2].set_size(Size::known(64 * 1024));
        let after = size_order(&targets.iter().map(|t| t.size()).collect::<Vec<_>>());
        cursor = keep_cursor(&order, &after, cursor, true);
        order = after;

        assert_eq!(cursor, 2, "the row moved down, so the cursor did too");
        assert_eq!(
            top_rows(&targets, &order, &HashSet::new())[cursor].name,
            watching
        );
    }

    #[test]
    fn an_untouched_cursor_stays_at_the_top_while_the_list_sorts() {
        // Row zero is where a cursor starts, not a row anybody chose. Following
        // it would walk the highlight down the list as the big rows arrive
        // above it, away from everything worth reading.
        let before = vec![0, 1, 2];
        let after = vec![2, 0, 1];

        assert_eq!(keep_cursor(&before, &after, 0, false), 0);
        assert_eq!(keep_cursor(&before, &after, 0, true), 1);
    }

    #[test]
    fn a_cursor_past_the_end_of_a_shorter_list_comes_back_into_range() {
        assert_eq!(keep_cursor(&[0, 1, 2], &[0], 2, false), 0);
        assert_eq!(keep_cursor(&[], &[], 3, true), 0);
    }

    #[test]
    fn a_mark_survives_the_resort_it_was_made_before() {
        // Marks name a target, not a line: the row they were made on moves.
        let mut targets = vec![
            target("alpha", Size::Counting(0)),
            target("bravo", Size::Counting(0)),
        ];
        let marked = HashSet::from([1]);

        let before = size_order(&targets.iter().map(|t| t.size()).collect::<Vec<_>>());
        assert!(top_rows(&targets, &before, &marked)[1].marked);

        targets[1].set_size(Size::known(64 * 1024));
        let after = size_order(&targets.iter().map(|t| t.size()).collect::<Vec<_>>());
        let rows = top_rows(&targets, &after, &marked);

        assert_eq!(rows[0].name, "bravo");
        assert!(rows[0].marked, "the mark followed the row up the list");
        assert!(!rows[1].marked);
    }

    #[test]
    fn equal_sizes_keep_the_order_they_were_discovered_in() {
        let sizes = [Size::Counting(0), Size::Counting(0), Size::Counting(0)];
        assert_eq!(size_order(&sizes), vec![0, 1, 2]);
    }

    #[test]
    fn loose_files_are_listed_and_counted() {
        // A directory of nothing but files has no child root to report it, so
        // ignoring the loose bytes reports the directory as empty.
        let scratch = Scratch::new("loose");
        fs::write(scratch.path().join("a.bin"), vec![0u8; 8 * 1024]).unwrap();
        fs::write(scratch.path().join("b.bin"), vec![0u8; 16 * 1024]).unwrap();
        fs::create_dir(scratch.path().join("sub")).unwrap();

        let files = loose_entries(scratch.path());
        let names: Vec<&str> = files.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a.bin", "b.bin"],
            "subdirectories are not files"
        );
        assert!(files.iter().all(|e| !e.is_dir));
        assert!(
            files.iter().all(|e| e.size.total().is_some()),
            "a file needs no walk"
        );

        let children = live::children_as_roots(scratch.path());
        let listed = listing(
            vec![entry("sub", true, Size::known(4 * 1024))],
            children.loose_bytes,
            None,
        );

        let total = listing_total(&listed).total().expect("everything measured");
        assert!(
            total >= 24 * 1024 + 4 * 1024,
            "the loose files were left out of the total: {total}"
        );
    }

    #[test]
    fn a_directory_still_measuring_has_no_total_to_give() {
        let listed = listing(
            vec![
                entry("done", true, Size::known(8 * 1024)),
                entry("counting", true, Size::Counting(4 * 1024)),
            ],
            0,
            None,
        );
        assert_eq!(listing_total(&listed).total(), None);
        assert_eq!(size_label(listing_total(&listed)), "scanning");
    }

    #[test]
    fn a_readable_but_empty_directory_still_reads_as_empty() {
        let listed = listing(Vec::new(), 0, None);
        assert!(matches!(dir_body(&listed), DirBody::Empty));
    }

    #[test]
    fn a_directory_that_would_not_open_is_never_drawn_as_empty() {
        // One level deeper, the same lie: `(empty)` over bytes the scan was
        // never allowed to see. And there is nothing here to offer a delete on,
        // since nothing could enumerate what would go.
        for denial in [Denial::Protected, Denial::Forbidden, Denial::Unattended] {
            let listed = listing(Vec::new(), 0, Some(denial));

            match dir_body(&listed) {
                DirBody::Denied(shown) => assert_eq!(shown, denial),
                _ => panic!("a refusal was drawn as an ordinary directory"),
            }
            assert!(dir_rows(&listed, &HashMap::new()).is_empty());
            assert_eq!(size_label(listing_total(&listed)), "unknown");
            assert_ne!(size_label(listing_total(&listed)), format_size(0));
        }
    }

    #[test]
    fn a_refused_directory_keeps_which_refusal_it_was() {
        let tcc = listing(Vec::new(), 0, Some(Denial::Protected));
        let unix = listing(Vec::new(), 0, Some(Denial::Forbidden));

        assert_eq!(listing_total(&tcc).denials(), protected(1));
        assert_eq!(listing_total(&unix).denials(), forbidden(1));
        assert_ne!(
            note(listing_total(&tcc), ""),
            note(listing_total(&unix), "")
        );
    }

    #[test]
    fn a_files_row_shows_its_size_while_its_siblings_are_still_counting() {
        let listed = listing(
            vec![
                entry("subdir", true, Size::Counting(0)),
                entry("file.bin", false, Size::known(8 * 1024)),
            ],
            8 * 1024,
            None,
        );
        let rows = dir_rows(&listed, &HashMap::new());

        assert_eq!(rows[0].size, "scanning");
        assert!(rows[0].is_dir);
        assert_eq!(rows[1].size, "8 KB");
        assert!(!rows[1].is_dir);
    }

    #[test]
    fn a_leaf_of_nothing_but_files_reports_the_files() {
        // The walk hands out one job per subdirectory, so a directory with none
        // is finished without a single Scanned event. Its bytes ride on RootDone
        // instead; reading that as zero would print 0 B for a directory full of
        // files.
        let scratch = Scratch::new("leaf");
        fs::write(scratch.path().join("a.bin"), vec![0u8; 8 * 1024]).unwrap();
        fs::write(scratch.path().join("b.bin"), vec![0u8; 8 * 1024]).unwrap();

        let size = streamed_size(scratch.path());

        assert!(
            size.total().is_some_and(|bytes| bytes >= 16 * 1024),
            "loose files went missing: {}",
            size_label(size)
        );
    }

    #[test]
    fn a_leaf_that_would_not_open_is_unknown_even_when_the_walk_said_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("leaf-locked");
        let locked = scratch.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.bin"), vec![0u8; 64 * 1024]).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let size = streamed_size(&locked);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));

        assert_eq!(size_label(size), "unknown");
        assert_ne!(size_label(size), format_size(0));
        assert_eq!(size.denials(), forbidden(1));
    }

    #[test]
    fn what_a_command_freed_is_not_reported_as_the_size_we_measured() {
        // The cache was 900 MB before `brew cleanup` ran. How much of it brew
        // decided to prune is not something anybody here measured.
        assert_eq!(freed_label(900_000_000, false), "unknown");
        assert_eq!(freed_label(900_000_000, true), "900 MB");
    }

    #[test]
    fn a_total_freed_with_a_command_in_it_is_a_floor() {
        let results = vec![
            CleanResult {
                label: "a directory".into(),
                outcome: Ok(8 * 1024),
                known: true,
            },
            CleanResult {
                label: "a command".into(),
                outcome: Ok(900 * 1024 * 1024),
                known: false,
            },
            CleanResult {
                label: "a failure".into(),
                outcome: Err("rm: denied".into()),
                known: true,
            },
        ];

        let (bytes, complete) = freed_total(&results);

        assert_eq!(bytes, 8 * 1024, "a command's own guess is not added in");
        assert_eq!(total_label(bytes, complete), "at least 8 KB");
    }

    #[test]
    fn only_a_directory_row_offers_a_drill() {
        let targets = vec![
            target("a directory", Size::known(0)),
            Target::unknown(
                "a command",
                "prunes something",
                CleanAction::RunCommand("pnpm".into(), vec!["store".into()]),
            ),
        ];
        let rows = top_rows(&targets, &[0, 1], &HashSet::new());

        assert!(rows[0].drillable);
        assert!(!rows[1].drillable, "there is no directory behind a command");
        assert_eq!(rows[1].size, "unknown", "and nothing can size it up front");
    }

    #[test]
    fn no_cell_on_the_cursor_row_is_drawn_in_the_colour_behind_it() {
        // The cursor row is a DarkGray background and the description cell is
        // DarkGray text, so a selected row was drawing its description in the
        // colour behind it. Every cell of every row is checked rather than the
        // one that was wrong, so a DarkGray cell added later fails here too.
        let mut app = App::new(vec![
            target("caches", Size::known(4 * 1024)),
            target("logs", Size::known(2 * 1024)),
        ]);
        app.toggle_mark();

        for row in 0..app.rows() {
            cursor_on(&mut app, row);
            let hidden = unreadable_cells(&app, 100, 12);
            assert!(hidden.is_empty(), "top row {row}: {hidden:?}");
        }
        assert!(drawn(&app, 100, 12).contains("a description"));

        let dir = PathBuf::from("/tmp/wsctl-cleanup-cursor");
        app.listings.insert(
            dir.clone(),
            listing(
                vec![
                    entry("sub", true, Size::known(8 * 1024)),
                    entry("f.bin", false, Size::Unknown(0)),
                ],
                0,
                None,
            ),
        );
        app.stack.push(View::Directory {
            path: dir,
            cursor: 0,
            picked: false,
            crumb: "caches".to_string(),
        });

        for row in 0..app.rows() {
            cursor_on(&mut app, row);
            let hidden = unreadable_cells(&app, 100, 12);
            assert!(hidden.is_empty(), "directory row {row}: {hidden:?}");
        }
    }
}
