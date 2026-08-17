//! A live view of where the volume's bytes went.
//!
//! The rows are known before a single byte is counted, so the list has its
//! final shape from the first frame and fills in rather than appearing all at
//! once. Their order is deliberately left alone while the sweep runs: sorting
//! on every update swaps rows under the cursor faster than anyone can read
//! them, so the list is sorted once, when the numbers are final.

use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use disk::audit::attribution_rules;
use disk::overview::disk_overview;
use disk::scan::{ScanResult, scan_dir};
use disk::sweep::{self, Denial, Flow, Progress, Sweep};
use disk::util::format_size;
use ratatui::{prelude::*, widgets::*};

/// A busy sweep reports far faster than a terminal can usefully repaint.
const REDRAW: Duration = Duration::from_millis(60);

const KEYS: &str = "  ↑↓ Move   Enter/→ Drill   Esc/← Back   r Rescan   o Finder   q Quit";

/// Nothing else is listened for until the walk is done, so nothing else is
/// offered.
const SCAN_KEYS: &str = "  Esc/q Stop";

struct App {
    rows: Vec<RootRow>,
    stack: Vec<View>,
    free: Option<u64>,
    jobs_left: usize,
    scanning: bool,
    quit: bool,
}

/// One line of the top view, from the moment the roots are announced.
struct RootRow {
    name: String,
    path: PathBuf,
    bytes: u64,
    done: bool,
    protected: usize,
    forbidden: usize,
}

enum View {
    Top {
        cursor: usize,
    },
    Directory {
        name: String,
        scan: ScanResult,
        cursor: usize,
    },
}

/// What a row can say about its own size. A refused directory is not empty,
/// and printing `0 B` for one is a claim the walk never made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measure {
    /// Still counting. The number so far is a lower bound.
    Partial(u64),
    Known(u64),
    Unknown,
}

struct Entry<'a> {
    name: &'a str,
    measure: Measure,
}

impl Measure {
    fn bytes(self) -> u64 {
        match self {
            Measure::Partial(bytes) | Measure::Known(bytes) => bytes,
            Measure::Unknown => 0,
        }
    }

    fn label(self) -> String {
        match self {
            Measure::Partial(0) => "scanning".to_string(),
            Measure::Partial(bytes) | Measure::Known(bytes) => format_size(bytes),
            Measure::Unknown => "unknown".to_string(),
        }
    }
}

impl RootRow {
    fn measure(&self) -> Measure {
        if !self.done {
            Measure::Partial(self.bytes)
        } else if self.bytes == 0 && self.protected + self.forbidden > 0 {
            Measure::Unknown
        } else {
            Measure::Known(self.bytes)
        }
    }
}

impl App {
    fn new(free: Option<u64>) -> Self {
        Self {
            rows: Vec::new(),
            stack: vec![View::Top { cursor: 0 }],
            free,
            jobs_left: 0,
            scanning: false,
            quit: false,
        }
    }

    fn current(&self) -> &View {
        self.stack.last().expect("stack never empty")
    }

    fn cursor(&self) -> usize {
        match self.current() {
            View::Top { cursor } | View::Directory { cursor, .. } => *cursor,
        }
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        match self.current() {
            View::Top { .. } => self
                .rows
                .iter()
                .map(|r| Entry {
                    name: &r.name,
                    measure: r.measure(),
                })
                .collect(),
            View::Directory { scan, .. } => scan
                .children
                .iter()
                .map(|c| Entry {
                    name: &c.name,
                    measure: Measure::Known(c.size),
                })
                .collect(),
        }
    }

    fn move_up(&mut self) {
        match self.stack.last_mut().expect("stack never empty") {
            View::Top { cursor } | View::Directory { cursor, .. } => {
                *cursor = cursor.saturating_sub(1);
            }
        }
    }

