use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use share_protocol::{ACCESS_JWT_HEADER, ViewerInput, ViewerMessage};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::assets;
use crate::live::{LiveShare, Outbound};
use crate::prompt::PromptBuffer;
use crate::relay::Relay;

pub fn router(relay: Arc<Relay>) -> Router {
    Router::new()
        .route("/s/{id}", get(page))
        .route("/s/{id}/ws", get(connect))
        .route("/assets/{name}", get(asset))
        .with_state(relay)
}

async fn page(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match authorize(&relay, &headers, &id).await {
        Ok(_) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            assets::PAGE,
        )
            .into_response(),
        Err(status) => status.into_response(),
    }
}

async fn asset(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if authenticate(&relay, &headers).await.is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }
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
    match authorize(&relay, &headers, &id).await {
        Ok((live, email)) => upgrade.on_upgrade(move |socket| session(relay, live, email, socket)),
        Err(status) => status.into_response(),
    }
}

pub async fn authorize(
    relay: &Relay,
    headers: &HeaderMap,
    id: &str,
) -> Result<(Arc<LiveShare>, String), StatusCode> {
    let email = authenticate(relay, headers)
        .await
        .ok_or(StatusCode::FORBIDDEN)?;
    let live = relay.find(id).ok_or(StatusCode::NOT_FOUND)?;
    if !live.admits(&email) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok((live, email))
}

async fn authenticate(relay: &Relay, headers: &HeaderMap) -> Option<String> {
    #[cfg(debug_assertions)]
    if let Some(email) = &relay.config.dev_email {
        return Some(email.clone());
    }
    let token = headers.get(ACCESS_JWT_HEADER)?.to_str().ok()?;
    match relay.verifier.verify(token).await {
        Ok(email) => Some(email),
        Err(error) => {
            tracing::warn!(%error, "rejected access token");
            None
        }
    }
}

async fn session(relay: Arc<Relay>, live: Arc<LiveShare>, email: String, mut socket: WebSocket) {
    let viewer_id = relay.next_viewer_id();
    let (tx, outbox) = mpsc::unbounded_channel();
    if !live.join(viewer_id, &email, tx) {
        return;
    }
    relay.store.viewer_joined(&live.id, viewer_id, &email).await;
    pump(&relay, &live, viewer_id, &email, &mut socket, outbox).await;
    live.leave(viewer_id);
    relay.store.viewer_left(&live.id, viewer_id).await;
    let _ = socket.send(Message::Close(None)).await;
}

async fn pump(
    relay: &Relay,
    live: &LiveShare,
    viewer_id: u64,
    email: &str,
    socket: &mut WebSocket,
    mut outbox: UnboundedReceiver<Outbound>,
) {
    let mut prompts = PromptBuffer::default();
    loop {
        tokio::select! {
            outbound = outbox.recv() => match outbound {
                Some(Outbound::Message(message)) => send(socket, &message).await,
                Some(Outbound::Bytes(bytes)) => { let _ = socket.send(Message::Binary(bytes.into())).await; }
                Some(Outbound::Close) | None => return,
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
    if !live.input(viewer_id, email, data.clone()) {
        return;
    }
    relay.store.input(&live.id, email, &data).await;
    let share = live.share();
    for prompt in prompts.feed(&data) {
        relay.store.prompt(&live.id, &share, email, &prompt).await;
    }
}

async fn send(socket: &mut WebSocket, message: &ViewerMessage) {
    if let Ok(text) = serde_json::to_string(message) {
        let _ = socket.send(Message::Text(text.into())).await;
    }
}
