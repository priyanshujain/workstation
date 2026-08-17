use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use disk::cleanup::{CleanAction, Target, discover_all_targets};
use disk::scan::{ScanResult, scan_dir};
use disk::util::format_size;
use ratatui::{prelude::*, widgets::*};

use crate::tui::widgets::centered_rect;

struct App {
    targets: Vec<Target>,
    marked_targets: HashSet<usize>,
    marked_paths: HashMap<PathBuf, MarkedPath>,
    stack: Vec<View>,
    cache: HashMap<PathBuf, ScanResult>,
    mode: Mode,
    results: Vec<CleanResult>,
    done_scroll: u16,
}

struct MarkedPath {
    is_dir: bool,
    size: u64,
}

enum View {
    Top {
        cursor: usize,
    },
    Directory {
        path: PathBuf,
        cursor: usize,
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
    size: u64,
    action: CleanAction,
}

struct CleanResult {
    label: String,
    outcome: Result<u64, String>,
}

impl App {
    fn new(targets: Vec<Target>) -> Self {
        Self {
            targets,
            marked_targets: HashSet::new(),
            marked_paths: HashMap::new(),
            stack: vec![View::Top { cursor: 0 }],
            cache: HashMap::new(),
            mode: Mode::Browse,
            results: Vec::new(),
            done_scroll: 0,
        }
    }

    fn current(&self) -> &View {
        self.stack.last().expect("stack never empty")
    }

