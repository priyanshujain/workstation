use crate::relay::{self, Relay};
use crate::rex::{Control, DataConnection, Events};
use crate::stream::{Piece, StreamParser};
use crate::translate;
use crate::{ShareEvent, ShareRequest};
use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use share_protocol::{
    AgentMessage, GuestCredential, Mode, ProgramStatus, RelayMessage, Share, Theme,
};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RELAY_LOST_SHARE: &str = "the relay lost the share";
const DATA_RETRY_DELAY: Duration = Duration::from_millis(500);

pub struct Engine<'a> {
    pub request: ShareRequest,
    pub control: Control,
    pub events: Events,
    pub share: Share,
    pub deadline: Instant,
    viewers: BTreeMap<u64, String>,
    theme: Option<Theme>,
    status: Option<ProgramStatus>,
    interrupt: Signal,
    terminate: Signal,
    on_event: Box<dyn FnMut(ShareEvent) + 'a>,
    share_id: String,
}

enum Reconnect {
    Back(Box<Relay>),
    Gone,
    Rejected,
}

enum Step {
    Continue,
    DataLost,
    RelayLost,
    Finish(Option<String>),
}

impl<'a> Engine<'a> {
    pub fn new(
        request: ShareRequest,
        control: Control,
        events: Events,
        share: Share,
        deadline: Instant,
        on_event: impl FnMut(ShareEvent) + 'a,
    ) -> Result<Self> {
        Ok(Self {
            request,
            control,
            events,
            share,
            deadline,
            viewers: BTreeMap::new(),
            theme: None,
            status: None,
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            on_event: Box::new(on_event),
            share_id: String::new(),
        })
    }

    pub async fn run(mut self) -> Result<()> {
        self.status = self.read_status().await;
        let (mut relay, url, credentials, existing) = self.register().await?;
        (self.on_event)(ShareEvent::Ready {
            url,
            expires_at_unix: self.share.expires_at_unix,
            credentials,
            existing,
        });
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
                    Reconnect::Back(fresh) => relay = *fresh,
                    Reconnect::Gone => {
                        self.notify_end("the share ended while the relay was away");
                        return Ok(());
                    }
                    Reconnect::Rejected => bail!(RELAY_LOST_SHARE),
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
            RelayMessage::ViewerJoined { viewer_id, email } => {
                self.viewers.insert(viewer_id, email.clone());
                (self.on_event)(ShareEvent::ViewerJoined { email });
                self.send_screen(relay, viewer_id).await
            }
            RelayMessage::ViewerLeft { viewer_id, email } => {
                (self.on_event)(ShareEvent::ViewerLeft { email });
                self.viewers.remove(&viewer_id);
                Step::Continue
            }
            RelayMessage::Input { data, email, .. } => {
                self.write_input(email, &data).await;
                Step::Continue
            }
            RelayMessage::Paste { text, email, .. } => {
                self.paste(email, text).await;
                Step::Continue
            }
            RelayMessage::Stop { reason } => {
                self.notify_end(&reason);
                Step::Finish(None)
            }
            RelayMessage::Registered { .. }
            | RelayMessage::Managed { .. }
            | RelayMessage::Resumed { .. }
            | RelayMessage::ResumeRejected { .. }
            | RelayMessage::Upload { .. } => Step::Continue,
        }
    }

    async fn write_input(&mut self, email: String, data: &str) {
        if self.request.mode != Mode::Input {
            return;
        }
        (self.on_event)(ShareEvent::Input { email });
        let args = json!({"data": data});
        let _ = self.terminal("write", args).await;
    }

    async fn paste(&mut self, email: String, text: String) {
        if self.request.mode != Mode::Input {
            return;
        }
        (self.on_event)(ShareEvent::Input { email });
        let event = json!({
            "session_id": self.request.session_id,
            "block_id": self.request.block_id,
            "event": {"type": "paste", "payload": {"text": text}},
        });
        let _ = self.control.call("block.event", event).await;
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
        for viewer_id in self.viewers.keys().copied().collect::<Vec<_>>() {
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

    async fn register(&mut self) -> Result<(Relay, String, Vec<GuestCredential>, Vec<String>)> {
        let register = AgentMessage::Register {
            share: self.share.clone(),
        };
        match self.connect_relay(&register).await? {
            (
                relay,
                RelayMessage::Registered {
                    share_id,
                    url,
                    credentials,
                    existing,
                },
            ) => {
                self.share_id = share_id;
                Ok((relay, url, credentials, existing))
            }
            (_, RelayMessage::Stop { reason }) => bail!("the relay refused the share: {reason}"),
            _ => bail!("the relay did not register the share"),
        }
    }

    async fn resume(&mut self) -> Result<Reconnect> {
        let resume = AgentMessage::Resume {
            share_id: self.share_id.clone(),
        };
        match self.connect_relay(&resume).await? {
            (mut relay, RelayMessage::ResumeRejected { .. }) => {
                self.end(&mut relay, Some(RELAY_LOST_SHARE.to_string()))
                    .await;
                Ok(Reconnect::Rejected)
            }
            (mut relay, _) => {
                self.send_state(&mut relay).await;
                Ok(Reconnect::Back(Box::new(relay)))
            }
        }
    }

    async fn connect_relay(&self, hello: &AgentMessage) -> Result<(Relay, RelayMessage)> {
        relay::connect(&self.request.relay_url, &self.request.secret, hello).await
    }

    async fn reconnect_relay(&mut self) -> Reconnect {
        for email in std::mem::take(&mut self.viewers).into_values() {
            (self.on_event)(ShareEvent::ViewerLeft { email });
        }
        let mut backoff = Duration::from_secs(1);
        loop {
            let wake = (Instant::now() + backoff).min(self.deadline);
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {},
                _ = self.interrupt.recv() => return Reconnect::Gone,
                _ = self.terminate.recv() => return Reconnect::Gone,
            }
            if Instant::now() >= self.deadline {
                return Reconnect::Gone;
            }
            if let Ok(outcome) = self.resume().await {
                return outcome;
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    fn notify_end(&mut self, reason: &str) {
        (self.on_event)(ShareEvent::Ended {
            reason: reason.to_string(),
        });
    }

    async fn end(&mut self, relay: &mut Relay, reason: Option<String>) {
        if let Some(reason) = reason {
            self.notify_end(&reason);
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
