use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use share_protocol::{
    AGENT_PATH, AGENT_SECRET_HEADER, AgentMessage, ManageReply, ManageRequest, Mode, RelayMessage,
    Share, ViewerMessage,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::live::LiveShare;
use crate::store::unix_now;

const SECRET: &str = "engine-secret";
const GUEST: &str = "guest@example.com";
const ORIGIN: &str = "https://share.example.com";

fn share(mode: Mode, expires_at_unix: u64) -> Share {
    Share {
        session_id: "session-1".into(),
        session_label: "work".into(),
        block_id: "block-1".into(),
        label: "claude".into(),
        host: "mac".into(),
        mode,
        guests: vec!["Guest@Example.com".into()],
        expires_at_unix,
        cols: 100,
        rows: 30,
    }
}

#[test]
fn admits_only_listed_guests() {
    let (engine, _) = mpsc::unbounded_channel();
    let live = LiveShare::new("id".into(), share(Mode::View, unix_now() + 60), engine);
    assert!(live.admits("guest@example.com"));
    assert!(live.admits("Guest@Example.com"));
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
    passwords: Arc<Mutex<HashMap<String, String>>>,
}

async fn start_relay() -> Running {
    start_relay_on(Arc::new(tempfile::tempdir().unwrap()), Arc::default()).await
}

async fn restart(running: &Running) -> Running {
    running.server.abort();
    start_relay_on(running.db.clone(), running.passwords.clone()).await
}

async fn start_relay_on(
    db: Arc<tempfile::TempDir>,
    passwords: Arc<Mutex<HashMap<String, String>>>,
) -> Running {
    let viewers = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let viewer = viewers.local_addr().unwrap().to_string();
    let agent = agents.local_addr().unwrap().to_string();
    let config = crate::RelayConfig {
        viewer_listen: viewer.clone(),
        agent_listen: agent.clone(),
        public_url: "https://share.example.com/".into(),
        secret: SECRET.into(),
        db_url: format!("file:{}", db.path().join("share.db").display()),
        db_token_file: None,
        trust_proxy_header: None,
    };
    let mut relay = crate::relay::Relay::new(config).await.unwrap();
    relay.engine_grace = Duration::from_secs(5);
    let server = tokio::spawn(crate::serve_relay(relay, viewers, agents));
    Running {
        viewer,
        agent,
        db,
        server,
        passwords,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: String, header: &str, value: &str) -> Result<Socket, tungstenite::Error> {
    connect_with(url, &[(header, value)]).await
}

async fn connect_with(url: String, headers: &[(&str, &str)]) -> Result<Socket, tungstenite::Error> {
    let mut request = url.into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(
            tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
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
    tokio::time::timeout(Duration::from_secs(10), socket.next())
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
    let RelayMessage::Registered {
        share_id,
        url,
        credentials,
        existing,
    } = next_json(&mut engine).await
    else {
        panic!("not registered")
    };
    assert_eq!(url, format!("https://share.example.com/s/{share_id}"));
    assert!(URL_SAFE_NO_PAD.decode(&share_id).unwrap().len() >= 16);
    let mut passwords = running.passwords.lock().unwrap();
    for credential in &credentials {
        assert!(credential.password.len() >= 20);
        assert!(!passwords.contains_key(&credential.email));
        passwords.insert(credential.email.clone(), credential.password.clone());
    }
    for email in &existing {
        assert!(passwords.contains_key(email));
    }
    assert!(passwords.contains_key(GUEST));
    drop(passwords);
    (engine, share_id)
}

fn password(running: &Running) -> String {
    running.passwords.lock().unwrap()[GUEST].clone()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn login(running: &Running, email: &str, password: &str) -> (u16, Option<String>) {
    let body = format!("email={}&password={password}", email.replace('@', "%40"));
    let response = client()
        .post(format!("http://{}/login", running.viewer))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .unwrap();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|value| value.to_str().unwrap().to_string());
    (response.status().as_u16(), cookie)
}

async fn sign_in(running: &Running) -> String {
    let (status, cookie) = login(running, GUEST, &password(running)).await;
    assert_eq!(status, 303);
    cookie_pair(&cookie.unwrap())
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

async fn page(running: &Running, path: &str, cookie: Option<&str>) -> (u16, String) {
    let mut request = client().get(format!("http://{}{path}", running.viewer));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

async fn share_status(running: &Running, id: &str) -> u16 {
    let cookie = sign_in(running).await;
    match viewer_socket_with(running, id, &cookie).await {
        Ok(_) => 200,
        refused => refused_status(refused),
    }
}

async fn viewer_socket(running: &Running, id: &str) -> Socket {
    let cookie = sign_in(running).await;
    viewer_socket_with(running, id, &cookie).await.unwrap()
}

async fn viewer_socket_with(
    running: &Running,
    id: &str,
    cookie: &str,
) -> Result<Socket, tungstenite::Error> {
    connect_with(
        format!("ws://{}/s/{id}/ws", running.viewer),
        &[("cookie", cookie), ("origin", ORIGIN)],
    )
    .await
}

fn refused_status(result: Result<Socket, tungstenite::Error>) -> u16 {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        other => panic!("expected refusal, got {:?}", other.map(|_| ())),
    }
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
async fn relays_screen_bytes_and_input() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::Input, unix_now() + 60)).await;
    send_json(&mut engine, &AgentMessage::Status { status: None }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut viewer = viewer_socket(&running, &id).await;
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
    assert_eq!(share_status(&running, &id).await, 404);
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
    assert_eq!(share_status(&running, &id).await, 404);
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
    let mut viewer = viewer_socket(running, id).await;
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

    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(share_status(&running, &id).await, 200);
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
    let cookie = sign_in(&first).await;
    let second = restart(&first).await;
    let mut viewer = viewer_socket_with(&second, &id, &cookie).await.unwrap();
    let ViewerMessage::Hello {
        share: restored, ..
    } = next_json(&mut viewer).await
    else {
        panic!("no hello")
    };
    let mut expected = share(Mode::Input, restored.expires_at_unix);
    expected.guests = vec![GUEST.into()];
    assert_eq!(restored, expected);
    assert_eq!(
        login(&second, "stranger@example.com", &password(&second))
            .await
            .0,
        401
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
    let second = restart(&first).await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    let reasons = end_reasons(&second).await;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.as_deref() == Some("engine lost"))
    );
}

async fn credential_hashes(running: &Running) -> Vec<String> {
    let database = libsql::Builder::new_local(running.db.path().join("share.db"))
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    let mut rows = connection
        .query("SELECT password_hash FROM guests", ())
        .await
        .unwrap();
    let mut hashes = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        hashes.push(row.get(0).unwrap());
    }
    hashes
}

#[tokio::test]
async fn login_sets_a_site_wide_session_cookie() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    for path in ["/".to_string(), format!("/s/{id}")] {
        let (status, body) = page(&running, &path, None).await;
        assert_eq!(status, 200);
        assert!(body.contains("Sign in"));
    }
    let (_, body) = page(&running, "/s/x\"><script>", None).await;
    assert!(body.contains(r#"name="next" value="/""#));

    let (status, set_cookie) = login(&running, "Guest@Example.COM", &password(&running)).await;
    assert_eq!(status, 303);
    let set_cookie = set_cookie.unwrap();
    for attribute in [
        "Path=/;",
        "HttpOnly",
        "SameSite=Strict",
        "Secure",
        "Max-Age=",
    ] {
        assert!(
            set_cookie.contains(attribute),
            "{set_cookie} lacks {attribute}"
        );
    }
    let cookie = cookie_pair(&set_cookie);
    for path in ["/".to_string(), format!("/s/{id}")] {
        let (status, body) = page(&running, &path, Some(&cookie)).await;
        assert_eq!(status, 200);
        assert!(body.contains("id=\"terminal\""));
    }
}

#[tokio::test]
async fn login_returns_to_the_requested_tab() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    for (next, expected) in [
        (format!("/s/{id}"), format!("/s/{id}")),
        ("https://evil.example.com".to_string(), "/".to_string()),
        ("//evil.example.com".to_string(), "/".to_string()),
    ] {
        let body = format!(
            "email={}&password={}&next={}",
            GUEST.replace('@', "%40"),
            password(&running),
            next.replace(':', "%3A").replace('/', "%2F")
        );
        let response = client()
            .post(format!("http://{}/login", running.viewer))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["location"], expected.as_str());
    }
}

