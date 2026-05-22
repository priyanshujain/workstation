use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{prelude::*, widgets::*};
use packages::brew::brewfile::{self, BrewfileEntry, BrewfileSource, EntryKind, RemoveTarget};
use packages::brew::info::{self, InstalledPackage, PkgKind};
use packages::brew::ops;
use disk::util::format_size;
use wsctl_core::{CommandRunner, SystemCommandRunner};

use crate::tui::widgets::centered_rect;

#[derive(Debug)]
struct Row {
    entry: BrewfileEntry,
    installed: Option<InstalledPackage>,
}

impl Row {
    fn kind(&self) -> PkgKind {
        match self.entry.kind {
            EntryKind::Cask => PkgKind::Cask,
            _ => PkgKind::Formula,
        }
    }

    fn size(&self) -> u64 {
        self.installed.as_ref().map(|p| p.size_bytes).unwrap_or(0)
    }
}

#[derive(PartialEq, Eq)]
enum Mode {
    Select,
    Confirm,
    Blocked,
    Running,
    Done,
}

struct StepResult {
    name: String,
    kind: PkgKind,
    size_bytes: u64,
    outcome: std::result::Result<(), String>,
}

struct App {
    brewfile_path: PathBuf,
    brewfile_source: BrewfileSource,
    rows: Vec<Row>,
    selected: Vec<bool>,
    cursor: usize,
    table_state: TableState,
    mode: Mode,
    /// dep_name -> packages currently installed that list it as a dep
    dependents_of: HashMap<String, Vec<String>>,
    /// (package, dependents-outside-selection) — populated when blocked
    blockers: Vec<(String, Vec<String>)>,
    results: Vec<StepResult>,
    autoremove_summary: Option<String>,
    brewfile_backup: Option<PathBuf>,
    brewfile_updated_count: usize,
}

impl App {
    fn new(
        brewfile_path: PathBuf,
        brewfile_source: BrewfileSource,
        rows: Vec<Row>,
        dependents_of: HashMap<String, Vec<String>>,
    ) -> Self {
        let len = rows.len();
        let mut table_state = TableState::default();
        if len > 0 {
            table_state.select(Some(0));
        }
        Self {
            brewfile_path,
            brewfile_source,
            rows,
            selected: vec![false; len],
            cursor: 0,
            table_state,
            mode: Mode::Select,
            dependents_of,
            blockers: Vec::new(),
            results: Vec::new(),
            autoremove_summary: None,
            brewfile_backup: None,
            brewfile_updated_count: 0,
        }
    }

    fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&s| s).count()
    }

    fn selected_size(&self) -> u64 {
        self.rows
            .iter()
            .zip(self.selected.iter())
            .filter(|&(_, &s)| s)
            .map(|(r, _)| r.size())
            .sum()
    }

    fn toggle_current(&mut self) {
        if !self.rows.is_empty() {
            self.selected[self.cursor] = !self.selected[self.cursor];
        }
    }

    fn toggle_all(&mut self) {
        let all = self.selected.iter().all(|&s| s);
        self.selected.iter_mut().for_each(|s| *s = !all);
    }

    fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.table_state.select(Some(self.cursor));
        }
    }

    fn move_down(&mut self) {
        if self.cursor + 1 < self.rows.len() {
            self.cursor += 1;
            self.table_state.select(Some(self.cursor));
        }
    }
}

