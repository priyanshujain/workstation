use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::{Value, json};
use share_protocol::{
    ACCESS_JWT_HEADER, AGENT_PATH, AGENT_SECRET_HEADER, AgentMessage, Mode, RelayMessage, Share,
    ViewerMessage,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::access::AccessVerifier;
use crate::live::LiveShare;
use crate::store::unix_now;

const TEAM: &str = "team.example.com";
const AUDIENCE: &str = "audience-tag";
const SECRET: &str = "engine-secret";
const GUEST: &str = "Guest@Example.com";

struct SigningKey {
    encoding: EncodingKey,
    n: String,
    e: String,
}

fn signing_key() -> &'static SigningKey {
    static KEY: OnceLock<SigningKey> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let pem = private.to_pkcs1_pem(Default::default()).unwrap();
        SigningKey {
            encoding: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            n: URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
            e: URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
        }
    })
}

fn token_with(kid: &str, claims: Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    encode(&header, &claims, &signing_key().encoding).unwrap()
}

fn token(email: &str) -> String {
    token_with(
        "k1",
        json!({ "email": email, "aud": [AUDIENCE], "iss": format!("https://{TEAM}"), "exp": unix_now() + 300 }),
    )
}

async fn certs_server() -> String {
    let key = signing_key();
    let certs =
        json!({ "keys": [{ "kid": "k1", "kty": "RSA", "alg": "RS256", "n": key.n, "e": key.e }] });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/cdn-cgi/access/certs",
        listener.local_addr().unwrap()
    );
    let app = axum::Router::new().route(
        "/cdn-cgi/access/certs",
        get(move || async move { Json(certs) }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn verifier() -> AccessVerifier {
    AccessVerifier::new(TEAM, AUDIENCE, certs_server().await)
}

fn share(mode: Mode, expires_at_unix: u64) -> Share {
    Share {
        session_id: "session-1".into(),
        session_label: "work".into(),
        block_id: "block-1".into(),
        label: "claude".into(),
        host: "mac".into(),
        mode,
        guests: vec![GUEST.to_lowercase()],
        expires_at_unix,
        cols: 100,
        rows: 30,
    }
}

#[tokio::test]
async fn accepts_valid_access_token() {
    assert_eq!(verifier().await.verify(&token(GUEST)).await.unwrap(), GUEST);
}

#[tokio::test]
async fn rejects_bad_access_tokens() {
    let verifier = verifier().await;
    let issuer = format!("https://{TEAM}");
    let bad = [
        token_with(
            "k1",
            json!({ "email": GUEST, "aud": ["other"], "iss": issuer, "exp": unix_now() + 300 }),
        ),
        token_with(
            "k1",
            json!({ "email": GUEST, "aud": [AUDIENCE], "iss": "https://evil.example.com", "exp": unix_now() + 300 }),
        ),
        token_with(
            "k1",
            json!({ "email": GUEST, "aud": [AUDIENCE], "iss": issuer, "exp": unix_now() - 3600 }),
        ),
        token_with(
            "k2",
            json!({ "email": GUEST, "aud": [AUDIENCE], "iss": issuer, "exp": unix_now() + 300 }),
        ),
        "not.a.token".to_string(),
    ];
    for token in bad {
        assert!(verifier.verify(&token).await.is_err(), "accepted {token}");
    }
}

#[test]
fn admits_only_listed_guests() {
    let (engine, _) = mpsc::unbounded_channel();
    let live = LiveShare::new("id".into(), share(Mode::View, unix_now() + 60), engine);
    assert!(live.admits("guest@example.com"));
    assert!(live.admits("GUEST@EXAMPLE.COM"));
    assert!(!live.admits("stranger@example.com"));
}

#[test]
fn forwards_input_only_in_input_mode() {
    let (engine, mut received) = mpsc::unbounded_channel();
    let view = LiveShare::new(
        "view".into(),
        share(Mode::View, unix_now() + 60),
        engine.clone(),
    );
    assert!(!view.input(1, GUEST, "ls\r".into()));
    assert!(received.try_recv().is_err());
    let input = LiveShare::new("input".into(), share(Mode::Input, unix_now() + 60), engine);
    assert!(input.input(1, GUEST, "ls\r".into()));
    let forwarded = RelayMessage::Input {
        viewer_id: 1,
        email: GUEST.into(),
        data: "ls\r".into(),
    };
    assert_eq!(received.try_recv().unwrap(), forwarded);
}

#[test]
fn share_expires_at_deadline() {
    let (engine, _) = mpsc::unbounded_channel();
    let live = LiveShare::new("id".into(), share(Mode::View, 1_000), engine);
    assert!(!live.expired(999));
    assert!(live.expired(1_000));
}

struct Running {
    viewer: String,
    agent: String,
    db: Arc<tempfile::TempDir>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start_relay() -> Running {
    start_relay_on(Arc::new(tempfile::tempdir().unwrap())).await
}

async fn start_relay_on(db: Arc<tempfile::TempDir>) -> Running {
    let viewers = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let viewer = viewers.local_addr().unwrap().to_string();
    let agent = agents.local_addr().unwrap().to_string();
    let config = crate::RelayConfig {
        viewer_listen: viewer.clone(),
        agent_listen: agent.clone(),
        public_url: "https://share.example.com/".into(),
        secret: SECRET.into(),
        access_team_domain: TEAM.into(),
        access_audience: AUDIENCE.into(),
        access_certs_url: Some(certs_server().await),
        db_url: format!("file:{}", db.path().join("share.db").display()),
        db_token_file: None,
        #[cfg(debug_assertions)]
        dev_email: None,
    };
    let mut relay = crate::relay::Relay::new(config).await.unwrap();
    relay.engine_grace = Duration::from_secs(2);
    let server = tokio::spawn(crate::serve_relay(relay, viewers, agents));
    Running {
        viewer,
        agent,
        db,
        server,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: String, header: &str, value: &str) -> Result<Socket, tungstenite::Error> {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        tungstenite::http::HeaderName::from_bytes(header.as_bytes()).unwrap(),
        value.parse().unwrap(),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

async fn send_json(socket: &mut Socket, value: &impl serde::Serialize) {
    socket
        .send(Message::text(serde_json::to_string(value).unwrap()))
        .await
        .unwrap();
}

async fn next(socket: &mut Socket) -> Message {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn next_json<T: serde::de::DeserializeOwned>(socket: &mut Socket) -> T {
    serde_json::from_str(next(socket).await.to_text().unwrap()).unwrap()
}

async fn register(running: &Running, share: Share) -> (Socket, String) {
    let mut engine = connect(
        format!("ws://{}{AGENT_PATH}", running.agent),
        AGENT_SECRET_HEADER,
        SECRET,
    )
    .await
    .unwrap();
    send_json(&mut engine, &AgentMessage::Register { share }).await;
    let RelayMessage::Registered { share_id, url } = next_json(&mut engine).await else {
        panic!("not registered")
    };
    assert_eq!(url, format!("https://share.example.com/s/{share_id}"));
    assert!(URL_SAFE_NO_PAD.decode(&share_id).unwrap().len() >= 16);
    (engine, share_id)
}

async fn status(running: &Running, path: &str, email: &str) -> u16 {
    let response = reqwest::Client::new()
        .get(format!("http://{}{path}", running.viewer))
        .header(ACCESS_JWT_HEADER, token(email))
        .send()
        .await
        .unwrap();
    response.status().as_u16()
}

async fn prompts(running: &Running) -> Vec<(String, String)> {
    let database = libsql::Builder::new_local(running.db.path().join("share.db"))
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query("SELECT email, prompt FROM prompts ORDER BY id", ())
        .await
        .unwrap();
    let mut found = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        found.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    found
}

#[tokio::test]
async fn rejects_engine_without_secret() {
    let running = start_relay().await;
    let refused = connect(
        format!("ws://{}{AGENT_PATH}", running.agent),
        AGENT_SECRET_HEADER,
        "wrong",
    )
    .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn gates_viewer_requests() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    assert_eq!(status(&running, &format!("/s/{id}"), GUEST).await, 200);
    assert_eq!(status(&running, "/assets/xterm.js", GUEST).await, 200);
    assert_eq!(
        status(&running, &format!("/s/{id}"), "stranger@example.com").await,
        403
    );
    assert_eq!(status(&running, "/s/unknown", GUEST).await, 404);
    let anonymous = reqwest::get(format!("http://{}/s/{id}", running.viewer))
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 403);
}

#[tokio::test]
async fn relays_screen_bytes_and_input() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::Input, unix_now() + 60)).await;
    send_json(&mut engine, &AgentMessage::Status { status: None }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut viewer = connect(
        format!("ws://{}/s/{id}/ws", running.viewer),
        ACCESS_JWT_HEADER,
        &token(GUEST),
    )
    .await
    .unwrap();
    assert!(matches!(
        next_json(&mut viewer).await,
        ViewerMessage::Hello { .. }
    ));
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Resize {
            cols: 100,
            rows: 30
        }
    );
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Status { status: None }
    );
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Viewers {
            emails: vec![GUEST.into()]
        }
    );

    let RelayMessage::ViewerJoined { viewer_id, email } = next_json(&mut engine).await else {
        panic!("no join")
    };
    assert_eq!(email, GUEST);
    engine
        .send(Message::binary(b"before screen".to_vec()))
        .await
        .unwrap();
    send_json(
        &mut engine,
        &AgentMessage::Screen {
            viewer_id,
            vt: "screen".into(),
        },
    )
    .await;
    engine
        .send(Message::binary(b"live".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Screen {
            vt: "screen".into()
        }
    );
    assert_eq!(next(&mut viewer).await.into_data().as_ref(), b"live");

    send_json(
        &mut viewer,
        &json!({ "type": "input", "data": "hi there\r" }),
    )
    .await;
    let forwarded = RelayMessage::Input {
        viewer_id,
        email: GUEST.into(),
        data: "hi there\r".into(),
    };
    assert_eq!(next_json::<RelayMessage>(&mut engine).await, forwarded);
    assert_eq!(
        prompts(&running).await,
        [(GUEST.to_string(), "hi there".to_string())]
    );

    send_json(
        &mut engine,
        &AgentMessage::Ended {
            reason: "stopped".into(),
        },
    )
    .await;
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Ended {
            reason: "stopped".into()
        }
    );
    assert_eq!(status(&running, &format!("/s/{id}"), GUEST).await, 404);
}