#[tokio::test]
async fn one_password_covers_every_share_for_a_guest() {
    let running = start_relay().await;
    let (_first_engine, first) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let (_second_engine, second) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let cookie = sign_in(&running).await;
    assert!(viewer_socket_with(&running, &first, &cookie).await.is_ok());
    assert!(viewer_socket_with(&running, &second, &cookie).await.is_ok());

    let mut private = share(Mode::View, unix_now() + 60);
    private.guests = vec!["other@example.com".into()];
    let (_private_engine, private) = register(&running, private).await;
    assert_eq!(
        refused_status(viewer_socket_with(&running, &private, &cookie).await),
        404
    );
}

#[tokio::test]
async fn rejects_wrong_password_and_unknown_email() {
    let running = start_relay().await;
    register(&running, share(Mode::View, unix_now() + 60)).await;
    let (status, cookie) = login(&running, GUEST, "wrong-password").await;
    assert_eq!((status, cookie), (401, None));
    let (status, cookie) = login(&running, "stranger@example.com", &password(&running)).await;
    assert_eq!((status, cookie), (401, None));
}

#[tokio::test]
async fn rate_limits_failed_logins() {
    let running = start_relay().await;
    register(&running, share(Mode::View, unix_now() + 60)).await;
    for _ in 0..crate::limits::MAX_FAILURES {
        assert_eq!(login(&running, GUEST, "wrong-password").await.0, 401);
    }
    assert_eq!(login(&running, GUEST, &password(&running)).await.0, 429);
}