pub fn run() -> Result<()> {
    let runner = SystemCommandRunner::new();
    let (path, source) = resolve_or_dump_brewfile(&runner)?;

    eprintln!("Loading {}…", path.display());

    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let entries: Vec<BrewfileEntry> = brewfile::parse(&content)
        .into_iter()
        .filter(|e| matches!(e.kind, EntryKind::Formula | EntryKind::Cask))
        .collect();

    if entries.is_empty() {
        return Err(anyhow!("{} has no brew or cask entries", path.display()));
    }

    let mut installed = info::fetch_installed(&runner)?;
    let prefix = info::brew_prefix(&runner)?;
    info::attach_sizes(&prefix, &mut installed);

    let mut rows: Vec<Row> = entries
        .into_iter()
        .map(|entry| {
            let kind_pkg = if entry.kind == EntryKind::Cask {
                PkgKind::Cask
            } else {
                PkgKind::Formula
            };
            let installed = installed
                .iter()
                .find(|p| p.kind == kind_pkg && p.name == entry.name)
                .cloned();
            Row { entry, installed }
        })
        .collect();

    rows.sort_by(|a, b| {
        b.installed
            .is_some()
            .cmp(&a.installed.is_some())
            .then(b.size().cmp(&a.size()))
            .then(a.entry.name.cmp(&b.entry.name))
    });

    let dependents_of = build_dependents_map(&installed);
    let app = App::new(path, source, rows, dependents_of);
    run_tui(app)
}

fn resolve_or_dump_brewfile(runner: &dyn CommandRunner) -> Result<(PathBuf, BrewfileSource)> {
    if let Some(found) = brewfile::discover() {
        return Ok(found);
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("cannot determine home directory to write Brewfile"))?;
    let target = home.join("Brewfile");
    eprintln!(
        "No Brewfile found. Generating one from currently installed packages → {}",
        target.display()
    );
    ops::bundle_dump(runner, &target)
        .with_context(|| format!("brew bundle dump --file={}", target.display()))?;
    brewfile::discover().ok_or_else(|| {
        anyhow!(
            "brew bundle dump completed but no Brewfile was discovered at {}",
            target.display()
        )
    })
}

fn build_dependents_map(installed: &[InstalledPackage]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in installed {
        for dep in &pkg.deps {
            map.entry(dep.clone()).or_default().push(pkg.name.clone());
        }
    }
    map
}

/// External dependents = packages that depend on `name` but are NOT in `selection`.
fn external_dependents(
    name: &str,
    dependents_of: &HashMap<String, Vec<String>>,
    selection: &HashSet<&str>,
) -> Vec<String> {
    dependents_of
        .get(name)
        .map(|deps| {
            deps.iter()
                .filter(|d| !selection.contains(d.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Order selected indices so packages that depend on others come first
/// (leaves of the in-selection dependency graph are uninstalled first).
fn uninstall_order(rows: &[Row], indices: &[usize]) -> Vec<usize> {
    let selection: HashSet<&str> = indices
        .iter()
        .map(|&i| rows[i].entry.name.as_str())
        .collect();
    let mut remaining: Vec<usize> = indices.to_vec();
    let mut order: Vec<usize> = Vec::with_capacity(indices.len());
    while !remaining.is_empty() {
        let pick = remaining.iter().position(|&i| {
            let name = rows[i].entry.name.as_str();
            !remaining.iter().any(|&j| {
                if j == i {
                    return false;
                }
                rows[j]
                    .installed
                    .as_ref()
                    .map(|p| p.deps.iter().any(|d| d == name))
                    .unwrap_or(false)
            }) && selection.contains(name) // keep using selection to satisfy borrow
        });
        match pick {
            Some(pos) => order.push(remaining.remove(pos)),
            None => {
                // Dependency cycle (shouldn't happen in brew). Append the rest as-is.
                order.append(&mut remaining);
            }
        }
    }
    order
}

fn run_tui(mut app: App) -> Result<()> {
    super::run(|terminal| event_loop(&mut app, terminal))
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    loop {
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
        match app.mode {
            Mode::Select => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Char(' ') => app.toggle_current(),
                KeyCode::Char('a') => app.toggle_all(),
                KeyCode::Enter if app.selected_count() > 0 => {
                    app.mode = Mode::Confirm;
                }
                _ => {}
            },
            Mode::Confirm => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let runner = SystemCommandRunner::new();
                    app.blockers = compute_blockers(app);
                    if !app.blockers.is_empty() {
                        app.mode = Mode::Blocked;
                    } else {
                        app.mode = Mode::Running;
                        run_uninstalls(app, terminal, &runner)?;
                        app.mode = Mode::Done;
                    }
                }
                _ => app.mode = Mode::Select,
            },
            Mode::Blocked => match key.code {
                KeyCode::Char('d') => {
                    deselect_blockers(app);
                    app.blockers.clear();
                    app.mode = Mode::Select;
                }
                KeyCode::Char('q') | KeyCode::Esc => app.mode = Mode::Select,
                _ => {}
            },
            Mode::Done => match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
                _ => {}
            },
            Mode::Running => {}
        }
    }
}

fn compute_blockers(app: &App) -> Vec<(String, Vec<String>)> {
    let selection: HashSet<&str> = app
        .selected
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s)
        .map(|(i, _)| app.rows[i].entry.name.as_str())
        .collect();
    let mut blockers = Vec::new();
    for (i, row) in app.rows.iter().enumerate() {
        if !app.selected[i] || row.kind() != PkgKind::Formula {
            continue;
        }
        let dependents = external_dependents(&row.entry.name, &app.dependents_of, &selection);
        if !dependents.is_empty() {
            blockers.push((row.entry.name.clone(), dependents));
        }
    }
    blockers
}