#[tokio::test]
async fn stops_engine_at_expiry() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::View, unix_now() + 2)).await;
    let stop: RelayMessage = serde_json::from_str(
        tokio::time::timeout(Duration::from_secs(4), engine.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .to_text()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stop,
        RelayMessage::Stop {
            reason: "expired".into()
        }
    );
    assert_eq!(status(&running, &format!("/s/{id}"), GUEST).await, 404);
}

async fn engine_socket(running: &Running) -> Socket {
    connect(
        format!("ws://{}{AGENT_PATH}", running.agent),
        AGENT_SECRET_HEADER,
        SECRET,
    )
    .await
    .unwrap()
}

async fn joined_viewer(running: &Running, id: &str) -> Socket {
    let mut viewer = connect(
        format!("ws://{}/s/{id}/ws", running.viewer),
        ACCESS_JWT_HEADER,
        &token(GUEST),
    )
    .await
    .unwrap();
    for _ in 0..3 {
        next(&mut viewer).await;
    }
    viewer
}

async fn end_reasons(running: &Running) -> Vec<Option<String>> {
    let database = libsql::Builder::new_local(running.db.path().join("share.db"))
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query("SELECT end_reason FROM shares", ())
        .await
        .unwrap();
    let mut reasons = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        reasons.push(row.get(0).unwrap());
    }
    reasons
}

