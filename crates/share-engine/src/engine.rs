use crate::ShareRequest;
use crate::relay::{self, Relay};
use crate::rex::{Control, DataConnection, Events};
use crate::stream::{Piece, StreamParser};
use crate::translate;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use share_protocol::{AgentMessage, Mode, ProgramStatus, RelayMessage, Share, Theme};
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

const MAX_BACKOFF: Duration = Duration::from_secs(30);
const DATA_RETRY_DELAY: Duration = Duration::from_millis(500);

pub struct Engine {
    pub request: ShareRequest,
    pub control: Control,
    pub events: Events,
    pub share: Share,
    pub deadline: Instant,
    viewers: BTreeSet<u64>,
    theme: Option<Theme>,
    status: Option<ProgramStatus>,
    interrupt: Signal,
    terminate: Signal,
}

enum Step {
    Continue,
    DataLost,
    RelayLost,
    Finish(Option<String>),
}

impl Engine {
    pub fn new(
        request: ShareRequest,
        control: Control,
        events: Events,
        share: Share,
        deadline: Instant,
    ) -> Result<Self> {
        Ok(Self {
            request,
            control,
            events,
            share,
            deadline,
            viewers: BTreeSet::new(),
            theme: None,
            status: None,
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    pub async fn run(mut self, on_url: impl FnOnce(&str)) -> Result<()> {
        self.status = self.read_status().await;
        let (mut relay, url) = self.connect_relay().await?;
        on_url(&url);
        self.send_state(&mut relay).await;
        loop {
            let data = match self
                .control
                .open_data(&self.request.session_id, &self.request.block_id)
                .await
            {
                Ok(data) => data,
                Err(error) => {
                    self.end(
                        &mut relay,
                        Some("the tab is no longer available".to_string()),
                    )
                    .await;
                    return Err(error);
                }
            };
            let (data, step) = self.pump(&mut relay, data).await;
            data.close().await;
            match step {
                Step::DataLost => tokio::time::sleep(DATA_RETRY_DELAY).await,
                Step::RelayLost => match self.reconnect_relay().await {
                    Some(fresh) => relay = fresh,
                    None => return Ok(()),
                },
                Step::Finish(reason) => {
                    self.end(&mut relay, reason).await;
                    return Ok(());
                }
                Step::Continue => {}
            }
        }
    }

    async fn pump(
        &mut self,
        relay: &mut Relay,
        mut data: DataConnection,
    ) -> (DataConnection, Step) {
        let mut parser = StreamParser::new(data.nonce);
        self.resend_screens(relay).await;
        loop {
            let step = tokio::select! {
                frame = data.socket.next() => match frame {
                    Some(Ok(Message::Binary(bytes))) => self.forward(relay, &mut parser, &bytes).await,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Step::DataLost,
                    Some(Ok(_)) => Step::Continue,
                },
                event = self.events.recv() => match event {
                    Some(event) => self.on_event(relay, &event).await,
                    None => Step::Finish(Some("the Rex server closed the connection".to_string())),
                },
                message = relay.next() => match message {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Step::RelayLost,
                    Some(Ok(message)) => match relay::parse(&message) {
                        Some(message) => self.on_relay(relay, message).await,
                        None => Step::Continue,
                    },
                },
                _ = tokio::time::sleep_until(self.deadline) => Step::Finish(Some("the share expired".to_string())),
                _ = self.interrupt.recv() => Step::Finish(Some("the host stopped sharing".to_string())),
                _ = self.terminate.recv() => Step::Finish(Some("the host stopped sharing".to_string())),
            };
            if !matches!(step, Step::Continue) {
                return (data, step);
            }
        }
    }

    async fn forward(
        &mut self,
        relay: &mut Relay,
        parser: &mut StreamParser,
        bytes: &[u8],
    ) -> Step {
        for piece in parser.feed(bytes) {
            let sent = match piece {
                Piece::Output(output) => relay.send(Message::Binary(output.into())).await.is_ok(),
                Piece::Control(control) => match self.on_control(&control) {
                    Some(AgentMessage::Ended { reason }) => return Step::Finish(Some(reason)),
                    Some(message) => relay::send(relay, &message).await.is_ok(),
                    None => true,
                },
            };
            if !sent {
                return Step::RelayLost;
            }
        }
        Step::Continue
    }

    fn on_control(&mut self, control: &Value) -> Option<AgentMessage> {
        let message = translate::agent_message(control)?;
        match &message {
            AgentMessage::Resize { cols, rows } => {
                (self.share.cols, self.share.rows) = (*cols, *rows)
            }
            AgentMessage::Theme { theme } => self.theme = Some(theme.clone()),
            _ => {}
        }
        Some(message)
    }

    async fn on_event(&mut self, relay: &mut Relay, event: &Value) -> Step {
        let payload = &event["payload"];
        match payload["type"].as_str() {
            Some("session_destroyed") if self.is_my_session(&payload["payload"]) => {
                Step::Finish(Some("the session was closed".to_string()))
            }
            Some("session_event") => {
                self.on_session_event(relay, &payload["payload"]["event"])
                    .await
            }
            _ => Step::Continue,
        }
    }

    fn is_my_session(&self, payload: &Value) -> bool {
        payload["session_id"] == self.request.session_id.as_str()
            || payload["session"]["session_id"] == self.request.session_id.as_str()
    }

    async fn on_session_event(&mut self, relay: &mut Relay, event: &Value) -> Step {
        let payload = &event["payload"];
        if payload["block_id"] != self.request.block_id.as_str() {
            return Step::Continue;
        }
        match (event["type"].as_str(), payload["name"].as_str()) {
            (Some("block_closed"), _) => Step::Finish(Some("the tab was closed".to_string())),
            (Some("block_event"), Some("child_exited")) => {
                Step::Finish(Some(translate::exit_reason(&payload["payload"])))
            }
            (Some("block_event"), Some("program_status_changed" | "program_status_removed")) => {
                self.status = self.read_status().await;
                self.send(
                    relay,
                    AgentMessage::Status {
                        status: self.status.clone(),
                    },
                )
                .await
            }
            _ => Step::Continue,
        }
    }

    async fn on_relay(&mut self, relay: &mut Relay, message: RelayMessage) -> Step {
        match message {
            RelayMessage::ViewerJoined { viewer_id, .. } => {
                self.viewers.insert(viewer_id);
                self.send_screen(relay, viewer_id).await
            }
            RelayMessage::ViewerLeft { viewer_id, .. } => {
                self.viewers.remove(&viewer_id);
                Step::Continue
            }
            RelayMessage::Input { data, .. } => {
                self.write_input(&data).await;
                Step::Continue
            }
            RelayMessage::Stop { .. } => Step::Finish(None),
            RelayMessage::Registered { .. } => Step::Continue,
        }
    }

    async fn write_input(&self, data: &str) {
        if self.request.mode != Mode::Input {
            return;
        }
        let args = json!({"data": data});
        let _ = self.terminal("write", args).await;
    }

    async fn send_screen(&self, relay: &mut Relay, viewer_id: u64) -> Step {
        let Ok(reply) = self.terminal("format", screen_args()).await else {
            return Step::Continue;
        };
        let vt = reply["content"].as_str().unwrap_or_default().to_string();
        self.send(relay, AgentMessage::Screen { viewer_id, vt })
            .await
    }

    async fn resend_screens(&self, relay: &mut Relay) {
        for viewer_id in self.viewers.clone() {
            self.send_screen(relay, viewer_id).await;
        }
    }

    async fn send(&self, relay: &mut Relay, message: AgentMessage) -> Step {
        match relay::send(relay, &message).await {
            Ok(()) => Step::Continue,
            Err(_) => Step::RelayLost,
        }
    }

    async fn send_state(&self, relay: &mut Relay) {
        if let Some(theme) = self.theme.clone() {
            self.send(relay, AgentMessage::Theme { theme }).await;
        }
        self.send(
            relay,
            AgentMessage::Status {
                status: self.status.clone(),
            },
        )
        .await;
    }

    async fn read_status(&self) -> Option<ProgramStatus> {
        let reply = self.terminal("program_status", json!({})).await.ok()?;
        translate::current_status(&reply)
    }

    async fn terminal(&self, method: &str, args: Value) -> Result<Value> {
        self.control
            .terminal(
                &self.request.session_id,
                &self.request.block_id,
                method,
                args,
            )
            .await
    }

    async fn connect_relay(&self) -> Result<(Relay, String)> {
        let register = AgentMessage::Register {
            share: self.share.clone(),
        };
        relay::connect(&self.request.relay_url, &self.request.secret, &register).await
    }

    async fn reconnect_relay(&mut self) -> Option<Relay> {
        self.viewers.clear();
        let mut backoff = Duration::from_secs(1);
        loop {
            let wake = (Instant::now() + backoff).min(self.deadline);
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {},
                _ = self.interrupt.recv() => return None,
                _ = self.terminate.recv() => return None,
            }
            if Instant::now() >= self.deadline {
                return None;
            }
            if let Ok((mut relay, _)) = self.connect_relay().await {
                self.send_state(&mut relay).await;
                return Some(relay);
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn end(&self, relay: &mut Relay, reason: Option<String>) {
        if let Some(reason) = reason {
            let _ = relay::send(relay, &AgentMessage::Ended { reason }).await;
        }
        let _ = relay.close(None).await;
    }
}

fn screen_args() -> Value {
    json!({
        "format": "vt",
        "extra_cursor": true,
        "extra_modes": true,
        "extra_palette": true,
        "extra_style": true,
        "extra_scrolling_region": true,
        "extra_charsets": true,
        "extra_keyboard": true,
        "extra_kitty_keyboard": true,
    })
}
