use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use share_engine::{ShareEvent, ShareRequest, Tab};
use share_protocol::{GuestCredential, ManageReply, ManageRequest, Mode, SharedTab};
use std::os::unix::fs::OpenOptionsExt;

const MARKER: &str = "shared: ";
const DEFAULT_TTL: &str = "1h";

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
    let relay = Relay::connect()?;
    let (session_id, block_id) = match (target.session, target.block) {
        (Some(session), Some(block)) => (session, block),
        _ => pick_tab()?,
    };
    let (known, shares) = relay.state()?;
    if let Some(shared) = shares.iter().find(|tab| tab.share.block_id == block_id) {
        return add_guests(&relay, shared, target.guests, &known);
    }
    let mode = match target.mode {
        Some(mode) => mode,
        None => ask_mode()?,
    };
    let guests = match target.guests.is_empty() {
        true => ask_guests("Share with: ", &known, &[], false)?,
        false => target.guests,
    };
    let ttl_secs = match target.ttl {
        Some(ttl) => parse_ttl(&ttl)?,
        None => ask_ttl()?,
    };
    let started = start_background(&session_id, &block_id, mode, &guests, ttl_secs)?;
    println!();
    println!("Shared {}  ({})", started.name, mode_text(mode));
    println!("Link   {}", started.url);
    print_access(&started.credentials, &started.existing);
    println!(
        "Ends in {}. The share keeps running when this window closes.",
        ends_in(started.expires_at_unix)
    );
    println!("Stop it with Stop Sharing. Manage guests and tabs with Shared Tabs.\n");
    wait_for_key()
}

fn add_guests(
    relay: &Relay,
    shared: &SharedTab,
    guests: Vec<String>,
    known: &[String],
) -> Result<()> {
    println!(
        "{} is already shared ({}).",
        tab_title(shared),
        mode_text(shared.share.mode)
    );
    println!("Guests: {}\n", shared.share.guests.join(", "));
    let guests = match guests.is_empty() {
        true => ask_guests(
            "Add guests (Enter for none): ",
            known,
            &shared.share.guests,
            true,
        )?,
        false => guests,
    };
    if guests.is_empty() {
        println!("Nothing changed.\n");
        return wait_for_key();
    }
    let request = ManageRequest::AddGuests {
        share_id: shared.share_id.clone(),
        guests,
    };
    let ManageReply::Credentials {
        credentials,
        existing,
    } = relay.call(request)?
    else {
        bail!("the relay sent an unexpected reply");
    };
    println!("\nLink   {}", shared.url);
    print_access(&credentials, &existing);
    println!();
    wait_for_key()
}

fn print_access(credentials: &[GuestCredential], existing: &[String]) {
    if !credentials.is_empty() {
        println!("\nNew guests. Send each their password; it shows only once:");
        let width = credentials.iter().map(|c| c.email.len()).max().unwrap_or(0);
        for credential in credentials {
            println!("  {:width$}  {}", credential.email, credential.password);
        }
    }
    if !existing.is_empty() {
        println!("\nThese guests use their current password:");
        for email in existing {
            println!("  {email}");
        }
    }
    println!();
}

pub fn ends_in(expires_at_unix: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let left = expires_at_unix.saturating_sub(now);
    format!("{}:{:02}:{:02}", left / 3600, left / 60 % 60, left % 60)
}

fn mode_text(mode: Mode) -> &'static str {
    match mode {
        Mode::View => "view only",
        Mode::Input => "guests can type",
    }
}

struct Relay {
    url: String,
    secret: String,
}

impl Relay {
    fn connect() -> Result<Self> {
        let config = load_config()?;
        let secret = read_secret(&config.secret_file)?;
        Ok(Relay {
            url: config.relay_url,
            secret,
        })
    }

    fn call(&self, request: ManageRequest) -> Result<ManageReply> {
        match share_engine::manage(&self.url, &self.secret, request)? {
            ManageReply::Failed { reason } => bail!("the relay refused: {reason}"),
            reply => Ok(reply),
        }
    }