async fn end_reason(running: &Running) -> Option<String> {
    let database = libsql::Builder::new_local(running.db.path().join("share.db"))
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query("SELECT end_reason FROM shares", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn resumes_share_after_engine_drop() {
    let running = start_relay().await;
    let (engine, id) = register(&running, share(Mode::Input, unix_now() + 60)).await;
    let mut viewer = joined_viewer(&running, &id).await;
    drop(engine);
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Reconnecting
    );
    send_json(&mut viewer, &json!({ "type": "input", "data": "lost\r" })).await;

    let mut engine = engine_socket(&running).await;
    send_json(
        &mut engine,
        &AgentMessage::Resume {
            share_id: id.clone(),
        },
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::Resumed {
            share_id: id.clone()
        }
    );
    let RelayMessage::ViewerJoined { viewer_id, email } = next_json(&mut engine).await else {
        panic!("no rejoin")
    };
    assert_eq!(email, GUEST);
    assert!(matches!(
        next_json(&mut viewer).await,
        ViewerMessage::Hello { .. }
    ));
    send_json(
        &mut engine,
        &AgentMessage::Screen {
            viewer_id,
            vt: "again".into(),
        },
    )
    .await;
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Screen { vt: "again".into() }
    );

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(status(&running, &format!("/s/{id}"), GUEST).await, 200);
    assert!(prompts(&running).await.is_empty());
}

