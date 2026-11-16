use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use share_engine::{ShareEvent, ShareRequest, Tab};
use share_protocol::Mode;

const MARKER: &str = "shared: ";

pub struct Target {
    pub block: Option<String>,
    pub session: Option<String>,
    pub mode: Option<Mode>,
    pub guests: Vec<String>,
    pub ttl: Option<String>,
}

#[derive(Deserialize)]
struct Config {
    relay_url: String,
    secret_file: PathBuf,
}

pub fn share(target: Target) -> Result<()> {
    let config = load_config()?;
    let secret = read_secret(&config.secret_file)?;
    let (session_id, block_id) = match (target.session, target.block) {
        (Some(session), Some(block)) => (session, block),
        _ => pick_tab()?,
    };
    let mode = match target.mode {
        Some(mode) => mode,
        None => ask_mode()?,
    };
    let guests = match target.guests.is_empty() {
        true => ask_guests()?,
        false => target.guests,
    };
    let ttl_secs = match target.ttl {
        Some(ttl) => parse_ttl(&ttl)?,
        None => ask_ttl()?,
    };

    let marker = Marker::set(&session_id, &block_id)?;
    end_on_hangup();
    let request = ShareRequest {
        session_id,
        block_id,
        mode,
        guests: guests.clone(),
        ttl_secs,
        relay_url: config.relay_url,
        secret,
    };
    let pane = Pane::start(mode, guests);
    let result = share_engine::run(request, |event| pane.handle(event));
    pane.stop();
    marker.restore();
    result
}

pub fn relay(mut config: share_relay::RelayConfig, secret_file: &Path) -> Result<()> {
    config.secret = read_secret(secret_file)?;
    share_relay::serve(config)
}

fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("wsctl/share.json")
}

fn load_config() -> Result<Config> {
    let path = config_path();
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {}; declare services.wsctl.share",
            path.display()
        )
    })?;
    serde_json::from_str(&raw).with_context(|| format!("could not parse {}", path.display()))
}

fn read_secret(path: &Path) -> Result<String> {
    let secret = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(secret.trim().to_string())
}

fn pick_tab() -> Result<(String, String)> {
    let tabs = share_engine::list_tabs()?;
    if tabs.is_empty() {
        bail!("no Rex tabs to share");
    }
    let Some(index) = crate::tui::share::pick(&tabs)? else {
        bail!("nothing picked");
    };
    let Tab {
        session_id,
        block_id,
        ..
    } = &tabs[index];
    Ok((session_id.clone(), block_id.clone()))
}

fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        bail!("no answer");
    }
    Ok(line.trim().to_string())
}

fn ask_mode() -> Result<Mode> {
    loop {
        match ask("Guests may [v]iew or also [i]nput? ")?.as_str() {
            "v" | "view" => return Ok(Mode::View),
            "i" | "input" => return Ok(Mode::Input),
            _ => continue,
        }
    }
}

fn ask_guests() -> Result<Vec<String>> {
    loop {
        let guests: Vec<String> = ask("Guest emails, separated by spaces or commas: ")?
            .split([' ', ','])
            .filter(|email| !email.is_empty())
            .map(str::to_string)
            .collect();
        if !guests.is_empty() && guests.iter().all(|email| email.contains('@')) {
            return Ok(guests);
        }
    }
}

fn ask_ttl() -> Result<u64> {
    loop {
        let answer = ask("Expires after [1h]: ")?;
        match parse_ttl(if answer.is_empty() { "1h" } else { &answer }) {
            Ok(secs) => return Ok(secs),
            Err(error) => eprintln!("{error}"),
        }
    }
}

fn parse_ttl(text: &str) -> Result<u64> {
    let split = text.len().saturating_sub(1);
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .with_context(|| format!("bad TTL {text:?}; use a number and s, m, h or d"))?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("bad TTL {text:?}; use a number and s, m, h or d"),
    };
    Ok(number * scale)
}

#[derive(Default)]
struct PaneState {
    expires_at_unix: Option<u64>,
    viewers: Vec<String>,
    last_input: Option<String>,
    ended: Option<String>,
}

struct Pane {
    state: Arc<Mutex<PaneState>>,
    mode: Mode,
    guests: Vec<String>,
}