fn deselect_blockers(app: &mut App) {
    let blocked: HashSet<&str> = app.blockers.iter().map(|(n, _)| n.as_str()).collect();
    for (i, row) in app.rows.iter().enumerate() {
        if blocked.contains(row.entry.name.as_str()) {
            app.selected[i] = false;
        }
    }
}

fn run_uninstalls(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runner: &dyn CommandRunner,
) -> Result<()> {
    let selected: Vec<usize> = app
        .selected
        .iter()
        .enumerate()
        .filter(|&(_, &s)| s)
        .map(|(i, _)| i)
        .collect();
    let order = uninstall_order(&app.rows, &selected);

    for idx in order {
        terminal.draw(|f| render(f, app))?;
        let row = &app.rows[idx];
        let name = row.entry.name.clone();
        let kind = row.kind();
        let size = row.size();
        let outcome = perform_uninstall(runner, &name, kind);
        app.results.push(StepResult {
            name,
            kind,
            size_bytes: size,
            outcome,
        });
    }

    terminal.draw(|f| render(f, app))?;
    match ops::autoremove(runner) {
        Ok(out) => app.autoremove_summary = Some(out),
        Err(e) => app.autoremove_summary = Some(format!("autoremove failed: {e}")),
    }

    let targets: Vec<RemoveTarget> = app
        .results
        .iter()
        .filter(|r| r.outcome.is_ok())
        .map(|r| RemoveTarget {
            kind: match r.kind {
                PkgKind::Formula => EntryKind::Formula,
                PkgKind::Cask => EntryKind::Cask,
            },
            name: r.name.clone(),
        })
        .collect();
    if !targets.is_empty()
        && let Ok(summary) = brewfile::remove_entries(&app.brewfile_path, &targets)
    {
        app.brewfile_backup = summary.backup;
        app.brewfile_updated_count = summary.removed.len();
    }
    Ok(())
}

fn perform_uninstall(
    runner: &dyn CommandRunner,
    name: &str,
    kind: PkgKind,
) -> std::result::Result<(), String> {
    let res = match kind {
        PkgKind::Formula => ops::uninstall_formula(runner, name),
        PkgKind::Cask => ops::uninstall_cask_zap(runner, name),
    };
    res.map_err(|e| e.to_string())?;
    if ops::is_installed(runner, name, kind).unwrap_or(true) {
        return Err("still present after uninstall".into());
    }
    Ok(())
}

fn render(f: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::Select | Mode::Confirm => render_select(f, app),
        Mode::Blocked => {
            render_select(f, app);
            render_blocked_popup(f, app);
        }
        Mode::Running => render_progress(f, app),
        Mode::Done => render_done(f, app),
    }
}