    fn move_down(&mut self) {
        let max = self.entries().len().saturating_sub(1);
        match self.stack.last_mut().expect("stack never empty") {
            View::Top { cursor } | View::Directory { cursor, .. } => {
                if *cursor < max {
                    *cursor += 1;
                }
            }
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        match self.current() {
            View::Top { cursor } => self.rows.get(*cursor).map(|r| r.path.clone()),
            View::Directory { scan, cursor, .. } => {
                scan.children.get(*cursor).map(|c| c.path.clone())
            }
        }
    }

    /// The selected row as something to walk into, or `None` when it is a file.
    fn drill_target(&self) -> Option<(PathBuf, String)> {
        match self.current() {
            View::Top { cursor } => self
                .rows
                .get(*cursor)
                .map(|r| (r.path.clone(), r.name.clone())),
            View::Directory { scan, cursor, .. } => scan
                .children
                .get(*cursor)
                .filter(|c| c.is_dir)
                .map(|c| (c.path.clone(), c.name.clone())),
        }
    }

    fn apply(&mut self, progress: Progress) {
        match progress {
            Progress::Started { roots, jobs } => {
                self.rows = roots
                    .into_iter()
                    .map(|root| RootRow {
                        name: root.name,
                        path: root.path,
                        bytes: 0,
                        done: false,
                        protected: 0,
                        forbidden: 0,
                    })
                    .collect();
                self.jobs_left = jobs;
                self.scanning = true;
                self.stack = vec![View::Top { cursor: 0 }];
            }
            Progress::Scanned {
                root,
                root_bytes,
                remaining,
                ..
            } => {
                if let Some(row) = self.rows.get_mut(root) {
                    row.bytes = root_bytes;
                }
                self.jobs_left = remaining;
            }
            Progress::RootDone { root } => {
                if let Some(row) = self.rows.get_mut(root) {
                    row.done = true;
                }
            }
            Progress::Tick => {}
        }
    }

    fn finish(&mut self, swept: Sweep) {
        self.rows = swept
            .roots
            .iter()
            .map(|usage| RootRow {
                name: usage.name.clone(),
                path: usage.path.clone(),
                bytes: usage.total,
                done: true,
                protected: denied(usage, Denial::Protected),
                forbidden: denied(usage, Denial::Forbidden),
            })
            .collect();
        self.rows.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        self.stop();
    }

    fn stop(&mut self) {
        self.scanning = false;
        self.jobs_left = 0;
    }

    fn roots_left(&self) -> usize {
        self.rows.iter().filter(|r| !r.done).count()
    }

    fn location(&self) -> String {
        let trail: Vec<&str> = self
            .stack
            .iter()
            .filter_map(|view| match view {
                View::Directory { name, .. } => Some(name.as_str()),
                View::Top { .. } => None,
            })
            .collect();
        if trail.is_empty() {
            "Select a location to explore:".to_string()
        } else {
            trail.join(" > ")
        }
    }
}

fn denied(usage: &sweep::RootUsage, denial: Denial) -> usize {
    usage
        .unreadable
        .iter()
        .filter(|u| u.denial == denial)
        .count()
}

fn bar_width(value: u64, max: u64, width: usize) -> usize {
    if max == 0 || width == 0 {
        return 0;
    }
    let filled = (value as f64 / max as f64 * width as f64).round() as usize;
    filled.min(width)
}

fn bar(value: u64, max: u64, width: usize) -> String {
    let filled = bar_width(value, max, width);
    if filled == 0 && value > 0 && width > 0 {
        return "▏".to_string();
    }
    "█".repeat(filled)
}

fn percent_text(value: u64, total: u64) -> String {
    if total == 0 || value == 0 {
        return "--".to_string();
    }
    format!("{:.1}%", value as f64 / total as f64 * 100.0)
}

/// Counted apart because the fixes are not interchangeable: Full Disk Access
/// lifts a `Protected` refusal and does nothing whatsoever for a `Forbidden`
/// one.
fn denial_counts(rows: &[RootRow]) -> (usize, usize) {
    rows.iter().fold((0, 0), |(protected, forbidden), row| {
        (protected + row.protected, forbidden + row.forbidden)
    })
}

fn progress_note(app: &App) -> String {
    if !app.scanning {
        return String::new();
    }
    let roots = app.roots_left();
    let noun = if roots == 1 {
        "directory"
    } else {
        "directories"
    };
    format!("  |  Scanning {roots} {noun}, {} left", app.jobs_left)
}

fn denial_note(rows: &[RootRow]) -> Option<String> {
    match denial_counts(rows) {
        (0, 0) => None,
        (protected, 0) => Some(format!(
            "{protected} directories unreadable; grant Full Disk Access to measure them"
        )),
        (0, forbidden) => Some(format!(
            "{forbidden} directories unreadable; they need sudo to measure"
        )),
        (protected, forbidden) => Some(format!(
            "{protected} unreadable without Full Disk Access, {forbidden} more need sudo"
        )),
    }
}

/// Narrow terminals lose the bar first: two cells of block are noise, whereas
/// the name and the size are the point.
fn bar_columns(width: u16) -> usize {
    if width < 56 {
        0
    } else {
        ((width as usize - 40) / 2).min(24)
    }
}

/// Browse the volume, live, from the first frame of the sweep.
pub fn run() -> Result<()> {
    let mut app = App::new(disk_overview().map(|o| o.free));
    super::run(|terminal| event_loop(&mut app, terminal))
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    sweep_live(app, terminal)?;

    while !app.quit {
        terminal.draw(|f| render(f, app))?;

        if !event::poll(Duration::from_millis(120))? {
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
            KeyCode::Up | KeyCode::Char('k') => app.move_up(),
            KeyCode::Down | KeyCode::Char('j') => app.move_down(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => drill_in(app, terminal)?,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                if app.stack.len() == 1 {
                    return Ok(());
                }
                app.stack.pop();
            }
            KeyCode::Char('r') => sweep_live(app, terminal)?,
            KeyCode::Char('o') => open_in_finder(app),
            _ => {}
        }
    }
    Ok(())
}

/// The sweep calls back on this thread, so drawing and key handling happen
/// inside the walk rather than alongside it. A `Tick` arrives every 80ms even
/// when nothing finishes, which is what keeps quitting responsive.
fn sweep_live(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let roots = sweep::discover_roots();
    let rules = attribution_rules();

    let mut failure: Option<anyhow::Error> = None;
    let mut painted: Option<Instant> = None;

    let swept = sweep::sweep_streaming(&roots, &rules, &mut |progress| {
        app.apply(progress);

        if painted.is_none_or(|at| at.elapsed() >= REDRAW) {
            painted = Some(Instant::now());
            if let Err(e) = terminal.draw(|f| render(f, app)) {
                failure = Some(e.into());
                return Flow::Cancel;
            }
        }

        match quit_pressed() {
            Ok(false) => Flow::Continue,
            Ok(true) => {
                app.quit = true;
                Flow::Cancel
            }
            Err(e) => {
                failure = Some(e.into());
                Flow::Cancel
            }
        }
    });

    if let Some(e) = failure {
        return Err(e);
    }
    match swept {
        Some(swept) => app.finish(swept),
        None => app.stop(),
    }
    Ok(())
}

fn quit_pressed() -> io::Result<bool> {
    while event::poll(Duration::ZERO)? {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn drill_in(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let Some((path, name)) = app.drill_target() else {
        return Ok(());
    };
    if !path.is_dir() {
        return Ok(());
    }

    // `scan_dir` sizes the whole subtree before it returns, which on a large
    // directory is long enough for a still frame to look like a hang.
    terminal.draw(|f| render_measuring(f, &name))?;
    let scan = scan_dir(&path);

    app.stack.push(View::Directory {
        name,
        scan,
        cursor: 0,
    });
    Ok(())
}

/// Reveal rather than open: a plain `open` on a file launches whatever app
/// claims it, which is not what a disk browser should do to a stray 4 GB blob.
fn open_in_finder(app: &App) {
    if let Some(path) = app.selected_path() {
        let _ = Command::new("open").arg("-R").arg(path).status();
    }
}

fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(f.area());

    render_header(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_footer(f, chunks[2], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let free = match app.free {
        Some(free) => format!("   ({} free)", format_size(free)),
        None => String::new(),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("  Analyze Disk", Style::default().fg(Color::Cyan).bold()),
            Span::styled(free, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {}", app.location()),
                Style::default().fg(Color::White),
            ),
            Span::styled(progress_note(app), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let entries = app.entries();
    if entries.is_empty() {
        let text = if app.scanning {
            "  Looking for roots..."
        } else {
            "  (empty)"
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let total: u64 = entries.iter().map(|e| e.measure.bytes()).sum();
    let largest = entries.iter().map(|e| e.measure.bytes()).max().unwrap_or(0);
    let width = bar_columns(area.width);
    let cursor = app.cursor();

    let rows: Vec<Row> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let bytes = entry.measure.bytes();
            let marker = if i == cursor { " ▶" } else { "  " };
            let row_style = if i == cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(marker).style(Style::default().fg(Color::Cyan)),
                Cell::from(Text::from(format!("{}.", i + 1)).right_aligned())
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(bar(bytes, largest, width)).style(Style::default().fg(Color::Blue)),
                Cell::from(Text::from(percent_text(bytes, total)).right_aligned())
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(entry.name).style(Style::default().fg(Color::White)),
                Cell::from(Text::from(entry.measure.label()).right_aligned())
                    .style(Style::default().fg(Color::Yellow)),
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Length(width as u16),
            Constraint::Length(6),
            Constraint::Min(8),
            Constraint::Length(9),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::NONE)
            .padding(Padding::horizontal(1)),
    );

    let mut state = TableState::default().with_selected(Some(cursor));
    f.render_stateful_widget(table, area, &mut state);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let note = denial_note(&app.rows)
        .map(|note| format!("  {note}"))
        .unwrap_or_default();
    let keys = if app.scanning { SCAN_KEYS } else { KEYS };
    let lines = vec![
        Line::from(Span::styled(note, Style::default().fg(Color::Yellow))),
        Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_measuring(f: &mut Frame, name: &str) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(f.area());
    let line = Line::from(Span::styled(
        format!("  Measuring {name}..."),
        Style::default().fg(Color::Yellow).bold(),
    ));
    f.render_widget(Paragraph::new(line), chunks[0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use disk::sweep::{Denials, Root, RootUsage, Unreadable};
    use ratatui::backend::TestBackend;
    use std::path::Path;

    fn root(name: &str) -> Root {
        Root {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
        }
    }

    fn usage(name: &str, total: u64, unreadable: Vec<Unreadable>) -> RootUsage {
        RootUsage {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            total,
            unattributed_total: 0,
            unattributed: Vec::new(),
            denials: unreadable.iter().fold(Denials::default(), |mut d, u| {
                match u.denial {
                    Denial::Protected => d.protected += 1,
                    Denial::Forbidden => d.forbidden += 1,
                }
                d
            }),
            unreadable,
        }
    }

    fn refusal(denial: Denial) -> Unreadable {
        Unreadable {
            path: Path::new("/tmp/locked").to_path_buf(),
            denial,
        }
    }

    fn started(names: &[&str]) -> App {
        let mut app = App::new(None);
        app.apply(Progress::Started {
            roots: names.iter().copied().map(root).collect(),
            jobs: names.len(),
        });
        app
    }

    fn scanned(app: &mut App, index: usize, bytes: u64) {
        app.apply(Progress::Scanned {
            root: index,
            path: PathBuf::from("/tmp/child"),
            root_bytes: bytes,
            remaining: 0,
        });
        app.apply(Progress::RootDone { root: index });
    }

    #[test]
    fn a_bar_is_proportional_to_the_largest_row() {
        assert_eq!(bar_width(100, 100, 20), 20);
        assert_eq!(bar_width(50, 100, 20), 10);
        assert_eq!(bar_width(25, 100, 20), 5);
        assert_eq!(bar_width(0, 100, 20), 0);
        assert_eq!(bar(50, 100, 20).chars().count(), 10);
    }

    #[test]
    fn a_bar_never_overflows_its_column() {
        assert_eq!(bar_width(200, 100, 20), 20);
        assert_eq!(bar(u64::MAX, 1, 8).chars().count(), 8);
    }

    #[test]
    fn a_bar_survives_nothing_to_draw_and_nowhere_to_draw_it() {
        assert_eq!(bar_width(0, 0, 20), 0);
        assert_eq!(bar_width(10, 0, 20), 0);
        assert_eq!(bar_width(10, 100, 0), 0);
        assert_eq!(bar(10, 0, 0), "");
        assert_eq!(bar(0, 0, 0), "");
    }

    #[test]
    fn a_row_too_small_to_fill_a_cell_still_shows_something() {
        assert_eq!(bar(1, 1_000_000, 20), "▏");
    }

    #[test]
    fn a_narrow_terminal_drops_the_bar_instead_of_overflowing() {
        assert_eq!(bar_columns(0), 0);
        assert_eq!(bar_columns(40), 0);
        assert!(bar_columns(200) <= 24);
    }

    #[test]
    fn rows_exist_before_anything_is_measured() {
        let app = started(&["Home", "User Library"]);
        let entries = app.entries();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].measure.label(), "scanning");
        assert_eq!(percent_text(entries[0].measure.bytes(), 0), "--");
    }

    #[test]
    fn rows_hold_their_order_until_the_sweep_finishes() {
        let mut app = started(&["Home", "User Library", "Applications"]);
        scanned(&mut app, 2, 90);
        scanned(&mut app, 0, 10);

        let order: Vec<&str> = app.entries().iter().map(|e| e.name).collect();
        assert_eq!(
            order,
            ["Home", "User Library", "Applications"],
            "a list that re-sorts mid-scan cannot be read"
        );

        app.finish(Sweep {
            roots: vec![
                usage("Home", 10, Vec::new()),
                usage("User Library", 50, Vec::new()),
                usage("Applications", 90, Vec::new()),
            ],
            rule_sizes: Vec::new(),
        });

        let order: Vec<&str> = app.entries().iter().map(|e| e.name).collect();
        assert_eq!(order, ["Applications", "User Library", "Home"]);
    }

    #[test]
    fn a_finished_sweep_stops_claiming_to_be_scanning() {
        let mut app = started(&["Home"]);
        assert!(app.scanning);

        app.finish(Sweep {
            roots: vec![usage("Home", 10, Vec::new())],
            rule_sizes: Vec::new(),
        });

        assert!(!app.scanning);
        assert_eq!(app.jobs_left, 0);
    }

    #[test]
    fn an_unreadable_root_with_no_bytes_is_unknown_not_empty() {
        let mut app = started(&["System data"]);
        app.finish(Sweep {
            roots: vec![usage("System data", 0, vec![refusal(Denial::Protected)])],
            rule_sizes: Vec::new(),
        });

        let label = app.entries()[0].measure.label();
        assert_eq!(label, "unknown");
        assert_ne!(label, format_size(0));
    }

    #[test]
    fn a_readable_empty_root_is_still_zero() {
        let mut app = started(&["Empty"]);
        app.finish(Sweep {
            roots: vec![usage("Empty", 0, Vec::new())],
            rule_sizes: Vec::new(),
        });

        assert_eq!(app.entries()[0].measure.label(), format_size(0));
    }

    #[test]
    fn denials_are_counted_apart() {
        let mut app = started(&["Home", "System data"]);
        app.finish(Sweep {
            roots: vec![
                usage("Home", 10, vec![refusal(Denial::Forbidden)]),
                usage(
                    "System data",
                    0,
                    vec![refusal(Denial::Protected), refusal(Denial::Protected)],
                ),
            ],
            rule_sizes: Vec::new(),
        });

        assert_eq!(denial_counts(&app.rows), (2, 1));
    }

    #[test]
    fn the_footer_only_offers_full_disk_access_where_it_would_help() {
        let protected = vec![RootRow {
            name: "System data".to_string(),
            path: PathBuf::from("/System"),
            bytes: 0,
            done: true,
            protected: 3,
            forbidden: 0,
        }];
        let forbidden = vec![RootRow {
            name: "Other users".to_string(),
            path: PathBuf::from("/Users"),
            bytes: 0,
            done: true,
            protected: 0,
            forbidden: 2,
        }];

        let note = denial_note(&protected).unwrap();
        assert!(note.contains("Full Disk Access"), "{note}");

        let note = denial_note(&forbidden).unwrap();
        assert!(note.contains("sudo"), "{note}");
        assert!(!note.contains("Full Disk Access"), "{note}");

        assert!(denial_note(&[]).is_none());
    }

    #[test]
    fn percentages_are_a_share_of_what_was_measured() {
        assert_eq!(percent_text(50, 200), "25.0%");
        assert_eq!(percent_text(0, 200), "--");
        assert_eq!(percent_text(50, 0), "--");
    }

    fn drawn(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn a_scanning_view_draws_rows_before_it_has_numbers() {
        let mut app = started(&["Home", "User Library"]);
        scanned(&mut app, 1, 40 * 1024 * 1024 * 1024);

        let screen = drawn(&app, 100, 20);
        assert!(screen.contains("Analyze Disk"), "{screen}");
        assert!(screen.contains("Scanning"), "{screen}");
        assert!(screen.contains("scanning"), "{screen}");
        assert!(screen.contains('█'), "{screen}");
        assert!(screen.contains("40.0 GB"), "{screen}");
    }

    #[test]
    fn drilling_in_lists_the_children_under_a_breadcrumb() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui");
        let mut app = started(&["Home"]);
        app.stack.push(View::Directory {
            name: "tui".to_string(),
            scan: scan_dir(&dir),
            cursor: 0,
        });

        assert!(app.entries().iter().any(|e| e.name == "analyze.rs"));
        assert_eq!(app.selected_path().unwrap().parent(), Some(dir.as_path()));

        let screen = drawn(&app, 100, 20);
        assert!(screen.contains("tui"), "{screen}");
        assert!(screen.contains("analyze.rs"), "{screen}");
    }

    #[test]
    fn a_terminal_too_small_for_the_layout_still_draws() {
        let mut app = started(&["Home", "User Library"]);
        scanned(&mut app, 0, 1024);

        for (width, height) in [(1, 1), (4, 2), (20, 3), (40, 6)] {
            drawn(&app, width, height);
        }
    }

    #[test]
    fn the_cursor_stays_inside_the_list() {
        let mut app = started(&["Home", "User Library"]);
        app.move_up();
        assert_eq!(app.cursor(), 0);

        app.move_down();
        app.move_down();
        app.move_down();
        assert_eq!(app.cursor(), 1);
    }
}
