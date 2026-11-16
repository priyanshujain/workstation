use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{ConnectInfo, Form, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use share_protocol::{VIEWER_PATH_PREFIX, ViewerInput, ViewerMessage};

use crate::assets;
use crate::auth::{random_token, token_hash, verify_password};
use crate::live::LiveShare;
use crate::prompt::PromptBuffer;
use crate::queue::{Outbound, Received, VIEWER_BACKLOG_BYTES, ViewerReceiver, viewer_queue};
use crate::relay::Relay;
use crate::store::unix_now;

const COOKIE_NAME: &str = "wsctl_share";
const BAD_LOGIN: &str = "That email and password do not match this share.";
const LIMITED_LOGIN: &str = "Too many failed attempts. Try again in 15 minutes.";

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

pub fn router(relay: Arc<Relay>) -> Router {
    Router::new()
        .route("/s/{id}", get(page))
        .route("/s/{id}/login", post(login))
        .route("/s/{id}/logout", post(logout))
        .route("/s/{id}/ws", get(connect))
        .route("/assets/{name}", get(asset))
        .with_state(relay)
}

async fn page(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(live) = relay.find(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match signed_in(&relay, &headers, &live).await {
        Ok(Some(_)) => html(StatusCode::OK, assets::PAGE.to_string()),
        Ok(None) => html(StatusCode::OK, login_page(&id, "")),
        Err(status) => status.into_response(),
    }
}

async fn login(
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<Login>,
) -> Response {
    let Some(live) = relay.find(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let email = form.email.trim().to_lowercase();
    let account = format!("account:{id}:{email}");
    let keys = [
        account.clone(),
        format!("ip:{}", client_ip(&relay, &headers, peer)),
    ];
    if relay.failures.blocked(&keys) {
        return html(
            StatusCode::TOO_MANY_REQUESTS,
            login_page(&id, LIMITED_LOGIN),
        );
    }
    let hash = match relay.store.credential_hash(&id, &email).await {
        Ok(hash) => hash,
        Err(error) => return unavailable(error),
    };
    let password = form.password;
    let verified = tokio::task::spawn_blocking(move || verify_password(&password, hash.as_deref()))
        .await
        .unwrap_or(false);
    if !verified || !live.admits(&email) {
        relay.failures.record(&keys);
        return html(StatusCode::UNAUTHORIZED, login_page(&id, BAD_LOGIN));
    }
    relay.failures.clear(&account);
    match start_session(&relay, &live, &email).await {
        Ok(cookie) => redirect(&id, cookie),
        Err(error) => unavailable(error),
    }
}

fn unavailable(error: anyhow::Error) -> Response {
    tracing::error!(%error, "share database unavailable");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

async fn start_session(relay: &Relay, live: &LiveShare, email: &str) -> anyhow::Result<String> {
    let token = random_token::<32>()?;
    let expires_at = live.share().expires_at_unix;
    relay
        .store
        .add_session(&token_hash(&token), &live.id, email, expires_at)
        .await?;
    let max_age = expires_at.saturating_sub(unix_now());
    Ok(cookie(relay, &live.id, &token, max_age))
}

async fn logout(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(token) = session_token(&headers)
        && let Err(error) = relay.store.delete_session(&token_hash(&token)).await
    {
        return unavailable(error);
    }
    redirect(&id, cookie(&relay, &id, "", 0))
}

async fn asset(Path(name): Path<String>) -> Response {
    match assets::find(&name) {
        Some((content_type, body)) => {
            ([(header::CONTENT_TYPE, content_type)], body).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
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
    let Some(live) = relay.find(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match signed_in(&relay, &headers, &live).await {
        Ok(Some(email)) => upgrade.on_upgrade(move |socket| session(relay, live, email, socket)),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(status) => status.into_response(),
    }
}

async fn signed_in(
    relay: &Relay,
    headers: &HeaderMap,
    live: &LiveShare,
) -> Result<Option<String>, StatusCode> {
    let Some(token) = session_token(headers) else {
        return Ok(None);
    };
    let email = relay
        .store
        .session_email(&token_hash(&token), &live.id)
        .await
        .map_err(|error| unavailable(error).status())?;
    Ok(email.filter(|email| live.admits(email)))
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn cookie(relay: &Relay, id: &str, token: &str, max_age: u64) -> String {
    let secure = if relay.config.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{COOKIE_NAME}={token}; Path={VIEWER_PATH_PREFIX}{id}; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}"
    )
}

fn redirect(id: &str, cookie: String) -> Response {
    let location = format!("{VIEWER_PATH_PREFIX}{id}");
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, location), (header::SET_COOKIE, cookie)],
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

fn login_page(id: &str, error: &str) -> String {
    assets::LOGIN
        .replace("__ACTION__", &format!("{VIEWER_PATH_PREFIX}{id}/login"))
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
    let Ok(ViewerInput::Input { data }) = serde_json::from_str(text) else {
        return;
    };
    if !live.accepts_input() {
        return;
    }
    if record_input(relay, live, email, prompts, &data)
        .await
        .is_err()
    {
        relay.stop_unaudited(live).await;
        return;
    }
    live.input(viewer_id, email, data);
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