fn render_blocked_popup(f: &mut Frame, app: &App) {
    let area = f.area();
    let height = (5 + app.blockers.len() as u16 * 2).min(area.height.saturating_sub(4));
    let popup = centered_rect(70, height, area);
    f.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " Blocked: some selected packages are dependencies of others.",
            Style::default().fg(Color::Red).bold(),
        )),
        Line::from(""),
    ];
    for (pkg, deps) in &app.blockers {
        lines.push(Line::from(vec![
            Span::styled(format!("  {pkg}"), Style::default().fg(Color::Yellow)),
            Span::styled("  used by: ", Style::default().fg(Color::DarkGray)),
            Span::styled(deps.join(", "), Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" d ", Style::default().fg(Color::Green).bold()),
        Span::raw("Deselect blocked + retry   "),
        Span::styled(" q ", Style::default().fg(Color::Red).bold()),
        Span::raw("Cancel"),
    ]));

    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Cannot proceed ")
            .border_style(Style::default().fg(Color::Red)),
    );
    f.render_widget(body, popup);
}

fn render_progress(f: &mut Frame, app: &App) {
    let area = f.area();
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Uninstalling…",
            Style::default().fg(Color::Yellow).bold(),
        )),
        Line::from(""),
    ];
    for r in &app.results {
        lines.push(result_line(r));
    }
    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Progress ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(body, area);
}

fn result_line(r: &StepResult) -> Line<'_> {
    let kind = match r.kind {
        PkgKind::Formula => "formula",
        PkgKind::Cask => "cask",
    };
    match &r.outcome {
        Ok(()) => Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(Color::Green)),
            Span::styled(r.name.as_str(), Style::default().fg(Color::White)),
            Span::styled(format!("  {kind}"), Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("  -{}", format_size(r.size_bytes)),
                Style::default().fg(Color::Green),
            ),
        ]),
        Err(e) => Line::from(vec![
            Span::styled("  ✗ ", Style::default().fg(Color::Red)),
            Span::styled(r.name.as_str(), Style::default().fg(Color::White)),
            Span::styled(format!("  {e}"), Style::default().fg(Color::Red)),
        ]),
    }
}

fn render_select(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let warn = app.brewfile_source == BrewfileSource::Cwd;
    let path_prefix = if warn { "  ⚠ project-local: " } else { "  " };
    let path_style = if warn {
        Style::default().fg(Color::Yellow).bold()
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled("  wsctl packages", Style::default().fg(Color::Cyan).bold()),
        Span::styled(path_prefix, path_style),
        Span::styled(app.brewfile_path.display().to_string(), path_style),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    let rows: Vec<ratatui::widgets::Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| package_row(row, app.selected[i], app.cursor == i))
        .collect();

    let widths = [
        Constraint::Length(3),
        Constraint::Length(26),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(12),
    ];
    let table = Table::new(rows, widths)
        .header(
            ratatui::widgets::Row::new(vec![
                "",
                "name",
                "kind",
                "status",
                "size",
                "deps",
                "installed",
            ])
            .style(Style::default().fg(Color::DarkGray)),
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .padding(Padding::horizontal(1)),
        );
    f.render_stateful_widget(table, chunks[1], &mut app.table_state);

    let status = if app.selected_count() > 0 {
        Line::from(vec![
            Span::styled(
                format!(
                    " Selected: {} pkgs ({}) ",
                    app.selected_count(),
                    format_size(app.selected_size())
                ),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                " [Enter] Uninstall  [a] All  [q] Quit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " [Space] Select  [a] All  [j/k] Navigate  [Enter] Uninstall  [q] Quit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(status, chunks[2]);

    if app.mode == Mode::Confirm {
        let popup = centered_rect(60, 9, area);
        f.render_widget(Clear, popup);
        let body = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "Uninstall {} packages ({})?",
                    app.selected_count(),
                    format_size(app.selected_size())
                ),
                Style::default().fg(Color::Yellow).bold(),
            )),
            Line::from(Span::styled(
                "  Casks are removed with --zap (preferences/data too).",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  brew autoremove runs after to clear orphaned deps.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(" y ", Style::default().fg(Color::Green).bold()),
                Span::raw("Yes   "),
                Span::styled(" n ", Style::default().fg(Color::Red).bold()),
                Span::raw("No"),
            ]),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Confirm ")
                .border_style(Style::default().fg(Color::Yellow)),
        );
        f.render_widget(body, popup);
    }
}

