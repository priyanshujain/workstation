mod engine;
mod relay;
mod rex;
mod socket;
mod stream;
mod translate;

use anyhow::{Context, Result, bail};
use rex::Control;
use serde_json::{Value, json};
use share_protocol::{GuestCredential, Mode, Share};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Tab {
    pub session_id: String,
    pub session_label: String,
    pub window_label: String,
    pub block_id: String,
    pub title: String,
}

pub struct ShareRequest {
    pub session_id: String,
    pub block_id: String,
    pub mode: Mode,
    pub guests: Vec<String>,
    pub ttl_secs: u64,
    pub relay_url: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShareEvent {
    Ready {
        url: String,
        expires_at_unix: u64,
        credentials: Vec<GuestCredential>,
    },
    ViewerJoined {
        email: String,
    },
    ViewerLeft {
        email: String,
    },
    Input {
        email: String,
    },
    Ended {
        reason: String,
    },
}

pub fn list_tabs() -> Result<Vec<Tab>> {
    runtime()?.block_on(async {
        let (control, _events) = Control::connect(&socket::server_socket()?).await?;
        let mut tabs = Vec::new();
        for session in sessions(&control).await? {
            tabs.extend(session_tabs(&control, &session).await?);
        }
        Ok(tabs)
    })
}

pub fn rex_call(session_id: &str, method: &str, args: Value) -> Result<Value> {
    runtime()?.block_on(async {
        let (control, _events) = Control::connect(&socket::server_socket()?).await?;
        control
            .call("session.attach", json!({"session_id": session_id}))
            .await?;
        control.call(method, args).await
    })
}

pub fn run(request: ShareRequest, on_event: impl FnMut(ShareEvent)) -> Result<()> {
    runtime()?.block_on(async {
        let (control, events) = Control::connect(&socket::server_socket()?).await?;
        control
            .call("session.attach", json!({"session_id": request.session_id}))
            .await?;
        let tab = find_tab(&control, &request).await?;
        let mut share = share_for(&request, &tab)?;
        (share.cols, share.rows) = size(&control, &request).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(request.ttl_secs);
        engine::Engine::new(request, control, events, share, deadline, on_event)?
            .run()
            .await
    })
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

async fn sessions(control: &Control) -> Result<Vec<Value>> {
    let reply = control
        .call("session.attach_all", json!({"event_scope": "session"}))
        .await?;
    Ok(reply["sessions"].as_array().cloned().unwrap_or_default())
}

async fn session_tabs(control: &Control, session: &Value) -> Result<Vec<Tab>> {
    let session_id = session["session_id"].as_str().unwrap_or_default();
    let view = control
        .call("session.view", json!({"session_id": session_id}))
        .await?;
    let mut tabs = Vec::new();
    for window in view["windows"].as_array().into_iter().flatten() {
        for block in terminal_blocks(window) {
            let block_id = block["block_id"].as_str().unwrap_or_default().to_string();
            tabs.push(Tab {
                session_id: session_id.to_string(),
                session_label: text(&view["label"]),
                window_label: text(&window["label"]),
                title: title(control, session_id, &block_id, block).await,
                block_id,
            });
        }
    }
    Ok(tabs)
}

fn terminal_blocks(window: &Value) -> impl Iterator<Item = &Value> {
    window["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|layer| layer["blocks"].as_array().into_iter().flatten())
        .filter(|block| block["creator_name"] == "com.superlogical.terminal")
}

async fn title(control: &Control, session_id: &str, block_id: &str, block: &Value) -> String {
    let reply = control
        .terminal(session_id, block_id, "title", json!({}))
        .await;
    match reply.map(|reply| text(&reply["title"])) {
        Ok(title) if !title.is_empty() => title,
        _ => text(&block["label"]),
    }
}

async fn find_tab(control: &Control, request: &ShareRequest) -> Result<Tab> {
    let session = json!({"session_id": request.session_id});
    let tabs = session_tabs(control, &session).await?;
    match tabs
        .into_iter()
        .find(|tab| tab.block_id == request.block_id)
    {
        Some(tab) => Ok(tab),
        None => bail!(
            "block {} is not a terminal in session {}",
            request.block_id,
            request.session_id
        ),
    }
}

async fn size(control: &Control, request: &ShareRequest) -> Result<(u16, u16)> {
    let reply = control
        .terminal(&request.session_id, &request.block_id, "size", json!({}))
        .await?;
    let dimension = |key: &str| {
        reply[key]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
    };
    Ok((
        dimension("columns").unwrap_or(80),
        dimension("rows").unwrap_or(24),
    ))
}

fn share_for(request: &ShareRequest, tab: &Tab) -> Result<Share> {
    let expires =
        SystemTime::now().duration_since(UNIX_EPOCH)? + Duration::from_secs(request.ttl_secs);
    Ok(Share {
        session_id: tab.session_id.clone(),
        session_label: tab.session_label.clone(),
        block_id: tab.block_id.clone(),
        label: tab.title.clone(),
        host: hostname().context("could not read the hostname")?,
        mode: request.mode,
        guests: request.guests.clone(),
        expires_at_unix: expires.as_secs(),
        cols: 0,
        rows: 0,
    })
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn hostname() -> Result<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for writes of its full length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        bail!(std::io::Error::last_os_error());
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let full = String::from_utf8_lossy(&buf[..len]);
    Ok(full.split('.').next().unwrap_or_default().to_string())
}
