use std::time::{SystemTime, UNIX_EPOCH};

use libsql::{Builder, Connection, params};
use share_protocol::Share;
use tokio::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shares (
    share_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    session_label TEXT NOT NULL,
    block_id TEXT NOT NULL,
    label TEXT NOT NULL,
    host TEXT NOT NULL,
    mode TEXT NOT NULL,
    guests TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    ended_at INTEGER,
    end_reason TEXT
);
CREATE TABLE IF NOT EXISTS viewers (
    id INTEGER PRIMARY KEY,
    share_id TEXT NOT NULL,
    viewer_id INTEGER NOT NULL,
    email TEXT NOT NULL,
    joined_at INTEGER NOT NULL,
    left_at INTEGER
);
CREATE TABLE IF NOT EXISTS prompts (
    id INTEGER PRIMARY KEY,
    share_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    session_label TEXT NOT NULL,
    block_id TEXT NOT NULL,
    email TEXT NOT NULL,
    prompt TEXT NOT NULL,
    submitted_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS inputs (
    id INTEGER PRIMARY KEY,
    share_id TEXT NOT NULL,
    email TEXT NOT NULL,
    data TEXT NOT NULL,
    ts INTEGER NOT NULL
);
";

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    pub async fn open(url: &str, token: Option<String>) -> anyhow::Result<Self> {
        let database = match url.strip_prefix("file:") {
            Some(path) => Builder::new_local(path).build().await?,
            None => {
                Builder::new_remote(url.to_string(), token.unwrap_or_default())
                    .build()
                    .await?
            }
        };
        let connection = database.connect()?;
        connection.execute_batch(SCHEMA).await?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub async fn share_registered(&self, share_id: &str, share: &Share) {
        let mode = serde_json::to_value(share.mode)
            .ok()
            .and_then(|mode| mode.as_str().map(str::to_string))
            .unwrap_or_default();
        let guests = serde_json::to_string(&share.guests).unwrap_or_default();
        let expires_at =
            i64::try_from(share.expires_at_unix.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.execute(
            "INSERT INTO shares (share_id, session_id, session_label, block_id, label, host, mode, guests, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                share_id, share.session_id.as_str(), share.session_label.as_str(), share.block_id.as_str(),
                share.label.as_str(), share.host.as_str(), mode, guests, unix_millis(), expires_at
            ],
        )
        .await;
    }

    pub async fn share_ended(&self, share_id: &str, reason: &str) {
        self.execute(
            "UPDATE shares SET ended_at = ?2, end_reason = ?3 WHERE share_id = ?1",
            params![share_id, unix_millis(), reason],
        )
        .await;
    }

    pub async fn viewer_joined(&self, share_id: &str, viewer_id: u64, email: &str) {
        self.execute(
            "INSERT INTO viewers (share_id, viewer_id, email, joined_at) VALUES (?1, ?2, ?3, ?4)",
            params![share_id, viewer_id as i64, email, unix_millis()],
        )
        .await;
    }

    pub async fn viewer_left(&self, share_id: &str, viewer_id: u64) {
        self.execute(
            "UPDATE viewers SET left_at = ?3 WHERE share_id = ?1 AND viewer_id = ?2",
            params![share_id, viewer_id as i64, unix_millis()],
        )
        .await;
    }

    pub async fn input(&self, share_id: &str, email: &str, data: &str) {
        self.execute(
            "INSERT INTO inputs (share_id, email, data, ts) VALUES (?1, ?2, ?3, ?4)",
            params![share_id, email, data, unix_millis()],
        )
        .await;
    }

    pub async fn prompt(&self, share_id: &str, share: &Share, email: &str, prompt: &str) {
        self.execute(
            "INSERT INTO prompts (share_id, session_id, session_label, block_id, email, prompt, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                share_id, share.session_id.as_str(), share.session_label.as_str(), share.block_id.as_str(),
                email, prompt, unix_millis()
            ],
        )
        .await;
    }

    async fn execute(&self, sql: &str, values: impl libsql::params::IntoParams) {
        if let Err(error) = self.connection.lock().await.execute(sql, values).await {
            tracing::error!(%error, "share database write failed");
        }
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