    fn current_mut(&mut self) -> &mut View {
        self.stack.last_mut().expect("stack never empty")
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

    fn cursor_max(&self) -> usize {
        match self.current() {
            View::Top { .. } => self.targets.len().saturating_sub(1),
            View::Directory { path, .. } => self
                .cache
                .get(path)
                .map(|s| s.children.len().saturating_sub(1))
                .unwrap_or(0),
        }
    }

    fn move_up(&mut self) {
        match self.current_mut() {
            View::Top { cursor } | View::Directory { cursor, .. } => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
        }
    }

    fn move_down(&mut self) {
        let max = self.cursor_max();
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

    fn toggle_mark(&mut self) {
        match self.current() {
            View::Top { cursor } => {
                let i = *cursor;
                if i < self.targets.len() {
                    if self.marked_targets.contains(&i) {
                        self.marked_targets.remove(&i);
                    } else {
                        self.marked_targets.insert(i);
                    }
                }
            }
            View::Directory { path, cursor, .. } => {
                let Some(scan) = self.cache.get(path) else {
                    return;
                };
                let Some(child) = scan.children.get(*cursor) else {
                    return;
                };
                let key = child.path.clone();
                use std::collections::hash_map::Entry;
                match self.marked_paths.entry(key) {
                    Entry::Occupied(e) => {
                        e.remove();
                    }
                    Entry::Vacant(e) => {
                        e.insert(MarkedPath {
                            is_dir: child.is_dir,
                            size: child.size,
                        });
                    }
                }
            }
        }
    }

    fn marked_count(&self) -> usize {
        self.marked_targets.len() + self.marked_paths.len()
    }

    fn marked_size(&self) -> u64 {
        let from_targets: u64 = self
            .marked_targets
            .iter()
            .filter_map(|&i| self.targets.get(i).map(|t| t.size))
            .sum();
        let from_paths: u64 = self.marked_paths.values().map(|m| m.size).sum();
        from_targets + from_paths
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
                label: format!("{} — {}", t.name, t.description),
                size: t.size,
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

        ops.sort_by_key(|o| std::cmp::Reverse(o.size));
        ops
    }

    fn total_freed(&self) -> u64 {
        self.results
            .iter()
            .filter_map(|r| r.outcome.as_ref().ok())
            .sum()
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
    let targets = discover_all_targets();
    let mut app = App::new(targets);
    super::run(|terminal| event_loop(&mut app, terminal))
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    loop {
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
}

fn drill_in(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let (next_path, next_crumb) = match app.current() {
        View::Top { cursor } => {
            let Some(t) = app.targets.get(*cursor) else {
                return Ok(());
            };
            match t.action() {
                CleanAction::RemoveContents(p) | CleanAction::RemoveDir(p) => {
                    if !p.is_dir() {
                        return Ok(());
                    }
                    (p.clone(), t.name.clone())
                }
                _ => return Ok(()),
            }
        }
        View::Directory { path, cursor, .. } => {
            let Some(scan) = app.cache.get(path) else {
                return Ok(());
            };
            let Some(child) = scan.children.get(*cursor) else {
                return Ok(());
            };
            if !child.is_dir {
                return Ok(());
            }
            (child.path.clone(), child.name.clone())
        }
    };

    if !app.cache.contains_key(&next_path) {
        terminal.draw(|f| render_scanning(f, &next_path))?;
        let result = scan_dir(&next_path);
        app.cache.insert(next_path.clone(), result);
    }

    app.stack.push(View::Directory {
        path: next_path,
        cursor: 0,
        crumb: next_crumb,
    });

    Ok(())
}

fn run_ops(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let ops = app.pending_ops();
    for op in ops {
        terminal.draw(|f| render(f, app))?;
        let target = Target::new(&op.label, "", op.size, op.action);
        let outcome = target.clean();
        app.results.push(CleanResult {
            label: op.label,
            outcome,
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
        View::Top { cursor } => render_top(f, chunks[1], app, *cursor),
        View::Directory { path, cursor, .. } => render_directory(f, chunks[1], app, path, *cursor),
    }

    render_footer(f, chunks[2], app);

    if app.mode == Mode::Confirm {
        render_confirm(f, area, app);
    }
}

fn render_top(f: &mut Frame, body: Rect, app: &App, cursor: usize) {
    let rows: Vec<Row> = app
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let marked = app.marked_targets.contains(&i);
            let check = if marked { " ✓" } else { " ·" };
            let check_style = if marked {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let size_str = if t.size > 0 {
                format_size(t.size)
            } else {
                "—".to_string()
            };

            let drillable = matches!(
                t.action(),
                CleanAction::RemoveContents(_) | CleanAction::RemoveDir(_)
            );
            let marker = if drillable { "📁" } else { "  " };

            let row_style = if i == cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(marker),
                Cell::from(t.name.as_str()).style(Style::default().fg(Color::White)),
                Cell::from(t.description.as_str()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(size_str).style(Style::default().fg(Color::Yellow)),
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
    .block(
        Block::default()
            .borders(Borders::NONE)
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(table, body);
}

fn render_directory(f: &mut Frame, body: Rect, app: &App, path: &Path, cursor: usize) {
    let Some(scan) = app.cache.get(path) else {
        let p = Paragraph::new("  Scanning...")
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .padding(Padding::horizontal(1)),
            );
        f.render_widget(p, body);
        return;
    };

    if scan.children.is_empty() {
        let p = Paragraph::new("  (empty)")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .padding(Padding::horizontal(1)),
            );
        f.render_widget(p, body);
        return;
    }

    let rows: Vec<Row> = scan
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let marked = app.marked_paths.contains_key(&c.path);
            let check = if marked { " ✓" } else { " ·" };
            let check_style = if marked {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let kind_marker = if c.is_dir { "📁" } else { "  " };
            let name_color = if c.is_dir { Color::Cyan } else { Color::White };
            let row_style = if i == cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(kind_marker),
                Cell::from(c.name.as_str()).style(Style::default().fg(name_color)),
                Cell::from(format_size(c.size)).style(Style::default().fg(Color::Yellow)),
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
    .block(
        Block::default()
            .borders(Borders::NONE)
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(table, body);
}

fn render_footer(f: &mut Frame, footer: Rect, app: &App) {
    let line = if app.marked_count() > 0 {
        Line::from(vec![
            Span::styled(
                format!(
                    " Marked: {} ({}) ",
                    app.marked_count(),
                    format_size(app.marked_size())
                ),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                " [Space] Mark  [Enter] Drill  [x] Clean  [Esc] Back  [q] Quit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " [Space] Mark  [Enter] Drill  [Esc] Back  [q] Quit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(line, footer);
}

fn render_confirm(f: &mut Frame, area: Rect, app: &App) {
    let ops = app.pending_ops();
    let total: u64 = ops.iter().map(|o| o.size).sum();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Clean {} items ({})?", ops.len(), format_size(total)),
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
                format!("  {}", format_size(op.size)),
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
                format_size(app.total_freed()),
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

fn render_scanning(f: &mut Frame, path: &Path) {
    let area = f.area();
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(area);
    let header = Paragraph::new(Line::from(vec![Span::styled(
        format!("  Scanning {}...", path.display()),
        Style::default().fg(Color::Yellow).bold(),
    )]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);
}

fn result_lines(r: &CleanResult) -> Vec<Line<'_>> {
    match &r.outcome {
        Ok(bytes) => vec![Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(Color::Green)),
            Span::styled(r.label.as_str(), Style::default().fg(Color::White)),
            Span::styled(
                format!("  {}", format_size(*bytes)),
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
