use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use share_protocol::{
    AGENT_PATH, AGENT_SECRET_HEADER, AgentMessage, Mode, ProgramStatus, RelayMessage, Share, Theme,
};
use share_relay::{RelayConfig, serve};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const SECRET: &str = "dev-secret";
const AGENT: &str = "127.0.0.1:7702";
const EMAIL: &str = "guest@example.com";

fn config(db_url: String) -> RelayConfig {
    RelayConfig {
        viewer_listen: "127.0.0.1:7701".into(),
        agent_listen: AGENT.into(),
        public_url: "http://127.0.0.1:7701".into(),
        secret: SECRET.into(),
        access_team_domain: "unused.example.com".into(),
        access_audience: "unused".into(),
        access_certs_url: None,
        db_url,
        db_token_file: None,
        #[cfg(debug_assertions)]
        dev_email: Some(EMAIL.into()),
    }
}

fn share() -> Share {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    Share {
        session_id: "dev-session".into(),
        session_label: "dev".into(),
        block_id: "dev-block".into(),
        label: "fake agent".into(),
        host: "localhost".into(),
        mode: Mode::Input,
        guests: vec![EMAIL.into()],
        expires_at_unix: now + 600,
        cols: 80,
        rows: 20,
    }
}

fn text(message: &AgentMessage) -> Message {
    Message::text(serde_json::to_string(message).unwrap())
}

fn greeting() -> Vec<AgentMessage> {
    let theme = Theme {
        foreground: Some("#e0e0e0".into()),
        background: Some("#202040".into()),
        cursor: None,
        palette: vec![],
    };
    let status = ProgramStatus {
        state: "blocked".into(),
        kind: Some("permission".into()),
        app: Some("claude".into()),
        title: Some("Run tests?".into()),
        msg: Some("waiting for approval".into()),
        progress: None,
    };
    vec![
        AgentMessage::Theme { theme },
        AgentMessage::Status {
            status: Some(status),
        },
    ]
}

#[tokio::main]
async fn main() {
    let db_url = std::env::args().nth(1).expect("usage: dev_relay <db-url>");
    std::thread::spawn(move || serve(config(db_url)).unwrap());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut request = format!("ws://{AGENT}{AGENT_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AGENT_SECRET_HEADER, SECRET.parse().unwrap());
    let (mut engine, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    engine
        .send(text(&AgentMessage::Register { share: share() }))
        .await
        .unwrap();
    for message in greeting() {
        engine.send(text(&message)).await.unwrap();
    }
    while let Some(Ok(Message::Text(frame))) = engine.next().await {
        println!("engine got {frame}");
        match serde_json::from_str(&frame).unwrap() {
            RelayMessage::ViewerJoined { viewer_id, .. } => {
                let vt = "\x1b[1;32mfake agent ready\x1b[0m\r\n$ ".to_string();
                engine
                    .send(text(&AgentMessage::Screen { viewer_id, vt }))
                    .await
                    .unwrap();
            }
            RelayMessage::Input { data, .. } => {
                engine
                    .send(Message::binary(data.replace('\r', "\r\n$ ").into_bytes()))
                    .await
                    .unwrap();
            }
            _ => {}
        }
    }
}