    fn state(&self) -> Result<(Vec<String>, Vec<SharedTab>)> {
        match self.call(ManageRequest::State)? {
            ManageReply::State { guests, shares } => Ok((guests, shares)),
            _ => bail!("the relay sent an unexpected reply"),
        }
    }
}

fn ask_guests(
    prompt: &str,
    known: &[String],
    taken: &[String],
    allow_empty: bool,
) -> Result<Vec<String>> {
    require_terminal("--guest")?;
    let available: Vec<&str> = known
        .iter()
        .filter(|email| !taken.contains(email))
        .map(String::as_str)
        .collect();
    if !available.is_empty() {
        println!("Known guests: {}", available.join(", "));
    }
    loop {
        let guests = parse_guests(&ask(prompt)?);
        match guests.iter().find(|email| !email.contains('@')) {
            Some(typo) => eprintln!("{typo} is not an email"),
            None if guests.is_empty() && !allow_empty => eprintln!("type at least one email"),
            None => return Ok(guests),
        }
    }
}

fn parse_guests(line: &str) -> Vec<String> {
    let mut guests: Vec<String> = Vec::new();
    for email in line.split([' ', ',']).filter(|email| !email.is_empty()) {
        let email = email.to_lowercase();
        if !guests.contains(&email) {
            guests.push(email);
        }
    }
    guests
}

fn require_terminal(flags: &str) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("no terminal to ask on; pass {flags}");
    }
    Ok(())
}

pub fn serve(target: Target) -> Result<()> {
    let config = load_config()?;
    let secret = read_secret(&config.secret_file)?;
    let (Some(session_id), Some(block_id), Some(mode), Some(ttl)) =
        (target.session, target.block, target.mode, target.ttl)
    else {
        bail!("serve needs --session, --block, --mode and --ttl");
    };
    let ttl_secs = parse_ttl(&ttl)?;
    let marker = Marker::set(&session_id, &block_id)?;
    let request = ShareRequest {
        session_id: session_id.clone(),
        block_id: block_id.clone(),
        mode,
        guests: target.guests.clone(),
        label: (!marker.label.is_empty()).then(|| marker.label.clone()),
        ttl_secs,
        relay_url: config.relay_url,
        secret,
    };
    let path = state_path(&block_id);
    let block_id_for_log = block_id.clone();
    let mut state = ActiveShare {
        pid: std::process::id(),
        session_id,
        block_id,
        name: marker.name.clone(),
        mode,
        url: String::new(),
        credentials: Vec::new(),
        existing: Vec::new(),
        expires_at_unix: 0,
        viewers: Vec::new(),
    };
    let result = share_engine::run(request, |event| {
        match event {
            ShareEvent::Ready {
                url,
                expires_at_unix,
                credentials,
                existing,
            } => {
                state.url = url;
                state.expires_at_unix = expires_at_unix;
                state.credentials = credentials;
                state.existing = existing;
            }
            ShareEvent::ViewerJoined { email } => state.viewers.push(email),
            ShareEvent::ViewerLeft { email } => {
                if let Some(index) = state.viewers.iter().position(|viewer| *viewer == email) {
                    state.viewers.remove(index);
                }
            }
            ShareEvent::Input { .. } => return,
            ShareEvent::Ended { reason } => {
                eprintln!("share ended: {reason}");
                let _ = std::fs::remove_file(&path);
                return;
            }
        }
        if let Err(error) = write_state(&path, &state) {
            eprintln!("could not record the share: {error:#}");
        }
    });
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(log_path(&block_id_for_log));
    marker.restore();
    result
}

pub fn list() -> Result<()> {
    let (guests, shares) = Relay::connect()?.state()?;
    print_tabs(&shares);
    print_guests(&guests, &shares);
    Ok(())
}