fn package_row(row: &Row, selected: bool, is_cursor: bool) -> ratatui::widgets::Row<'_> {
    let check = if selected { " ✓" } else { "  " };
    let check_style = if selected {
        Style::default().fg(Color::Green).bold()
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let kind = match row.entry.kind {
        EntryKind::Cask => "cask",
        _ => "formula",
    };

    let (status_str, status_style) = if row.installed.is_some() {
        ("installed", Style::default().fg(Color::Green))
    } else {
        ("missing", Style::default().fg(Color::DarkGray))
    };

    let size_str = if row.size() > 0 {
        format_size(row.size())
    } else {
        "—".to_string()
    };

    let deps_str = row
        .installed
        .as_ref()
        .map(|p| p.deps.len().to_string())
        .unwrap_or_else(|| "—".to_string());

    let date_str = row
        .installed
        .as_ref()
        .and_then(|p| p.installed_at)
        .map(format_date)
        .unwrap_or_else(|| "—".to_string());

    let row_style = if is_cursor {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ratatui::widgets::Row::new(vec![
        Cell::from(check).style(check_style),
        Cell::from(row.entry.name.as_str()).style(Style::default().fg(Color::White)),
        Cell::from(kind).style(Style::default().fg(Color::Cyan)),
        Cell::from(status_str).style(status_style),
        Cell::from(size_str).style(Style::default().fg(Color::Yellow)),
        Cell::from(deps_str).style(Style::default().fg(Color::DarkGray)),
        Cell::from(date_str).style(Style::default().fg(Color::DarkGray)),
    ])
    .style(row_style)
}

fn format_date(unix_secs: u64) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::from_unix_timestamp(unix_secs as i64)
        .ok()
        .and_then(|dt| dt.format(&format).ok())
        .unwrap_or_else(|| "—".to_string())
}

fn render_done(f: &mut Frame, app: &App) {
    let area = f.area();
    let freed: u64 = app
        .results
        .iter()
        .filter(|r| r.outcome.is_ok())
        .map(|r| r.size_bytes)
        .sum();
    let ok = app.results.iter().filter(|r| r.outcome.is_ok()).count();
    let failed = app.results.len() - ok;

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Done",
            Style::default().fg(Color::Green).bold(),
        )),
        Line::from(""),
    ];
    for r in &app.results {
        lines.push(result_line(r));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Removed: ", Style::default().bold()),
        Span::styled(
            format!("{ok} ok, {failed} failed"),
            Style::default().fg(Color::White),
        ),
        Span::styled("    Freed: ", Style::default().bold()),
        Span::styled(
            format_size(freed),
            Style::default().fg(Color::Green).bold(),
        ),
    ]));
    if let Some(out) = &app.autoremove_summary {
        let summary = autoremove_summary(out);
        lines.push(Line::from(vec![
            Span::styled("  Autoremove: ", Style::default().bold()),
            Span::styled(summary, Style::default().fg(Color::White)),
        ]));
    }
    if app.brewfile_updated_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("  Brewfile: ", Style::default().bold()),
            Span::styled(
                format!(
                    "{} line(s) removed from {}",
                    app.brewfile_updated_count,
                    app.brewfile_path.display()
                ),
                Style::default().fg(Color::White),
            ),
        ]));
        if let Some(bak) = &app.brewfile_backup {
            lines.push(Line::from(vec![
                Span::styled("  Backup:   ", Style::default().bold()),
                Span::styled(
                    bak.display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press q to exit",
        Style::default().fg(Color::DarkGray),
    )));

    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(Style::default().fg(Color::Green)),
    );
    f.render_widget(body, area);
}

