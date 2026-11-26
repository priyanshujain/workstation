use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{ConnectInfo, Form, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use share_protocol::{RelayMessage, UploadFile, VIEWER_PATH_PREFIX, ViewerInput, ViewerMessage};

use crate::assets;
use crate::auth::{random_token, token_hash, verify_password};
use crate::live::LiveShare;
use crate::prompt::PromptBuffer;
use crate::queue::{Outbound, Received, VIEWER_BACKLOG_BYTES, ViewerReceiver, viewer_queue};
use crate::relay::{Relay, normalize};
use crate::store::unix_now;

const COOKIE_NAME: &str = "wsctl_share";
const BAD_LOGIN: &str = "That email and password do not match.";
const LIMITED_LOGIN: &str = "Too many failed attempts. Try again in 15 minutes.";
const SESSION_SECS: u64 = 7 * 24 * 3600;
const MAX_PASTE_BYTES: usize = 128 * 1024;
const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;
const MAX_UPLOAD_FILES: u32 = 5;
const MAX_NAME_BYTES: usize = 255;
const MAX_VIEWER_MESSAGE_BYTES: usize = 14 * 1024 * 1024;

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
    #[serde(default)]
    next: String,
}

pub fn router(relay: Arc<Relay>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/events", get(events))
        .route("/s/{id}", get(tab))
        .route("/s/{id}/ws", get(connect))
        .route("/assets/{name}", get(asset))
        .with_state(relay)
}

async fn home(State(relay): State<Arc<Relay>>, headers: HeaderMap) -> Response {
    app_or_login(&relay, &headers, "/").await
}

async fn tab(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    app_or_login(&relay, &headers, &format!("{VIEWER_PATH_PREFIX}{id}")).await
}

async fn app_or_login(relay: &Relay, headers: &HeaderMap, path: &str) -> Response {
    match signed_in(relay, headers).await {
        Ok(Some(_)) => html(StatusCode::OK, assets::PAGE.to_string()),
        Ok(None) => html(StatusCode::OK, login_page(safe_next(path), "")),
        Err(status) => status.into_response(),
    }
}

async fn login(
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<Login>,
) -> Response {
    let next = safe_next(&form.next);
    let email = normalize(&form.email);
    let account = format!("account:{email}");
    let keys = [
        account.clone(),
        format!("ip:{}", client_ip(&relay, &headers, peer)),
    ];
    if relay.failures.blocked(&keys) {
        return html(
            StatusCode::TOO_MANY_REQUESTS,
            login_page(next, LIMITED_LOGIN),
        );
    }
    let hash = match relay.store.guest_hash(&email).await {
        Ok(hash) => hash,
        Err(error) => return unavailable(error),
    };
    let password = form.password;
    let verified = tokio::task::spawn_blocking(move || verify_password(&password, hash.as_deref()))
        .await
        .unwrap_or(false);
    if !verified {
        relay.failures.record(&keys);
        return html(StatusCode::UNAUTHORIZED, login_page(next, BAD_LOGIN));
    }
    relay.failures.clear(&account);
    match start_session(&relay, &email).await {
        Ok(cookie) => redirect(next, cookie),
        Err(error) => unavailable(error),
    }
}

fn safe_next(next: &str) -> &str {
    let valid = next.strip_prefix(VIEWER_PATH_PREFIX).is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    });
    if valid { next } else { "/" }
}

