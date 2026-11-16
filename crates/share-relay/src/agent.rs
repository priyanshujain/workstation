use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use share_protocol::{AGENT_PATH, AGENT_SECRET_HEADER, AgentMessage, RelayMessage};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::live::LiveShare;
use crate::relay::Relay;
use crate::store::unix_now;

pub fn router(relay: Arc<Relay>) -> Router {
    Router::new()
        .route(AGENT_PATH, get(connect))
        .with_state(relay)
}

async fn connect(
    State(relay): State<Arc<Relay>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !secret_matches(&headers, &relay.config.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade.on_upgrade(move |socket| session(relay, socket))
}

pub fn secret_matches(headers: &HeaderMap, secret: &str) -> bool {
    let given = headers
        .get(AGENT_SECRET_HEADER)
        .map_or(&[][..], |value| value.as_bytes());
    !secret.is_empty() && bool::from(given.ct_eq(secret.as_bytes()))
}

enum Outcome {
    Ended(String),
    Disconnected,
}

async fn session(relay: Arc<Relay>, mut socket: WebSocket) {
    let Some(first) = read_first(&mut socket).await else {
        return;
    };
    let (engine, outbox) = mpsc::unbounded_channel();
    let Some(live) = attach(&relay, &mut socket, first, engine).await else {
        return;
    };
    let expires_at = live.share().expires_at_unix;
    match pump(&live, &mut socket, outbox, expires_at).await {
        Outcome::Ended(reason) => relay.end(&live, &reason).await,
        Outcome::Disconnected => relay.orphan(live),
    }
}

async fn attach(
    relay: &Arc<Relay>,
    socket: &mut WebSocket,
    first: AgentMessage,
    engine: UnboundedSender<RelayMessage>,
) -> Option<Arc<LiveShare>> {
    match first {
        AgentMessage::Register { share } if share.expires_at_unix > unix_now() => {
            let (live, credentials) = relay.register(share, engine).await.ok()?;
            live.send_engine(RelayMessage::Registered {
                share_id: live.id.clone(),
                url: relay.viewer_url(&live.id),
                credentials,
            });
            Some(live)
        }
        AgentMessage::Resume { share_id } => {
            let resumed = relay.resume(&share_id, engine);
            if resumed.is_none() {
                send(socket, &RelayMessage::ResumeRejected { share_id }).await;
            }
            resumed
        }
        _ => None,
    }
}

async fn read_first(socket: &mut WebSocket) -> Option<AgentMessage> {
    let Some(Ok(Message::Text(text))) = socket.recv().await else {
        return None;
    };
    serde_json::from_str(&text).ok()
}

async fn pump(
    live: &LiveShare,
    socket: &mut WebSocket,
    mut outbox: UnboundedReceiver<RelayMessage>,
    expires_at: u64,
) -> Outcome {
    let expiry = tokio::time::sleep(Duration::from_secs(expires_at.saturating_sub(unix_now())));
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            () = &mut expiry => {
                let reason = "expired".to_string();
                send(socket, &RelayMessage::Stop { reason: reason.clone() }).await;
                return Outcome::Ended(reason);
            }
            Some(message) = outbox.recv() => send(socket, &message).await,
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(bytes))) => live.bytes(&bytes),
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                    Ok(AgentMessage::Ended { reason }) => return Outcome::Ended(reason),
                    Ok(message) => live.apply(message),
                    Err(error) => tracing::warn!(%error, "bad engine message"),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => return Outcome::Disconnected,
            },
        }
    }
}

async fn send(socket: &mut WebSocket, message: &RelayMessage) {
    if let Ok(text) = serde_json::to_string(message) {
        let _ = socket.send(Message::Text(text.into())).await;
    }
}
