use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use disk::audit::{self, Category};
use disk::scan::{ScanResult, scan_dir};
use disk::util::format_size;
use ratatui::{prelude::*, widgets::*};

struct App {
    categories: Vec<Category>,
    selectable: Vec<(usize, usize)>,
    cache: HashMap<PathBuf, ScanResult>,
    stack: Vec<View>,
}

enum View {
    Categories {
        cursor: usize,
    },
    Directory {
        path: PathBuf,
        cursor: usize,
        crumb: String,
    },
}

impl App {
    fn new(categories: Vec<Category>) -> Self {
        let mut selectable = Vec::new();
        for (ci, cat) in categories.iter().enumerate() {
            for (pi, _) in cat.paths.iter().enumerate() {
                selectable.push((ci, pi));
            }
        }
        Self {
            categories,
            selectable,
            cache: HashMap::new(),
            stack: vec![View::Categories { cursor: 0 }],
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

    fn move_up(&mut self) {
        match self.current_mut() {
            View::Categories { cursor } => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
            View::Directory { cursor, .. } => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
        }
    }

    fn move_down(&mut self) {
        let max = match self.current() {
            View::Categories { .. } => self.selectable.len().saturating_sub(1),
            View::Directory { path, .. } => {
                let count = self.cache.get(path).map(|s| s.children.len()).unwrap_or(0);
                count.saturating_sub(1)
            }
        };
        match self.current_mut() {
            View::Categories { cursor } | View::Directory { cursor, .. } => {
                if *cursor < max {
                    *cursor += 1;
                }
            }
        }
    }

    fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }
}

pub fn run() -> Result<()> {
    let categories = audit::scan_categories();
    let mut app = App::new(categories);

    super::run(|terminal| event_loop(&mut app, terminal))
}

fn event_loop(app: &mut App, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    // Prime cache for the initial directory view if entered directly later
    loop {
        terminal.draw(|f| render(f, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    drill_in(app, terminal)?;
                }
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    app.pop();
                }
                _ => {}
            }
        }
    }
}

fn drill_in(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let (next_path, next_crumb) = match app.current() {
        View::Categories { cursor } => {
            let Some(&(ci, pi)) = app.selectable.get(*cursor) else {
                return Ok(());
            };
            let cat = &app.categories[ci];
            let path = cat.paths[pi].path.clone();
            let crumb = format!("{} > {}", cat.name, cat.paths[pi].label);
            (path, crumb)
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

fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled("  wsctl disk audit", Style::default().fg(Color::Cyan).bold()),
        Span::styled("  ", Style::default()),
        Span::styled(app.breadcrumb(), Style::default().fg(Color::White)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    match app.current() {
        View::Categories { cursor } => render_categories(f, chunks[1], chunks[2], app, *cursor),
        View::Directory { path, cursor, .. } => {
            render_directory(f, chunks[1], chunks[2], app, path, *cursor)
        }
    }
}

fn render_categories(f: &mut Frame, body: Rect, footer: Rect, app: &App, cursor: usize) {
    let selected_pair = app.selectable.get(cursor).copied();

    let mut rows: Vec<Row> = Vec::new();
    for (ci, cat) in app.categories.iter().enumerate() {
        rows.push(Row::new(vec![
            Cell::from(format_size(cat.total_size))
                .style(Style::default().fg(Color::Yellow).bold()),
            Cell::from(cat.name.as_str()).style(Style::default().fg(Color::White).bold()),
        ]));

        for (pi, p) in cat.paths.iter().enumerate() {
            let is_cursor = selected_pair == Some((ci, pi));
            let row_style = if is_cursor {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            rows.push(
                Row::new(vec![
                    Cell::from(format!("  {}", format_size(p.size)))
                        .style(Style::default().fg(Color::DarkGray)),
                    Cell::from(format!("  {}", p.label))
                        .style(Style::default().fg(Color::DarkGray)),
                ])
                .style(row_style),
            );
        }
    }

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Min(1)]).block(
        Block::default()
            .borders(Borders::NONE)
            .padding(Padding::horizontal(1)),
    );
    f.render_widget(table, body);

    let total: u64 = app.categories.iter().map(|c| c.total_size).sum();
    let footer_line = Line::from(vec![
        Span::styled(
            format!(" Total: {} ", format_size(total)),
            Style::default().fg(Color::Green).bold(),
        ),
        Span::styled(
            " [j/k] Navigate  [Enter] Open  [q] Quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(footer_line, footer);
}

fn render_directory(
    f: &mut Frame,
    body: Rect,
    footer: Rect,
    app: &App,
    path: &Path,
    cursor: usize,
) {
    let Some(scan) = app.cache.get(path) else {
        let p = Paragraph::new("  Scanning...")
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(p, body);
        return;
    };

    if scan.children.is_empty() {
        let p = Paragraph::new("  (empty)")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::NONE).padding(Padding::horizontal(1)));
        f.render_widget(p, body);
    } else {
        let rows: Vec<Row> = scan
            .children
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let row_style = if i == cursor {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };
                let kind_marker = if c.is_dir { "📁" } else { "  " };
                let name_color = if c.is_dir { Color::Cyan } else { Color::White };
                Row::new(vec![
                    Cell::from(format_size(c.size)).style(Style::default().fg(Color::Yellow)),
                    Cell::from(kind_marker),
                    Cell::from(c.name.as_str()).style(Style::default().fg(name_color)),
                ])
                .style(row_style)
            })
            .collect();

        let table = Table::new(
            rows,
            [Constraint::Length(12), Constraint::Length(3), Constraint::Min(1)],
        )
        .block(
            Block::default()
                .borders(Borders::NONE)
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(table, body);
    }

    let footer_line = Line::from(vec![
        Span::styled(
            format!(" Total: {} ", format_size(scan.total_size)),
            Style::default().fg(Color::Green).bold(),
        ),
        Span::styled(
            " [j/k] Navigate  [Enter] Open  [Esc] Back  [q] Quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(footer_line, footer);
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