fn unavailable(error: anyhow::Error) -> Response {
    tracing::error!(%error, "share database unavailable");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

async fn start_session(relay: &Relay, email: &str) -> anyhow::Result<String> {
    let token = random_token::<32>()?;
    relay
        .store
        .add_session(&token_hash(&token), email, unix_now() + SESSION_SECS)
        .await?;
    Ok(cookie(relay, &token, SESSION_SECS))
}

async fn logout(State(relay): State<Arc<Relay>>, headers: HeaderMap) -> Response {
    let token = match signed_in_token(&relay, &headers).await {
        Ok(signed_in) => signed_in.map(|(token, _)| token),
        Err(status) => return status.into_response(),
    };
    if let Some(token) = token
        && let Err(error) = relay.store.delete_session(&token_hash(&token)).await
    {
        return unavailable(error);
    }
    redirect("/", cookie(&relay, "", 0))
}

async fn asset(Path(name): Path<String>) -> Response {
    match assets::find(&name) {
        Some((content_type, body)) => {
            ([(header::CONTENT_TYPE, content_type)], body).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn events(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(&relay, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match signed_in(&relay, &headers).await {
        Ok(Some(email)) => upgrade.on_upgrade(move |socket| lobby(relay, email, socket)),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(status) => status.into_response(),
    }
}

async fn lobby(relay: Arc<Relay>, email: String, mut socket: WebSocket) {
    let mut changes = relay.subscribe();
    let mut sent = None;
    loop {
        let tabs = relay.guest_tabs(&email);
        if sent.as_ref() != Some(&tabs) {
            send(&mut socket, &ViewerMessage::Tabs { tabs: tabs.clone() }).await;
            sent = Some(tabs);
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { return },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn connect(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_allowed(&relay, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let email = match signed_in(&relay, &headers).await {
        Ok(Some(email)) => email,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(status) => return status.into_response(),
    };
    match relay.find(&id).filter(|live| live.admits(&email)) {
        Some(live) => upgrade
            .max_message_size(MAX_VIEWER_MESSAGE_BYTES)
            .max_frame_size(MAX_VIEWER_MESSAGE_BYTES)
            .on_upgrade(move |socket| session(relay, live, email, socket)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn signed_in(relay: &Relay, headers: &HeaderMap) -> Result<Option<String>, StatusCode> {
    Ok(signed_in_token(relay, headers)
        .await?
        .map(|(_, email)| email))
}

async fn signed_in_token(
    relay: &Relay,
    headers: &HeaderMap,
) -> Result<Option<(String, String)>, StatusCode> {
    for token in session_tokens(headers) {
        let email = relay
            .store
            .session_email(&token_hash(&token))
            .await
            .map_err(|error| unavailable(error).status())?;
        if let Some(email) = email {
            return Ok(Some((token, email)));
        }
    }
    Ok(None)
}

fn session_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn cookie(relay: &Relay, token: &str, max_age: u64) -> String {
    let secure = if relay.config.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{COOKIE_NAME}={token}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}")
}

fn redirect(location: &str, cookie: String) -> Response {
    (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, location.to_string()),
            (header::SET_COOKIE, cookie),
        ],
    )
        .into_response()
}

fn html(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

fn login_page(next: &str, error: &str) -> String {
    assets::LOGIN
        .replace("__NEXT__", next)
        .replace("__ERROR__", error)
}

fn origin_allowed(relay: &Relay, headers: &HeaderMap) -> bool {
    let expected = public_origin(&relay.config.public_url);
    headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| origin.eq_ignore_ascii_case(expected))
}

fn public_origin(public_url: &str) -> &str {
    let after_scheme = public_url.find("://").map_or(0, |index| index + 3);
    let end = public_url[after_scheme..]
        .find('/')
        .map_or(public_url.len(), |index| after_scheme + index);
    &public_url[..end]
}

fn client_ip(relay: &Relay, headers: &HeaderMap, peer: SocketAddr) -> String {
    relay
        .config
        .trust_proxy_header
        .as_deref()
        .and_then(|name| headers.get(name))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| peer.ip().to_string())
}

async fn session(relay: Arc<Relay>, live: Arc<LiveShare>, email: String, mut socket: WebSocket) {
    let viewer_id = relay.next_viewer_id();
    let (tx, outbox) = viewer_queue(VIEWER_BACKLOG_BYTES);
    if !live.join(viewer_id, &email, tx) {
        return;
    }
    if let Err(error) = relay.store.viewer_joined(&live.id, viewer_id, &email).await {
        tracing::error!(%error, "could not record a viewer join");
    }
    pump(&relay, &live, viewer_id, &email, &mut socket, outbox).await;
    live.leave(viewer_id);
    if let Err(error) = relay.store.viewer_left(&live.id, viewer_id).await {
        tracing::error!(%error, "could not record a viewer leaving");
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn pump(
    relay: &Relay,
    live: &LiveShare,
    viewer_id: u64,
    email: &str,
    socket: &mut WebSocket,
    mut outbox: ViewerReceiver,
) {
    let mut prompts = PromptBuffer::default();
    loop {
        tokio::select! {
            outbound = outbox.recv() => match outbound {
                Received::Outbound(Outbound::Message(message)) => send(socket, &message).await,
                Received::Outbound(Outbound::Bytes(bytes)) => { let _ = socket.send(Message::Binary(bytes.into())).await; }
                Received::Lagged => {
                    close(socket, LAGGED_REASON).await;
                    return;
                }
                Received::Outbound(Outbound::Close) | Received::Closed => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => receive(relay, live, viewer_id, email, &mut prompts, &text).await,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn receive(
    relay: &Relay,
    live: &LiveShare,
    viewer_id: u64,
    email: &str,
    prompts: &mut PromptBuffer,
    text: &str,
) {
    let Ok(input) = serde_json::from_str(text) else {
        return;
    };
    if !live.accepts_input() {
        return;
    }
    let Some((record, message)) = forwarded(input, viewer_id, email) else {
        return;
    };
    if record_input(relay, live, email, prompts, &record)
        .await
        .is_err()
    {
        relay.stop_unaudited(live).await;
        return;
    }
    live.send_input(message);
}

fn forwarded(input: ViewerInput, viewer_id: u64, email: &str) -> Option<(String, RelayMessage)> {
    let email = email.to_string();
    match input {
        ViewerInput::Input { data } => Some((
            data.clone(),
            RelayMessage::Input {
                viewer_id,
                email,
                data,
            },
        )),
        ViewerInput::Paste { text } if text.len() <= MAX_PASTE_BYTES => Some((
            bracketed(&text),
            RelayMessage::Paste {
                viewer_id,
                email,
                text,
            },
        )),
        ViewerInput::Upload { file } => {
            let record = bracketed(&image_placeholder(&file, upload_size(&file)?));
            Some((
                record,
                RelayMessage::Upload {
                    viewer_id,
                    email,
                    file,
                },
            ))
        }
        ViewerInput::Paste { .. } => None,
    }
}

fn upload_size(file: &UploadFile) -> Option<usize> {
    let described = (1..=MAX_UPLOAD_FILES).contains(&file.count)
        && file.index < file.count
        && file.mime.starts_with("image/")
        && (1..=MAX_NAME_BYTES).contains(&file.name.len())
        && (1..=MAX_NAME_BYTES).contains(&file.batch.len());
    let size = STANDARD.decode(&file.data).ok()?.len();
    (described && (1..=MAX_UPLOAD_BYTES).contains(&size)).then_some(size)
}

fn image_placeholder(file: &UploadFile, size: usize) -> String {
    let name: String = file.name.chars().filter(|c| !c.is_control()).collect();
    format!("[image {name} {} KB]", size.div_ceil(1024))
}

fn bracketed(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

async fn record_input(
    relay: &Relay,
    live: &LiveShare,
    email: &str,
    prompts: &mut PromptBuffer,
    data: &str,
) -> anyhow::Result<()> {
    relay.store.input(&live.id, email, data).await?;
    let share = live.share();
    for prompt in prompts.feed(data) {
        relay.store.prompt(&live.id, &share, email, &prompt).await?;
    }
    Ok(())
}

async fn send(socket: &mut WebSocket, message: &ViewerMessage) {
    if let Ok(text) = serde_json::to_string(message) {
        let _ = socket.send(Message::Text(text.into())).await;
    }
}

const LAGGED_REASON: &str = "viewer fell behind; reload to catch up";

async fn close(socket: &mut WebSocket, reason: &str) {
    let frame = CloseFrame {
        code: close_code::POLICY,
        reason: reason.into(),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

#[cfg(test)]
mod tests {
    use super::public_origin;

    #[test]
    fn origin_drops_path() {
        assert_eq!(
            public_origin("https://share.example.com/"),
            "https://share.example.com"
        );
        assert_eq!(
            public_origin("http://127.0.0.1:7701"),
            "http://127.0.0.1:7701"
        );
    }
}