#[tokio::test]
async fn socket_needs_session_cookie_and_matching_origin() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let url = format!("ws://{}/s/{id}/ws", running.viewer);
    assert_eq!(
        refused_status(connect_with(url.clone(), &[("origin", ORIGIN)]).await),
        401
    );
    let cookie = sign_in(&running).await;
    let wrong_origin = connect_with(
        url.clone(),
        &[("cookie", &cookie), ("origin", "https://evil.example.com")],
    )
    .await;
    assert_eq!(refused_status(wrong_origin), 403);
    assert_eq!(
        refused_status(connect_with(url, &[("cookie", &cookie)]).await),
        403
    );
    assert!(viewer_socket_with(&running, &id, &cookie).await.is_ok());
}

#[tokio::test]
async fn logout_ends_the_session() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let cookie = sign_in(&running).await;
    let response = client()
        .post(format!("http://{}/logout", running.viewer))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        refused_status(viewer_socket_with(&running, &id, &cookie).await),
        401
    );
}

#[tokio::test]
async fn session_and_credentials_survive_restart() {
    let first = start_relay().await;
    let (_old_engine, id) = register(&first, share(Mode::View, unix_now() + 60)).await;
    let cookie = sign_in(&first).await;
    let second = restart(&first).await;
    let (_, body) = page(&second, &format!("/s/{id}"), Some(&cookie)).await;
    assert!(body.contains("id=\"terminal\""));
    assert_eq!(login(&second, GUEST, &password(&second)).await.0, 303);
}

#[tokio::test]
async fn stores_only_password_hashes() {
    let running = start_relay().await;
    register(&running, share(Mode::View, unix_now() + 60)).await;
    let password = password(&running);
    let hashes = credential_hashes(&running).await;
    assert_eq!(hashes.len(), 1);
    assert!(hashes[0].starts_with("$argon2id$"));
    for entry in std::fs::read_dir(running.db.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(password.len())
                .any(|window| window == password.as_bytes())
        );
    }
}

