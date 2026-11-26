use std::io::{self, Write};

use anyhow::{Result, bail};
use crossterm::{
    cursor::{MoveToColumn, MoveUp},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};

const HINT: &str = "Tab completes, ↑↓ picks another, comma adds more, Enter is done";

pub fn ask(
    prompt: &str,
    known: &[String],
    taken: &[String],
    allow_empty: bool,
) -> Result<Vec<String>> {
    terminal::enable_raw_mode()?;
    let mut input = Input {
        known,
        taken,
        chosen: Vec::new(),
        typed: String::new(),
        pick: 0,
        error: None,
    };
    let result = input.run(prompt, allow_empty);
    terminal::disable_raw_mode()?;
    let mut out = io::stdout();
    queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
    if let Ok(chosen) = &result {
        queue!(out, Print(format!("{prompt}{}\r\n", chosen.join(", "))))?;
    }
    out.flush()?;
    result
}

struct Input<'a> {
    known: &'a [String],
    taken: &'a [String],
    chosen: Vec<String>,
    typed: String,
    pick: usize,
    error: Option<String>,
}

impl Input<'_> {
    fn run(&mut self, prompt: &str, allow_empty: bool) -> Result<Vec<String>> {
        loop {
            self.draw(prompt)?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            self.error = None;
            match key.code {
                KeyCode::Char('c') if control => bail!("cancelled"),
                KeyCode::Esc => bail!("cancelled"),
                KeyCode::Char(',' | ' ') => self.commit(),
                KeyCode::Char(character) => {
                    self.typed.push(character);
                    self.pick = 0;
                }
                KeyCode::Tab | KeyCode::Right => self.accept(),
                KeyCode::Down => self.pick += 1,
                KeyCode::Up => self.pick = self.pick.saturating_sub(1),
                KeyCode::Backspace => {
                    if self.typed.pop().is_none() {
                        self.typed = self.chosen.pop().unwrap_or_default();
                    }
                    self.pick = 0;
                }
                KeyCode::Enter => {
                    match self.suggestion() {
                        Some(_) => self.accept(),
                        None => self.commit(),
                    }
                    if self.error.is_some() {
                        continue;
                    }
                    if self.chosen.is_empty() && !allow_empty {
                        self.error = Some("type at least one email".to_string());
                        continue;
                    }
                    return Ok(std::mem::take(&mut self.chosen));
                }
                _ => {}
            }
        }
    }

    fn available(&self) -> impl Iterator<Item = &String> {
        self.known
            .iter()
            .filter(|email| !self.chosen.contains(email) && !self.taken.contains(email))
    }

    fn completions(&self) -> Vec<&String> {
        let typed = self.typed.to_lowercase();
        if typed.is_empty() {
            return Vec::new();
        }
        self.available()
            .filter(|email| email.starts_with(&typed) && **email != typed)
            .collect()
    }

    fn suggestion(&self) -> Option<&String> {
        let completions = self.completions();
        (!completions.is_empty()).then(|| completions[self.pick % completions.len()])
    }

    fn accept(&mut self) {
        if let Some(email) = self.suggestion().cloned() {
            self.typed = email;
            self.commit();
        }
    }

    fn commit(&mut self) {
        let email = self.typed.trim().to_lowercase();
        if email.is_empty() {
            return;
        }
        if !email.contains('@') {
            self.error = Some(format!("{email} is not an email"));
            return;
        }
        if !self.chosen.contains(&email) {
            self.chosen.push(email);
        }
        self.typed.clear();
        self.pick = 0;
    }

    fn draw(&self, prompt: &str) -> Result<()> {
        let width = usize::from(terminal::size()?.0).max(20) - 1;
        let mut body = self.chosen.join(", ");
        if !body.is_empty() {
            body.push_str(", ");
        }
        body.push_str(&self.typed);
        let ghost = self
            .suggestion()
            .map(|email| email[self.typed.len()..].to_string())
            .unwrap_or_default();
        let room = width.saturating_sub(prompt.len() + ghost.len()).max(8);
        let shown = match body.chars().count() > room {
            true => {
                let tail: String = body.chars().skip(body.chars().count() + 1 - room).collect();
                format!("…{tail}")
            }
            false => body,
        };
        let ghost: String = ghost
            .chars()
            .take(width.saturating_sub(prompt.len() + shown.chars().count()))
            .collect();
        let cursor = prompt.len() + shown.chars().count();
        let (hint, color) = match &self.error {
            Some(error) => (error.clone(), Color::Red),
            None => (self.hint(), Color::DarkGrey),
        };
        let hint: String = hint.chars().take(width).collect();
        let mut out = io::stdout();
        queue!(
            out,
            MoveToColumn(0),
            Clear(ClearType::FromCursorDown),
            Print(prompt),
            Print(&shown),
            SetForegroundColor(Color::DarkGrey),
            Print(ghost),
            ResetColor,
            Print("\r\n"),
            SetForegroundColor(color),
            Print(hint),
            ResetColor,
            MoveUp(1),
            MoveToColumn(u16::try_from(cursor.min(width)).unwrap_or(0)),
        )?;
        out.flush()?;
        Ok(())
    }

    fn hint(&self) -> String {
        let matches: Vec<&String> = match self.typed.is_empty() {
            true => self.available().collect(),
            false => self.completions(),
        };
        match matches.is_empty() {
            true => HINT.to_string(),
            false => {
                let listed: Vec<&str> =
                    matches.iter().take(4).map(|email| email.as_str()).collect();
                let more = matches.len().saturating_sub(4);
                let more = if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                };
                format!("known: {}{more}   {HINT}", listed.join("  "))
            }
        }
    }
}
