use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Row, Table, TableState},
};
use share_engine::Tab;

pub fn pick(tabs: &[Tab]) -> Result<Option<usize>> {
    let mut state = TableState::default().with_selected(0);
    let mut picked = None;
    super::run(|terminal| {
        loop {
            terminal.draw(|f| render(f, tabs, &mut state))?;
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
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Enter => {
                    picked = state.selected();
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => {
                    state.select(state.selected().map(|i| (i + 1).min(tabs.len() - 1)))
                }
                _ => {}
            }
        }
    })?;
    Ok(picked)
}

fn render(f: &mut Frame, tabs: &[Tab], state: &mut TableState) {
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    let header = Paragraph::new(Line::from(Span::styled(
        "  wsctl share  pick a tab",
        Style::default().fg(Color::Cyan).bold(),
    )))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    let rows = tabs.iter().map(|tab| {
        Row::new(vec![
            tab.session_label.clone(),
            tab.window_label.clone(),
            tab.title.clone(),
        ])
    });
    let widths = [
        Constraint::Length(24),
        Constraint::Length(24),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["session", "tab", "title"]).style(Style::default().fg(Color::DarkGray)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray).bold())
        .block(Block::default().padding(Padding::horizontal(1)));
    f.render_stateful_widget(table, chunks[1], state);

    let footer =
        Paragraph::new(" ↑↓ move  enter share  q quit").style(Style::default().fg(Color::DarkGray));
    f.render_widget(footer, chunks[2]);
}