async fn drop_table(running: &Running, table: &str) {
    let database = libsql::Builder::new_local(running.db.path().join("share.db"))
        .build()
        .await
        .unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute(&format!("DROP TABLE {table}"), ())
        .await
        .unwrap();
}

#[tokio::test]
async fn register_fails_when_credentials_cannot_be_saved() {
    let running = start_relay().await;
    drop_table(&running, "guests").await;
    let mut engine = engine_socket(&running).await;
    send_json(
        &mut engine,
        &AgentMessage::Register {
            share: share(Mode::View, unix_now() + 60),
        },
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::Stop {
            reason: "the relay could not save the share".into()
        }
    );
}

#[tokio::test]
async fn login_reports_an_unavailable_database() {
    let running = start_relay().await;
    register(&running, share(Mode::View, unix_now() + 60)).await;
    drop_table(&running, "guests").await;
    assert_eq!(login(&running, GUEST, &password(&running)).await.0, 503);
}

#[tokio::test]
async fn ends_share_when_input_cannot_be_recorded() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::Input, unix_now() + 60)).await;
    let mut viewer = joined_viewer(&running, &id).await;
    let RelayMessage::ViewerJoined { .. } = next_json(&mut engine).await else {
        panic!("no join")
    };
    drop_table(&running, "inputs").await;
    send_json(
        &mut viewer,
        &json!({ "type": "input", "data": "rm -rf /\r" }),
    )
    .await;
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::Stop {
            reason: crate::relay::AUDIT_FAILED.into()
        }
    );
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Ended {
            reason: crate::relay::AUDIT_FAILED.into()
        }
    );
    assert_eq!(share_status(&running, &id).await, 404);
}

async fn manage(running: &Running, request: ManageRequest) -> ManageReply {
    let mut engine = engine_socket(running).await;
    send_json(&mut engine, &AgentMessage::Manage { request }).await;
    let RelayMessage::Managed { reply } = next_json(&mut engine).await else {
        panic!("no manage reply")
    };
    reply
}

async fn lobby(running: &Running) -> Socket {
    let cookie = sign_in(running).await;
    connect_with(
        format!("ws://{}/events", running.viewer),
        &[("cookie", &cookie), ("origin", ORIGIN)],
    )
    .await
    .unwrap()
}

async fn next_tabs(socket: &mut Socket) -> Vec<String> {
    let ViewerMessage::Tabs { tabs } = next_json(socket).await else {
        panic!("no tabs")
    };
    tabs.into_iter().map(|tab| tab.id).collect()
}

#[tokio::test]
async fn lobby_follows_the_guest_shares() {
    let running = start_relay().await;
    let (_first_engine, first) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut lobby = lobby(&running).await;
    assert_eq!(next_tabs(&mut lobby).await, std::slice::from_ref(&first));
    let (mut second_engine, second) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut both = vec![first.clone(), second.clone()];
    both.sort();
    let mut listed = next_tabs(&mut lobby).await;
    listed.sort();
    assert_eq!(listed, both);
    send_json(
        &mut second_engine,
        &AgentMessage::Ended {
            reason: "done".into(),
        },
    )
    .await;
    assert_eq!(next_tabs(&mut lobby).await, [first]);
}

#[tokio::test]
async fn manage_lists_shares_and_guests() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let _viewer = joined_viewer(&running, &id).await;
    let ManageReply::State { guests, shares } = manage(&running, ManageRequest::State).await else {
        panic!("no state")
    };
    assert_eq!(guests, [GUEST]);
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].share_id, id);
    assert_eq!(shares[0].share.guests, [GUEST]);
    assert_eq!(shares[0].watching, [GUEST]);
}

#[tokio::test]
async fn adds_guests_to_a_running_share() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let reply = manage(
        &running,
        ManageRequest::AddGuests {
            share_id: id.clone(),
            guests: vec![GUEST.into(), " New@Example.com ".into()],
        },
    )
    .await;
    let ManageReply::Credentials {
        credentials,
        existing,
    } = reply
    else {
        panic!("no credentials")
    };
    assert_eq!(existing, [GUEST]);
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].email, "new@example.com");
    let (status, cookie) = login(&running, "new@example.com", &credentials[0].password).await;
    assert_eq!(status, 303);
    let cookie = cookie_pair(&cookie.unwrap());
    assert!(viewer_socket_with(&running, &id, &cookie).await.is_ok());
}

