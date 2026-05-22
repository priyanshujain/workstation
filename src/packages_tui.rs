use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use wsctl_core::brew_info::{self, InstalledPackage, PkgKind};
use wsctl_core::brewfile::{self, BrewfileEntry, EntryKind};
use wsctl_core::scan;
use wsctl_core::SystemCommandRunner;

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
    Done,
}

struct App {
    brewfile_path: PathBuf,
    rows: Vec<Row>,
    selected: Vec<bool>,
    cursor: usize,
    mode: Mode,
}

impl App {
    fn new(brewfile_path: PathBuf, rows: Vec<Row>) -> Self {
        let len = rows.len();
        Self {
            brewfile_path,
            rows,
            selected: vec![false; len],
            cursor: 0,
            mode: Mode::Select,
        }
    }

    fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&s| s).count()
    }

    fn selected_size(&self) -> u64 {
        self.rows
            .iter()
            .zip(self.selected.iter())
            .filter(|(_, &s)| s)
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
        }
    }

    fn move_down(&mut self) {
        if self.cursor + 1 < self.rows.len() {
            self.cursor += 1;
        }
    }
}

pub fn run() -> Result<()> {
    let path = brewfile::discover().ok_or_else(|| {
        anyhow!(
            "No Brewfile found. Searched: $HOMEBREW_BUNDLE_FILE, ./Brewfile, \
             ~/.Brewfile, ~/Brewfile, $XDG_CONFIG_HOME/homebrew/Brewfile"
        )
    })?;

    eprintln!("Loading {}…", path.display());

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let entries: Vec<BrewfileEntry> = brewfile::parse(&content)
        .into_iter()
        .filter(|e| matches!(e.kind, EntryKind::Formula | EntryKind::Cask))
        .collect();

    if entries.is_empty() {
        return Err(anyhow!(
            "{} has no brew or cask entries",
            path.display()
        ));
    }

    let runner = SystemCommandRunner::new();
    let mut installed = brew_info::fetch_installed(&runner)?;
    let prefix = brew_info::brew_prefix(&runner)?;
    brew_info::attach_sizes(&prefix, &mut installed);

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

    let app = App::new(path, rows);
    run_tui(app)
}

fn run_tui(mut app: App) -> Result<()> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut app, &mut terminal);

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    result
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
                    // Uninstall flow is wired in the next step.
                    app.mode = Mode::Done;
                }
                _ => app.mode = Mode::Select,
            },
            Mode::Done => match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => return Ok(()),
                _ => {}
            },
        }
    }
}

fn render(f: &mut Frame, app: &App) {
    match app.mode {
        Mode::Select | Mode::Confirm => render_select(f, app),
        Mode::Done => render_done(f, app),
    }
}

fn render_select(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled("  wsctl packages", Style::default().fg(Color::Cyan).bold()),
        Span::styled(
            format!("  {}", app.brewfile_path.display()),
            Style::default().fg(Color::DarkGray),
        ),
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
        .map(|(i, row)| package_row(i, row, app))
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
                "", "name", "kind", "status", "size", "deps", "installed",
            ])
            .style(Style::default().fg(Color::DarkGray)),
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .padding(Padding::horizontal(1)),
        );
    f.render_widget(table, chunks[1]);

    let status = if app.selected_count() > 0 {
        Line::from(vec![
            Span::styled(
                format!(
                    " Selected: {} pkgs ({}) ",
                    app.selected_count(),
                    scan::format_size(app.selected_size())
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
                    scan::format_size(app.selected_size())
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

fn package_row<'a>(i: usize, row: &'a Row, app: &'a App) -> ratatui::widgets::Row<'a> {
    let check = if app.selected[i] { " ✓" } else { "  " };
    let check_style = if app.selected[i] {
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
        scan::format_size(row.size())
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

    let row_style = if i == app.cursor {
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
    let secs = unix_secs as i64;
    let days = secs / 86_400;
    let (mut y, mut m, mut d) = (1970i64, 1u32, 1u32);
    let mut remaining = days;
    loop {
        let year_days: i64 = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }
    let months_lengths: [i64; 12] = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for (mi, &len) in months_lengths.iter().enumerate() {
        if remaining < len {
            m = mi as u32 + 1;
            d = remaining as u32 + 1;
            break;
        }
        remaining -= len;
    }
    format!("{y:04}-{m:02}-{d:02}")
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

fn render_done(f: &mut Frame, _app: &App) {
    let area = f.area();
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Uninstall flow not wired yet (placeholder).",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Press q to exit",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Done ")
            .border_style(Style::default().fg(Color::Green)),
    );
    f.render_widget(body, area);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let y = area.height.saturating_sub(height) / 2;
    let width = area.width * percent_x / 100;
    let x = area.width.saturating_sub(width) / 2;
    Rect::new(x, y, width, height)
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
}