fn autoremove_summary(stdout: &str) -> String {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return "nothing to remove".into();
    }
    let count = trimmed
        .lines()
        .filter(|l| l.starts_with("Uninstalling "))
        .count();
    if count == 0 {
        return trimmed.lines().next().unwrap_or("done").to_string();
    }
    format!("{count} orphaned dep(s) removed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_date_known_timestamps() {
        // 1700000000 = 2023-11-14 22:13:20 UTC
        assert_eq!(format_date(1700000000), "2023-11-14");
        // 1577836800 = 2020-01-01 00:00:00 UTC
        assert_eq!(format_date(1577836800), "2020-01-01");
        // 0 = epoch
        assert_eq!(format_date(0), "1970-01-01");
    }

    #[test]
    fn format_date_handles_leap_year() {
        // 951782400 = 2000-02-29 00:00:00 UTC
        assert_eq!(format_date(951782400), "2000-02-29");
    }

    fn row(name: &str, kind: EntryKind, deps: Vec<&str>) -> Row {
        let entry = BrewfileEntry {
            kind,
            name: name.to_string(),
            full_name: name.to_string(),
            raw: format!("brew \"{name}\""),
            line_number: 1,
        };
        let kind_pkg = match kind {
            EntryKind::Cask => PkgKind::Cask,
            _ => PkgKind::Formula,
        };
        let installed = Some(InstalledPackage {
            name: name.to_string(),
            kind: kind_pkg,
            version: Some("1".into()),
            size_bytes: 0,
            deps: deps.into_iter().map(|s| s.to_string()).collect(),
            installed_at: None,
        });
        Row { entry, installed }
    }

    #[test]
    fn build_dependents_map_inverts_dep_lists() {
        let installed = vec![
            InstalledPackage {
                name: "ripgrep".into(),
                kind: PkgKind::Formula,
                version: None,
                size_bytes: 0,
                deps: vec!["pcre2".into()],
                installed_at: None,
            },
            InstalledPackage {
                name: "bat".into(),
                kind: PkgKind::Formula,
                version: None,
                size_bytes: 0,
                deps: vec!["pcre2".into()],
                installed_at: None,
            },
        ];
        let map = build_dependents_map(&installed);
        let mut got = map.get("pcre2").cloned().unwrap_or_default();
        got.sort();
        assert_eq!(got, vec!["bat", "ripgrep"]);
    }

    #[test]
    fn external_dependents_filters_selection() {
        let mut map = HashMap::new();
        map.insert("pcre2".to_string(), vec!["ripgrep".into(), "bat".into()]);
        let selection: HashSet<&str> = ["ripgrep"].into_iter().collect();
        let got = external_dependents("pcre2", &map, &selection);
        assert_eq!(got, vec!["bat"]);
    }

    #[test]
    fn uninstall_order_puts_dependents_before_their_deps() {
        // ripgrep depends on pcre2. Order should be [ripgrep, pcre2].
        let rows = vec![
            row("pcre2", EntryKind::Formula, vec![]),
            row("ripgrep", EntryKind::Formula, vec!["pcre2"]),
        ];
        let order = uninstall_order(&rows, &[0, 1]);
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn uninstall_order_handles_unrelated_packages() {
        let rows = vec![
            row("fzf", EntryKind::Formula, vec![]),
            row("zoxide", EntryKind::Formula, vec![]),
        ];
        let order = uninstall_order(&rows, &[0, 1]);
        assert_eq!(order.len(), 2);
        assert!(order.contains(&0) && order.contains(&1));
    }

    #[test]
    fn autoremove_summary_counts_uninstall_lines() {
        let out = "Uninstalling pcre2\nUninstalling libssh\n";
        assert_eq!(autoremove_summary(out), "2 orphaned dep(s) removed");
    }

    #[test]
    fn autoremove_summary_handles_no_op() {
        assert_eq!(autoremove_summary(""), "nothing to remove");
        assert_eq!(autoremove_summary("\n  \n"), "nothing to remove");
    }
}