fn print_tabs(shares: &[SharedTab]) {
    if shares.is_empty() {
        println!("No tab is shared.\n");
    }
    for tab in shares {
        println!(
            "{} on {}  ({})",
            tab_title(tab),
            tab.share.host,
            mode_text(tab.share.mode)
        );
        println!("  session  {}", tab.share.session_id);
        println!("  block    {}", tab.share.block_id);
        println!("  link     {}", tab.url);
        println!("  ends in  {}", ends_in(tab.share.expires_at_unix));
        println!("  guests   {}\n", guests_of(tab).join(", "));
    }
}

fn guests_of(tab: &SharedTab) -> Vec<String> {
    tab.share
        .guests
        .iter()
        .map(|email| match tab.watching.contains(email) {
            true => format!("{email} (watching)"),
            false => email.clone(),
        })
        .collect()
}

fn print_guests(guests: &[String], shares: &[SharedTab]) {
    if guests.is_empty() {
        println!("No guests yet.");
        return;
    }
    println!("Guests:");
    let width = guests.iter().map(String::len).max().unwrap_or(0);
    for email in guests {
        let tabs: Vec<String> = tabs_of(shares, email).map(tab_title).collect();
        let tabs = match tabs.is_empty() {
            true => "no tabs".to_string(),
            false => tabs.join(", "),
        };
        println!("  {email:width$}  {tabs}");
    }
}

fn tabs_of<'a>(shares: &'a [SharedTab], email: &'a str) -> impl Iterator<Item = &'a SharedTab> {
    shares
        .iter()
        .filter(move |tab| tab.share.guests.iter().any(|guest| guest == email))
}

fn tab_title(tab: &SharedTab) -> String {
    format!("{} / {}", tab.share.session_label, tab.share.label)
}

pub fn manage() -> Result<()> {
    let relay = Relay::connect()?;
    let result = crate::tui::shares::manage(|request| {
        share_engine::manage(&relay.url, &relay.secret, request)
    });
    if let Err(error) = &result {
        eprintln!("{error:#}");
        return wait_for_key();
    }
    result
}