#[tokio::test]
async fn ends_share_when_grace_runs_out() {
    let running = start_relay().await;
    let (engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut viewer = joined_viewer(&running, &id).await;
    drop(engine);
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Reconnecting
    );
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Ended {
            reason: "engine lost".into()
        }
    );
    assert_eq!(end_reason(&running).await.as_deref(), Some("engine lost"));

    let mut engine = engine_socket(&running).await;
    send_json(
        &mut engine,
        &AgentMessage::Resume {
            share_id: id.clone(),
        },
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::ResumeRejected { share_id: id }
    );
}

#[tokio::test]
async fn rejects_resume_of_live_share() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut other = engine_socket(&running).await;
    send_json(
        &mut other,
        &AgentMessage::Resume {
            share_id: id.clone(),
        },
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut other).await,
        RelayMessage::ResumeRejected { share_id: id }
    );
}

#[tokio::test]
async fn closes_viewer_that_falls_behind() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut slow = joined_viewer(&running, &id).await;
    let mut fast = joined_viewer(&running, &id).await;
    next(&mut slow).await;
    for _ in 0..2 {
        let RelayMessage::ViewerJoined { viewer_id, .. } = next_json(&mut engine).await else {
            panic!("no join")
        };
        send_json(
            &mut engine,
            &AgentMessage::Screen {
                viewer_id,
                vt: String::new(),
            },
        )
        .await;
    }
    let chunk = vec![b'x'; 256 * 1024];
    let total = 64 * chunk.len();
    let reader = tokio::spawn(async move {
        let mut received = 0;
        while received < total {
            if let Message::Binary(bytes) = next(&mut fast).await {
                received += bytes.len();
            }
        }
        received
    });
    for _ in 0..64 {
        engine.send(Message::binary(chunk.clone())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), reader)
            .await
            .unwrap()
            .unwrap(),
        total
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let reason = loop {
        match tokio::time::timeout(Duration::from_secs(10), slow.next())
            .await
            .unwrap()
        {
            Some(Ok(Message::Close(Some(frame)))) => break frame.reason.to_string(),
            Some(Ok(_)) => {}
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_eq!(reason, "viewer fell behind; reload to catch up");
}

#[tokio::test]
async fn restores_open_shares_after_restart() {
    let first = start_relay().await;
    let (_old_engine, id) = register(&first, share(Mode::Input, unix_now() + 60)).await;
    first.server.abort();

    let second = start_relay_on(first.db.clone()).await;
    let mut viewer = connect(
        format!("ws://{}/s/{id}/ws", second.viewer),
        ACCESS_JWT_HEADER,
        &token(GUEST),
    )
    .await
    .unwrap();
    let ViewerMessage::Hello {
        share: restored, ..
    } = next_json(&mut viewer).await
    else {
        panic!("no hello")
    };
    assert_eq!(restored, share(Mode::Input, restored.expires_at_unix));
    assert_eq!(
        status(&second, &format!("/s/{id}"), "stranger@example.com").await,
        403
    );

    let mut engine = engine_socket(&second).await;
    send_json(
        &mut engine,
        &AgentMessage::Resume {
            share_id: id.clone(),
        },
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::Resumed {
            share_id: id.clone()
        }
    );
    let RelayMessage::ViewerJoined { email, .. } = next_json(&mut engine).await else {
        panic!("no rejoin")
    };
    assert_eq!(email, GUEST);
}

#[tokio::test]
async fn ends_restored_share_without_engine() {
    let first = start_relay().await;
    let (_old_engine, _) = register(&first, share(Mode::View, unix_now() + 60)).await;
    first.server.abort();
    let second = start_relay_on(first.db.clone()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let reasons = end_reasons(&second).await;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.as_deref() == Some("engine lost"))
    );
}
