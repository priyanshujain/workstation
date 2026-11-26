use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const API_REVISION: u64 = 14;
const CONTROL_PROTOCOL: &str = "rex.control.v1";
const TERMINAL_PROTOCOL: &str = "rex.data.terminal.v1";
const BYTES_PROTOCOL: &str = "rex.data.bytes.v1";
const UPLOAD_CHUNK_BYTES: usize = 64 * 1024;
const TERMINAL: &str = "com.superlogical.terminal";
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const WELCOME: u8 = 0x81;
const REJECT: u8 = 0x82;

pub type Socket = WebSocketStream<UnixStream>;
pub type Events = mpsc::UnboundedReceiver<Value>;
type Replies = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>>;

pub struct Control {
    socket: PathBuf,
    outgoing: mpsc::UnboundedSender<Message>,
    replies: Replies,
    next_id: AtomicU64,
}

pub struct DataConnection {
    pub socket: Socket,
    pub nonce: u64,
}

async fn open(socket: &Path, path: &str, protocol: &str) -> Result<Socket> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("could not reach the Rex server at {}", socket.display()))?;
    let mut request = format!("ws://localhost{path}").into_client_request()?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", protocol.parse()?);
    let (socket, _) = tokio_tungstenite::client_async(request, stream).await?;
    Ok(socket)
}

