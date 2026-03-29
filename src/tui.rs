use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::*, widgets::*};
use wsctl_core::scan::{self, CleanupTarget};

struct App {
    targets: Vec<CleanupTarget>,
    selected: Vec<bool>,
    cursor: usize,
    mode: Mode,
    results: Vec<CleanResult>,
}

#[derive(PartialEq)]
enum Mode {
    Select,
    Confirm,
    Running,
    Done,
}

struct CleanResult {
    name: String,
    outcome: Result<u64, String>,
}

impl App {
    fn new(targets: Vec<CleanupTarget>) -> Self {
        let len = targets.len();
        Self {
            targets,
            selected: vec![false; len],
            cursor: 0,
            mode: Mode::Select,
            results: Vec::new(),
        }
    }

    fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&s| s).count()
    }

    fn selected_size(&self) -> u64 {
        self.targets
            .iter()
            .zip(self.selected.iter())
            .filter(|(_, &s)| s)
            .map(|(t, _)| t.size)
            .sum()
    }

    fn total_freed(&self) -> u64 {
        self.results
            .iter()
            .filter_map(|r| r.outcome.as_ref().ok())
            .sum()
    }

    fn toggle_current(&mut self) {
        if !self.targets.is_empty() {
            self.selected[self.cursor] = !self.selected[self.cursor];
        }
    }

    fn toggle_all(&mut self) {
        let all_selected = self.selected.iter().all(|&s| s);
        for s in &mut self.selected {
            *s = !all_selected;
        }
    }

    fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.cursor < self.targets.len().saturating_sub(1) {
            self.cursor += 1;
        }
    }
}

pub fn run() -> io::Result<()> {
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

    let targets = scan::discover_cleanup_targets();
    let mut app = App::new(targets);

    loop {
        terminal.draw(|f| render(f, &app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.mode {
                    Mode::Select => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
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
                            app.mode = Mode::Running;
                            run_cleanups(&mut app, &mut terminal)?;
                            app.mode = Mode::Done;
                        }
                        _ => {
                            app.mode = Mode::Select;
                        }
                    },
                    Mode::Done => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => break,
                        _ => {}
                    },
                    Mode::Running => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

fn run_cleanups(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> io::Result<()> {
    let indices: Vec<usize> = app
        .selected
        .iter()
        .enumerate()
        .filter(|(_, &s)| s)
        .map(|(i, _)| i)
        .collect();

    for &idx in &indices {
        terminal.draw(|f| render(f, app))?;

        let name = app.targets[idx].name.clone();
        let outcome = app.targets[idx].clean();
        app.results.push(CleanResult { name, outcome });
    }

    Ok(())
}

fn render(f: &mut Frame, app: &App) {
    match app.mode {
        Mode::Select | Mode::Confirm => render_select(f, app),
        Mode::Running => render_progress(f, app),
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
        Span::styled("  wsctl cleanup", Style::default().fg(Color::Cyan).bold()),
        Span::styled("  Disk Cleanup", Style::default().fg(Color::White)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    let rows: Vec<Row> = app
        .targets
        .iter()
        .enumerate()
        .map(|(i, target)| {
            let check = if app.selected[i] { " ✓" } else { "  " };
            let check_style = if app.selected[i] {
                Style::default().fg(Color::Green).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let size_str = if target.size > 0 {
                scan::format_size(target.size)
            } else {
                "—".to_string()
            };

            let row_style = if i == app.cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(target.name.as_str()).style(Style::default().fg(Color::White)),
                Cell::from(target.description.as_str())
                    .style(Style::default().fg(Color::DarkGray)),
                Cell::from(size_str).style(Style::default().fg(Color::Yellow)),
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Percentage(30),
            Constraint::Percentage(50),
            Constraint::Length(10),
        ],
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
                    " Selected: {} items ({}) ",
                    app.selected_count(),
                    scan::format_size(app.selected_size())
                ),
                Style::default().fg(Color::Green).bold(),
            ),
            Span::styled(
                " [Enter] Clean  [a] Toggle all  [q] Quit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(Span::styled(
            " [Space] Select  [a] All  [j/k] Navigate  [q] Quit",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(status, chunks[2]);

    if app.mode == Mode::Confirm {
        let popup_area = centered_rect(50, 7, area);
        f.render_widget(Clear, popup_area);

        let popup = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "Clean {} items ({})?",
                    app.selected_count(),
                    scan::format_size(app.selected_size())
                ),
                Style::default().fg(Color::Yellow).bold(),
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
        f.render_widget(popup, popup_area);
    }
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

    for result in &app.results {
        lines.push(result_line(result));
    }

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Progress ")
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(paragraph, area);
}

fn render_done(f: &mut Frame, app: &App) {
    let area = f.area();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Cleanup Complete",
            Style::default().fg(Color::Green).bold(),
        )),
        Line::from(""),
    ];

    for result in &app.results {
        lines.push(result_line(result));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Total freed: ", Style::default().bold()),
        Span::styled(
            scan::format_size(app.total_freed()),
            Style::default().fg(Color::Green).bold(),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press q to exit",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(Style::default().fg(Color::Green)),
    );
    f.render_widget(paragraph, area);
}

fn result_line(result: &CleanResult) -> Line<'_> {
    match &result.outcome {
        Ok(bytes) => Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(Color::Green)),
            Span::styled(result.name.as_str(), Style::default().fg(Color::White)),
            Span::styled(
                format!("  {}", scan::format_size(*bytes)),
                Style::default().fg(Color::Green),
            ),
        ]),
        Err(e) => Line::from(vec![
            Span::styled("  ✗ ", Style::default().fg(Color::Red)),
            Span::styled(result.name.as_str(), Style::default().fg(Color::White)),
            Span::styled(format!("  {e}"), Style::default().fg(Color::Red)),
        ]),
    }
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let y = area.height.saturating_sub(height) / 2;
    let width = area.width * percent_x / 100;
    let x = (area.width.saturating_sub(width)) / 2;
    Rect::new(x, y, width, height)
}