#[tokio::test]
async fn removes_a_guest_from_one_share_or_all() {
    let running = start_relay().await;
    let mut shared = share(Mode::View, unix_now() + 60);
    shared.guests.push("other@example.com".into());
    let (mut first_engine, first) = register(&running, shared).await;
    let (mut second_engine, second) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let mut viewer = joined_viewer(&running, &first).await;
    let RelayMessage::ViewerJoined { .. } = next_json(&mut first_engine).await else {
        panic!("no join")
    };

    let reply = manage(
        &running,
        ManageRequest::RemoveGuest {
            email: GUEST.into(),
            share_id: Some(first.clone()),
        },
    )
    .await;
    assert_eq!(reply, ManageReply::Done);
    assert_eq!(
        next_json::<ViewerMessage>(&mut viewer).await,
        ViewerMessage::Ended {
            reason: crate::live::ACCESS_REMOVED.into()
        }
    );
    let RelayMessage::ViewerLeft { email, .. } = next_json(&mut first_engine).await else {
        panic!("no leave")
    };
    assert_eq!(email, GUEST);
    assert_eq!(share_status(&running, &first).await, 404);
    assert_eq!(share_status(&running, &second).await, 200);
    let RelayMessage::ViewerJoined { .. } = next_json(&mut second_engine).await else {
        panic!("no join")
    };

    manage(
        &running,
        ManageRequest::RemoveGuest {
            email: GUEST.into(),
            share_id: None,
        },
    )
    .await;
    loop {
        match next_json::<RelayMessage>(&mut second_engine).await {
            RelayMessage::Stop { reason } => break assert_eq!(reason, "no guests are left"),
            _ => continue,
        }
    }
    let ManageReply::State { shares, .. } = manage(&running, ManageRequest::State).await else {
        panic!("no state")
    };
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].share.guests, ["other@example.com"]);
}

#[tokio::test]
async fn reset_password_replaces_the_old_one() {
    let running = start_relay().await;
    register(&running, share(Mode::View, unix_now() + 60)).await;
    let old = password(&running);
    let ManageReply::Credentials { credentials, .. } = manage(
        &running,
        ManageRequest::ResetPassword {
            email: GUEST.into(),
        },
    )
    .await
    else {
        panic!("no credentials")
    };
    assert_eq!(login(&running, GUEST, &old).await.0, 401);
    assert_eq!(
        login(&running, GUEST, &credentials[0].password).await.0,
        303
    );
}

#[tokio::test]
async fn forget_guest_ends_access_and_account() {
    let running = start_relay().await;
    let (_engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let cookie = sign_in(&running).await;
    manage(
        &running,
        ManageRequest::ForgetGuest {
            email: GUEST.into(),
        },
    )
    .await;
    assert_eq!(
        refused_status(viewer_socket_with(&running, &id, &cookie).await),
        401
    );
    assert_eq!(login(&running, GUEST, &password(&running)).await.0, 401);
    let ManageReply::State { guests, .. } = manage(&running, ManageRequest::State).await else {
        panic!("no state")
    };
    assert!(guests.is_empty());
}

#[tokio::test]
async fn stop_share_tells_the_engine() {
    let running = start_relay().await;
    let (mut engine, id) = register(&running, share(Mode::View, unix_now() + 60)).await;
    let reply = manage(
        &running,
        ManageRequest::StopShare {
            share_id: id.clone(),
        },
    )
    .await;
    assert_eq!(reply, ManageReply::Done);
    assert_eq!(
        next_json::<RelayMessage>(&mut engine).await,
        RelayMessage::Stop {
            reason: "the host stopped sharing".into()
        }
    );
    assert_eq!(
        manage(&running, ManageRequest::StopShare { share_id: id }).await,
        ManageReply::Failed {
            reason: "that share has ended".into()
        }
    );
}
