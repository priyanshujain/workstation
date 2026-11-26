use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};
use share_protocol::{GuestCredential, ManageReply, ManageRequest, Mode, SharedTab};

const REFRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq)]
enum View {
    Tabs,
    Guests,
}

#[derive(Clone, PartialEq)]
enum Item {
    Tab(String),
    TabGuest(String, String),
    Guest(String),
    GuestTab(String, String),
}

enum Pending {
    Confirm(String, ManageRequest),
    Password(GuestCredential),
}

struct Screen {
    view: View,
    guests: Vec<String>,
    shares: Vec<SharedTab>,
    items: Vec<Item>,
    table: TableState,
    message: String,
    pending: Option<Pending>,
}

pub fn manage(mut call: impl FnMut(ManageRequest) -> Result<ManageReply>) -> Result<()> {
    let mut screen = Screen {
        view: View::Tabs,
        guests: Vec::new(),
        shares: Vec::new(),
        items: Vec::new(),
        table: TableState::default().with_selected(0),
        message: String::new(),
        pending: None,
    };
    super::run(|terminal| {
        let mut fetched = None::<Instant>;
        loop {
            if fetched.is_none_or(|at| at.elapsed() >= REFRESH) {
                screen.refresh(&mut call);
                fetched = Some(Instant::now());
            }
            terminal.draw(|f| screen.render(f))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if screen.key(key.code, &mut call) {
                return Ok(());
            }
            if screen.message.is_empty() {
                continue;
            }
            fetched = None;
        }
    })
}

impl Screen {
    fn refresh(&mut self, call: &mut impl FnMut(ManageRequest) -> Result<ManageReply>) {
        match call(ManageRequest::State) {
            Ok(ManageReply::State { guests, shares }) => {
                self.guests = guests;
                self.shares = shares;
            }
            Ok(ManageReply::Failed { reason }) => self.message = format!("relay: {reason}"),
            Ok(_) => self.message = "the relay sent an unexpected reply".to_string(),
            Err(error) => self.message = format!("{error:#}"),
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let selected = self.selected().cloned();
        self.items = match self.view {
            View::Tabs => self.tab_items(),
            View::Guests => self.guest_items(),
        };
        let index = selected
            .and_then(|item| self.items.iter().position(|candidate| *candidate == item))
            .or(self.table.selected())
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        self.table.select(Some(index));
    }

    fn tab_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for tab in &self.shares {
            items.push(Item::Tab(tab.share_id.clone()));
            for guest in &tab.share.guests {
                items.push(Item::TabGuest(tab.share_id.clone(), guest.clone()));
            }
        }
        items
    }