fn text(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

impl Control {
    pub async fn connect(socket: &Path) -> Result<(Self, Events)> {
        let mut ws = open(socket, "/api/control", CONTROL_PROTOCOL).await?;
        ws.send(text(hello())).await?;
        tokio::time::timeout(HELLO_TIMEOUT, expect_welcome(&mut ws))
            .await
            .context("the Rex server did not answer hello")??;
        let (sink, stream) = ws.split();
        let (outgoing, queue) = mpsc::unbounded_channel();
        let (events_tx, events) = mpsc::unbounded_channel();
        let replies = Replies::default();
        tokio::spawn(write_all(queue, sink));
        tokio::spawn(read_all(stream, replies.clone(), events_tx));
        let control = Self {
            socket: socket.to_path_buf(),
            outgoing,
            replies,
            next_id: AtomicU64::new(1),
        };
        Ok((control, events))
    }

    pub async fn call(&self, method: &str, payload: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (reply_tx, reply) = oneshot::channel();
        self.replies.lock().unwrap().insert(id.clone(), reply_tx);
        let request = json!({"type": "request", "id": id, "method": method, "payload": payload});
        self.outgoing
            .send(text(request))
            .map_err(|_| anyhow!("the Rex control connection is closed"))?;
        reply
            .await
            .map_err(|_| anyhow!("the Rex control connection closed during {method}"))?
            .with_context(|| format!("rex {method}"))
    }

    pub async fn terminal(
        &self,
        session_id: &str,
        block_id: &str,
        method: &str,
        args: Value,
    ) -> Result<Value> {
        let payload = json!({"session_id": session_id, "block_id": block_id, "args": args});
        self.call(&format!("{TERMINAL}.{method}"), payload).await
    }

    pub async fn open_data(&self, session_id: &str, block_id: &str) -> Result<DataConnection> {
        let payload = json!({
            "session_id": session_id,
            "block_id": block_id,
            "data_ref_id": "terminal",
            "subprotocol": TERMINAL_PROTOCOL,
        });
        let reply = self.call("data.connect", payload).await?;
        let ticket = reply["ticket"]
            .as_str()
            .context("data.connect returned no ticket")?;
        let (socket, nonce) = self.redeem(ticket, TERMINAL_PROTOCOL).await?;
        Ok(DataConnection { socket, nonce })
    }

    pub async fn upload(
        &self,
        session_id: &str,
        block_id: &str,
        args: Value,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let reply = self
            .terminal(session_id, block_id, "upload_begin", args)
            .await?;
        let ticket = reply["ticket"]
            .as_str()
            .context("upload_begin returned no ticket")?;
        let (socket, _) = self.redeem(ticket, BYTES_PROTOCOL).await?;
        tokio::spawn(send_bytes(socket, bytes));
        Ok(())
    }

    async fn redeem(&self, ticket: &str, protocol: &str) -> Result<(Socket, u64)> {
        let mut hello = vec![0x01];
        hello.extend(URL_SAFE_NO_PAD.decode(ticket.trim_end_matches('='))?);
        let mut socket = open(&self.socket, "/api/data", protocol).await?;
        socket.send(Message::Binary(hello.into())).await?;
        let nonce = loop {
            match socket
                .next()
                .await
                .context("the Rex data connection closed")??
            {
                Message::Binary(bytes) => break parse_welcome(&bytes)?,
                Message::Close(frame) => bail!("the Rex data connection closed: {frame:?}"),
                _ => continue,
            }
        };
        Ok((socket, nonce))
    }
}

async fn send_bytes(mut socket: Socket, bytes: Vec<u8>) {
    for chunk in bytes.chunks(UPLOAD_CHUNK_BYTES) {
        if socket
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    let half_close = Message::Binary(Vec::new().into());
    if socket.send(half_close).await.is_err() {
        return;
    }
    while let Some(Ok(message)) = socket.next().await {
        if matches!(message, Message::Close(_)) {
            return;
        }
    }
}

pub fn upload_ref() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("no randomness: {error}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "upload:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

impl DataConnection {
    pub async fn close(mut self) {
        let frame = CloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        };
        let _ = self.socket.close(Some(frame)).await;
    }
}

fn hello() -> Value {
    json!({
        "type": "hello",
        "api_revision": API_REVISION,
        "client": {"kind": "cli", "name": "wsctl-share", "version": env!("CARGO_PKG_VERSION")},
    })
}

async fn expect_welcome(ws: &mut Socket) -> Result<()> {
    while let Some(message) = ws.next().await {
        if let Message::Text(body) = message? {
            let reply: Value = serde_json::from_str(&body)?;
            return match reply["type"].as_str() {
                Some("welcome") => Ok(()),
                _ => bail!("the Rex server refused hello: {reply}"),
            };
        }
    }
    bail!("the Rex server closed the control connection")
}

async fn write_all(
    mut queue: mpsc::UnboundedReceiver<Message>,
    mut sink: SplitSink<Socket, Message>,
) {
    while let Some(message) = queue.recv().await {
        if sink.send(message).await.is_err() {
            break;
        }
    }
    let _ = sink.close().await;
}

async fn read_all(
    mut stream: SplitStream<Socket>,
    replies: Replies,
    events: mpsc::UnboundedSender<Value>,
) {
    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(body) = message else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        match value["type"].as_str() {
            Some("response") => answer(&replies, value),
            Some("event") => {
                let _ = events.send(value);
            }
            _ => {}
        }
    }
    replies.lock().unwrap().clear();
}

fn answer(replies: &Replies, mut value: Value) {
    let Some(id) = value["id"].as_str().map(str::to_string) else {
        return;
    };
    let Some(reply) = replies.lock().unwrap().remove(&id) else {
        return;
    };
    let result = match value.get("error") {
        Some(error) => Err(anyhow!(
            "{}",
            error["message"].as_str().unwrap_or("request failed")
        )),
        None => Ok(value["payload"].take()),
    };
    let _ = reply.send(result);
}

fn parse_welcome(bytes: &[u8]) -> Result<u64> {
    match bytes.first() {
        Some(&WELCOME) if bytes.len() >= 32 => Ok(u64::from_le_bytes(bytes[24..32].try_into()?)),
        Some(&REJECT) => bail!(
            "Rex refused the data connection, reason {:#04x}",
            bytes.get(1).unwrap_or(&0)
        ),
        _ => bail!("unexpected Rex data handshake of {} bytes", bytes.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nonce_from_welcome() {
        let mut welcome = vec![WELCOME];
        welcome.extend([7; 16]);
        welcome.extend([0; 3]);
        welcome.extend(0u32.to_le_bytes());
        welcome.extend(9489415115013900158u64.to_le_bytes());
        assert_eq!(parse_welcome(&welcome).unwrap(), 9489415115013900158);
    }

    #[test]
    fn reports_reject_reason() {
        let error = parse_welcome(&[REJECT, 0x07]).unwrap_err();
        assert!(error.to_string().contains("0x07"));
    }
}