impl Pane {
    fn start(mode: Mode, guests: Vec<String>) -> Self {
        let state = Arc::new(Mutex::new(PaneState::default()));
        let ticking = state.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let state = ticking.lock().unwrap();
                if state.ended.is_some() {
                    break;
                }
                redraw(&state);
            }
        });
        Self {
            state,
            mode,
            guests,
        }
    }

    fn handle(&self, event: ShareEvent) {
        let mut state = self.state.lock().unwrap();
        match event {
            ShareEvent::Ready {
                url,
                expires_at_unix,
            } => {
                announce(&url, self.mode, &self.guests);
                state.expires_at_unix = Some(expires_at_unix);
            }
            ShareEvent::ViewerJoined { email } => state.viewers.push(email),
            ShareEvent::ViewerLeft { email } => {
                if let Some(index) = state.viewers.iter().position(|viewer| *viewer == email) {
                    state.viewers.remove(index);
                }
            }
            ShareEvent::Input { email } => state.last_input = Some(email),
            ShareEvent::Ended { reason } => {
                redraw(&state);
                println!("\nShare ended: {reason}");
                state.ended = Some(reason);
                return;
            }
        }
        redraw(&state);
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        if state.ended.is_none() {
            state.ended = Some(String::new());
            println!();
        }
    }
}

fn redraw(state: &PaneState) {
    let Some(expires_at_unix) = state.expires_at_unix else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let left = expires_at_unix.saturating_sub(now);
    let viewers = match state.viewers.is_empty() {
        true => "none".to_string(),
        false => state.viewers.join(", "),
    };
    let input = match &state.last_input {
        Some(email) => format!("  last input: {email}"),
        None => String::new(),
    };
    print!(
        "\r\x1b[KViewers: {viewers}  expires in {:02}:{:02}:{:02}{input}",
        left / 3600,
        left / 60 % 60,
        left % 60
    );
    let _ = io::stdout().flush();
}

fn announce(url: &str, mode: Mode, guests: &[String]) {
    println!("\n{url}\n");
    println!("Mode:    {mode:?}");
    println!("Guests:  {}", guests.join(", "));
    println!("\nClose this pane or run Stop Sharing to end the share.\n");
    let _ = io::stdout().flush();
}

fn end_on_hangup() {
    let mut hangup: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut hangup);
        libc::sigaddset(&mut hangup, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &hangup, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        let mut signal = 0;
        unsafe {
            libc::sigwait(&hangup, &mut signal);
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    });
}

struct Marker {
    session_id: String,
    window_id: String,
    label: String,
}

impl Marker {
    fn set(session_id: &str, block_id: &str) -> Result<Self> {
        let view = rex_call("session.view", json!({ "session_id": session_id }))?;
        let window = window_holding(&view, block_id)
            .with_context(|| format!("block {block_id} is in no window of {session_id}"))?;
        let marker = Marker {
            session_id: session_id.to_string(),
            window_id: window["window_id"].as_str().unwrap_or_default().to_string(),
            label: original_label(window),
        };
        let shown = window["label"].as_str().unwrap_or_default();
        marker.label_window(&format!("{MARKER}{shown}"))?;
        Ok(marker)
    }

    fn restore(&self) {
        let _ = self.label_window(&self.label);
    }

    fn label_window(&self, label: &str) -> Result<()> {
        let label: String = label.chars().take(128).collect();
        let args =
            json!({ "session_id": self.session_id, "window_id": self.window_id, "label": label });
        rex_call("session.set_window_label", args).map(drop)
    }
}

fn window_holding<'a>(view: &'a Value, block_id: &str) -> Option<&'a Value> {
    view["windows"].as_array()?.iter().find(|window| {
        window["layers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|layer| {
                layer["blocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|block| block["block_id"] == block_id)
            })
    })
}

fn original_label(window: &Value) -> String {
    let id = window["window_id"].as_str().unwrap_or_default();
    let label = window["label"].as_str().unwrap_or_default();
    let label = label.strip_prefix(MARKER).unwrap_or(label);
    let default = format!("Window {}", &id[id.len().saturating_sub(8)..]);
    match label == default {
        true => String::new(),
        false => label.to_string(),
    }
}

fn rex_call(method: &str, args: Value) -> Result<Value> {
    let output = Command::new("rex")
        .args(["api", "call", method, &args.to_string()])
        .output()
        .context("could not run rex")?;
    if !output.status.success() {
        bail!(
            "rex api call {method}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.trim().is_empty() {
        true => Ok(Value::Null),
        false => Ok(serde_json::from_str(&stdout)?),
    }
}