    fn guest_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for guest in &self.guests {
            items.push(Item::Guest(guest.clone()));
            for tab in self.tabs_of(guest) {
                items.push(Item::GuestTab(guest.clone(), tab.share_id.clone()));
            }
        }
        items
    }

    fn tabs_of<'a>(&'a self, guest: &'a str) -> impl Iterator<Item = &'a SharedTab> {
        self.shares
            .iter()
            .filter(move |tab| tab.share.guests.iter().any(|email| email == guest))
    }

    fn tab(&self, share_id: &str) -> Option<&SharedTab> {
        self.shares.iter().find(|tab| tab.share_id == share_id)
    }

    fn selected(&self) -> Option<&Item> {
        self.items.get(self.table.selected()?)
    }

    fn key(
        &mut self,
        code: KeyCode,
        call: &mut impl FnMut(ManageRequest) -> Result<ManageReply>,
    ) -> bool {
        match self.pending.take() {
            Some(Pending::Confirm(_, request)) => {
                if matches!(code, KeyCode::Char('y') | KeyCode::Enter) {
                    self.run(request, call);
                } else {
                    self.message = "Nothing changed.".to_string();
                }
                return false;
            }
            Some(Pending::Password(_)) => return false,
            None => {}
        }
        self.message.clear();
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                self.view = match self.view {
                    View::Tabs => View::Guests,
                    View::Guests => View::Tabs,
                };
                self.table.select(Some(0));
                self.rebuild();
            }
            KeyCode::Up | KeyCode::Char('k') => self.table.select_previous(),
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.items.len().saturating_sub(1);
                self.table
                    .select(self.table.selected().map(|index| (index + 1).min(last)));
            }
            KeyCode::Char(action) => self.ask(action),
            _ => {}
        }
        false
    }

    fn ask(&mut self, action: char) {
        let Some(item) = self.selected().cloned() else {
            return;
        };
        let guest_of = |item: &Item| match item {
            Item::Guest(email) | Item::GuestTab(email, _) | Item::TabGuest(_, email) => {
                Some(email.clone())
            }
            Item::Tab(_) => None,
        };
        let request = match (action, &item) {
            ('x', Item::Tab(id) | Item::GuestTab(_, id) | Item::TabGuest(id, _)) => Some((
                format!("Stop sharing {}?", self.tab_name(id)),
                ManageRequest::StopShare {
                    share_id: id.clone(),
                },
            )),
            ('d', Item::TabGuest(id, email) | Item::GuestTab(email, id)) => Some((
                format!(
                    "Stop sharing {} with {email}?{}",
                    self.tab_name(id),
                    self.stops(email, Some(id))
                ),
                ManageRequest::RemoveGuest {
                    email: email.clone(),
                    share_id: Some(id.clone()),
                },
            )),
            ('d', Item::Guest(email)) => Some((
                format!(
                    "Stop sharing all {} tabs with {email}?{}",
                    self.tabs_of(email).count(),
                    self.stops(email, None)
                ),
                ManageRequest::RemoveGuest {
                    email: email.clone(),
                    share_id: None,
                },
            )),
            ('r', item) => guest_of(item).map(|email| {
                (
                    format!("Make a new password for {email}? The old one stops working."),
                    ManageRequest::ResetPassword { email },
                )
            }),
            ('f', item) => guest_of(item).map(|email| {
                (
                    format!("Forget {email}? They lose every tab and their password."),
                    ManageRequest::ForgetGuest { email },
                )
            }),
            _ => None,
        };
        if let Some((question, request)) = request {
            self.pending = Some(Pending::Confirm(question, request));
        }
    }

    fn run(
        &mut self,
        request: ManageRequest,
        call: &mut impl FnMut(ManageRequest) -> Result<ManageReply>,
    ) {
        let done = match &request {
            ManageRequest::StopShare { .. } => "Sharing stopped.".to_string(),
            ManageRequest::RemoveGuest { email, .. } => format!("{email} no longer has access."),
            ManageRequest::ForgetGuest { email } => format!("Forgot {email}."),
            _ => String::new(),
        };
        self.message = match call(request) {
            Ok(ManageReply::Credentials {
                mut credentials, ..
            }) if !credentials.is_empty() => {
                self.pending = Some(Pending::Password(credentials.remove(0)));
                "New password made.".to_string()
            }
            Ok(ManageReply::Done) => done,
            Ok(ManageReply::Failed { reason }) => format!("Failed: {reason}"),
            Ok(_) => "the relay sent an unexpected reply".to_string(),
            Err(error) => format!("Failed: {error:#}"),
        };
    }

    fn stops(&self, email: &str, share_id: Option<&str>) -> String {
        let only_guest = self
            .tabs_of(email)
            .filter(|tab| share_id.is_none_or(|id| id == tab.share_id))
            .filter(|tab| tab.share.guests.len() == 1)
            .count();
        match only_guest {
            0 => String::new(),
            1 => format!(" {email} is the only guest, so sharing stops."),
            count => format!(" Sharing stops on {count} tabs where {email} is the only guest."),
        }
    }

    fn tab_name(&self, share_id: &str) -> String {
        self.tab(share_id)
            .map(|tab| format!("{} / {}", tab.share.session_label, tab.share.label))
            .unwrap_or_else(|| "this tab".to_string())
    }

    fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(f.area());
        f.render_widget(self.header(), chunks[0]);
        if self.items.is_empty() {
            let empty = match self.view {
                View::Tabs => "  No tab is shared. Use Share Tab on a tab to share it.",
                View::Guests => "  No guests yet. Share a tab to add one.",
            };
            f.render_widget(Paragraph::new(empty).fg(Color::DarkGray), chunks[1]);
        } else {
            let table = self.table();
            f.render_stateful_widget(table, chunks[1], &mut self.table);
        }
        let (message, color) = match &self.pending {
            Some(Pending::Confirm(question, _)) => (format!(" {question}  y/n"), Color::Yellow),
            _ => (format!(" {}", self.message), Color::Green),
        };
        f.render_widget(
            Paragraph::new(message)
                .fg(color)
                .bold()
                .wrap(Wrap { trim: false }),
            chunks[2],
        );
        f.render_widget(Paragraph::new(self.footer()).fg(Color::DarkGray), chunks[3]);
        if let Some(Pending::Password(credential)) = &self.pending {
            password_popup(f, credential);
        }
    }

    fn header(&self) -> Paragraph<'static> {
        let tab = |label: &'static str, view: View| match self.view == view {
            true => Span::styled(
                format!(" {label} "),
                Style::default().fg(Color::Black).bg(Color::Cyan).bold(),
            ),
            false => Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray)),
        };
        Paragraph::new(Line::from(vec![
            Span::styled("  Shared Tabs  ", Style::default().fg(Color::Cyan).bold()),
            tab("tabs", View::Tabs),
            Span::raw(" "),
            tab("guests", View::Guests),
        ]))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
    }

    fn table(&self) -> Table<'static> {
        let rows = self.items.iter().map(|item| self.row(item));
        let (headers, widths) = match self.view {
            View::Tabs => (
                vec!["tab", "host", "mode", "watching", "ends in"],
                vec![
                    Constraint::Min(24),
                    Constraint::Length(12),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Length(9),
                ],
            ),
            View::Guests => (
                vec!["guest", "tabs", "watching"],
                vec![
                    Constraint::Min(24),
                    Constraint::Length(6),
                    Constraint::Length(10),
                ],
            ),
        };
        Table::new(rows, widths)
            .header(Row::new(headers).style(Style::default().fg(Color::DarkGray)))
            .row_highlight_style(Style::default().bg(Color::DarkGray).bold())
            .block(Block::default().padding(Padding::horizontal(1)))
    }

    fn row(&self, item: &Item) -> Row<'static> {
        match item {
            Item::Tab(id) => {
                let Some(tab) = self.tab(id) else {
                    return Row::new(vec![String::new()]);
                };
                let mode = match tab.share.mode {
                    Mode::View => "view",
                    Mode::Input => "input",
                };
                Row::new(vec![
                    self.tab_name(id),
                    tab.share.host.clone(),
                    mode.to_string(),
                    tab.watching.len().to_string(),
                    ends_in(tab.share.expires_at_unix),
                ])
                .bold()
            }
            Item::TabGuest(id, email) => {
                let watching = self.tab(id).is_some_and(|tab| tab.watching.contains(email));
                Row::new(vec![
                    format!("    {email}"),
                    String::new(),
                    String::new(),
                    if watching { "yes" } else { "" }.to_string(),
                ])
                .fg(Color::Gray)
            }
            Item::Guest(email) => {
                let tabs = self.tabs_of(email).count();
                let watching = self
                    .tabs_of(email)
                    .filter(|tab| tab.watching.contains(email))
                    .count();
                Row::new(vec![email.clone(), tabs.to_string(), watching.to_string()]).bold()
            }
            Item::GuestTab(email, id) => {
                let host = self
                    .tab(id)
                    .map(|tab| tab.share.host.clone())
                    .unwrap_or_default();
                let watching = self.tab(id).is_some_and(|tab| tab.watching.contains(email));
                Row::new(vec![
                    format!("    {} on {host}", self.tab_name(id)),
                    String::new(),
                    if watching { "yes" } else { "" }.to_string(),
                ])
                .fg(Color::Gray)
            }
        }
    }

    fn footer(&self) -> String {
        let actions = match self.selected() {
            Some(Item::Tab(_)) => "x stop sharing",
            Some(Item::TabGuest(..)) | Some(Item::GuestTab(..)) => {
                "d remove from tab  x stop tab  r new password  f forget"
            }
            Some(Item::Guest(_)) => "d remove from all tabs  r new password  f forget",
            None => "",
        };
        format!(" tab view  {actions}  q close")
    }
}

fn password_popup(f: &mut Frame, credential: &GuestCredential) {
    let area = centered(f.area(), 64, 7);
    let text = vec![
        Line::from(vec![
            Span::raw("Guest     "),
            Span::raw(credential.email.clone()).bold(),
        ]),
        Line::from(vec![
            Span::raw("Password  "),
            Span::raw(credential.password.clone())
                .fg(Color::Yellow)
                .bold(),
        ]),
        Line::raw(""),
        Line::from("This password shows only once. Press any key to close.").fg(Color::DarkGray),
    ];
    let popup = Paragraph::new(text).wrap(Wrap { trim: false }).block(
        Block::default()
            .title(" New password ")
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(Clear, area);
    f.render_widget(popup, area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

pub fn ends_in(expires_at_unix: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let left = expires_at_unix.saturating_sub(now);
    format!("{}:{:02}:{:02}", left / 3600, left / 60 % 60, left % 60)
}