pub fn stop(block_id: Option<String>, all: bool) -> Result<()> {
    let targets: Vec<ActiveShare> = match (block_id, all) {
        (_, true) => active_shares()?,
        (Some(block_id), false) => find_share(&block_id)?.into_iter().collect(),
        (None, false) => bail!("name a tab with --block, or pass --all"),
    };
    if targets.is_empty() {
        println!("This tab is not shared");
        return Ok(());
    }
    for share in &targets {
        unsafe { libc::kill(share.pid as i32, libc::SIGTERM) };
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while targets.iter().any(|share| alive(share.pid)) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let names: Vec<String> = targets.iter().map(|share| share.name.clone()).collect();
    println!("Sharing stopped: {}", names.join(", "));
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ActiveShare {
    pid: u32,
    session_id: String,
    block_id: String,
    name: String,
    mode: Mode,
    url: String,
    credentials: Vec<GuestCredential>,
    #[serde(default)]
    existing: Vec<String>,
    expires_at_unix: u64,
    viewers: Vec<String>,
}

fn wait_for_key() -> Result<()> {
    if !io::stdin().is_terminal() {
        return Ok(());
    }
    print!("Press Enter to close.");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(())
}

fn start_background(
    session_id: &str,
    block_id: &str,
    mode: Mode,
    guests: &[String],
    ttl_secs: u64,
) -> Result<ActiveShare> {
    let dir = shares_dir();
    std::fs::create_dir_all(&dir)?;
    let log_path = log_path(block_id);
    let log = std::fs::File::create(&log_path)?;
    let mode = match mode {
        Mode::View => "view",
        Mode::Input => "input",
    };
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "share",
            "serve",
            "--session",
            session_id,
            "--block",
            block_id,
        ])
        .args(["--mode", mode, "--ttl", &format!("{ttl_secs}s")])
        .args(guests.iter().flat_map(|guest| ["--guest", guest.as_str()]))
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().context("could not start the share")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(share) = find_share(block_id)?
            && !share.url.is_empty()
        {
            return Ok(share);
        }
        if child.try_wait()?.is_some() || std::time::Instant::now() > deadline {
            let _ = child.kill();
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            bail!("the share did not start:\n{}", log.trim());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn shares_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
        .join("wsctl/shares")
}

fn file_stem(block_id: &str) -> String {
    block_id.replace(':', "-")
}

fn log_path(block_id: &str) -> PathBuf {
    shares_dir().join(format!("{}.log", file_stem(block_id)))
}

fn state_path(block_id: &str) -> PathBuf {
    shares_dir().join(format!("{}.json", file_stem(block_id)))
}

fn write_state(path: &Path, state: &ActiveShare) -> Result<()> {
    std::fs::create_dir_all(shares_dir())?;
    let staging = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staging)?;
    file.write_all(serde_json::to_string(state)?.as_bytes())?;
    std::fs::rename(staging, path)?;
    Ok(())
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn find_share(block_id: &str) -> Result<Option<ActiveShare>> {
    Ok(active_shares()?
        .into_iter()
        .find(|share| share.block_id == block_id))
}

fn active_shares() -> Result<Vec<ActiveShare>> {
    let Ok(entries) = std::fs::read_dir(shares_dir()) else {
        return Ok(Vec::new());
    };
    let mut shares = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<ActiveShare>(&raw) {
            Ok(share) if alive(share.pid) => shares.push(share),
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(shares)
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
    if !io::stdin().is_terminal() {
        bail!(
            "pass --session and --block; open tabs:\n{}",
            tab_flags(&tabs)
        );
    }
    for (index, tab) in tabs.iter().enumerate() {
        println!("{:>3}  {}", index + 1, tab_name(tab));
    }
    loop {
        let answer = ask("Share tab number: ")?;
        if let Some(tab) = answer
            .parse::<usize>()
            .ok()
            .and_then(|number| tabs.get(number.wrapping_sub(1)))
        {
            return Ok((tab.session_id.clone(), tab.block_id.clone()));
        }
    }
}

fn tab_name(tab: &Tab) -> String {
    format!(
        "{} / {}  {}",
        tab.session_label, tab.window_label, tab.title
    )
}

fn tab_flags(tabs: &[Tab]) -> String {
    tabs.iter()
        .map(|tab| {
            format!(
                "  --session {} --block {}  {}",
                tab.session_id,
                tab.block_id,
                tab_name(tab)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    require_terminal("--mode")?;
    loop {
        match ask("Guests may [v]iew or also [i]nput? ")?.as_str() {
            "v" | "view" => return Ok(Mode::View),
            "i" | "input" => return Ok(Mode::Input),
            _ => continue,
        }
    }
}

fn ask_ttl() -> Result<u64> {
    if !io::stdin().is_terminal() {
        return parse_ttl(DEFAULT_TTL);
    }
    loop {
        let answer = ask(&format!("Expires after [{DEFAULT_TTL}]: "))?;
        match parse_ttl(if answer.is_empty() {
            DEFAULT_TTL
        } else {
            &answer
        }) {
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

struct Marker {
    name: String,
    session_id: String,
    window_id: String,
    label: String,
}

impl Marker {
    fn set(session_id: &str, block_id: &str) -> Result<Self> {
        let view = rex_call("session.view", json!({ "session_id": session_id }))?;
        let window = window_holding(&view, block_id)
            .with_context(|| format!("block {block_id} is in no window of {session_id}"))?;
        let shown = window["label"].as_str().unwrap_or_default();
        let tab = shown.strip_prefix(MARKER).unwrap_or(shown);
        let session = view["label"].as_str().unwrap_or_default();
        let marker = Marker {
            name: format!("{session} / {tab}"),
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
    let session_id = args["session_id"].as_str().unwrap_or_default().to_string();
    share_engine::rex_call(&session_id, method, args)
}
