use std::time::{SystemTime, UNIX_EPOCH};

use libsql::params::{IntoParams, Params};
use libsql::{Builder, Connection, Database, params};
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
    cols INTEGER NOT NULL,
    rows INTEGER NOT NULL,
    ended_at INTEGER,
    end_reason TEXT
);
CREATE TABLE IF NOT EXISTS guests (
    email TEXT PRIMARY KEY,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS guest_sessions (
    token_hash TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
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

const MOVE_SHARE_PASSWORDS_TO_GUESTS: &str = "
CREATE TABLE IF NOT EXISTS guest_credentials (
    share_id TEXT NOT NULL,
    email TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT OR IGNORE INTO guests (email, password_hash, created_at)
    SELECT email, password_hash, created_at FROM guest_credentials ORDER BY created_at DESC;
DROP TABLE guest_credentials;
DROP TABLE IF EXISTS viewer_sessions;
";

pub struct Store {
    database: Database,
    order: Mutex<()>,
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
        connection
            .execute_batch(MOVE_SHARE_PASSWORDS_TO_GUESTS)
            .await?;
        Ok(Self {
            database,
            order: Mutex::new(()),
        })
    }

    pub async fn share_registered(&self, share_id: &str, share: &Share) -> anyhow::Result<()> {
        let mode = serde_json::to_value(share.mode)
            .ok()
            .and_then(|mode| mode.as_str().map(str::to_string))
            .unwrap_or_default();
        let guests = serde_json::to_string(&share.guests).unwrap_or_default();
        let expires_at =
            i64::try_from(share.expires_at_unix.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.execute(
            "INSERT INTO shares (share_id, session_id, session_label, block_id, label, host, mode, guests, created_at, expires_at, cols, rows)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                share_id, share.session_id.as_str(), share.session_label.as_str(), share.block_id.as_str(),
                share.label.as_str(), share.host.as_str(), mode, guests, unix_millis(), expires_at, share.cols, share.rows
            ],
        )
        .await
    }

    pub async fn open_shares(&self) -> anyhow::Result<Vec<(String, Share)>> {
        let connection = self.connect().await?;
        let mut rows = connection
            .query(
                "SELECT share_id, session_id, session_label, block_id, label, host, mode, guests, expires_at, cols, rows
                 FROM shares WHERE ended_at IS NULL AND expires_at > ?1",
                params![unix_millis()],
            )
            .await?;
        let mut shares = Vec::new();
        while let Some(row) = rows.next().await? {
            shares.push((row.get(0)?, share_from(&row)?));
        }
        Ok(shares)
    }

    pub async fn guest_hash(&self, email: &str) -> anyhow::Result<Option<String>> {
        self.text(
            "SELECT password_hash FROM guests WHERE email = ?1",
            params![email],
        )
        .await
    }

    pub async fn set_guest(&self, email: &str, password_hash: &str) -> anyhow::Result<()> {
        self.execute(
            "INSERT INTO guests (email, password_hash, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (email) DO UPDATE SET password_hash = excluded.password_hash",
            params![email, password_hash, unix_millis()],
        )
        .await
    }

    pub async fn forget_guest(&self, email: &str) -> anyhow::Result<()> {
        self.execute("DELETE FROM guests WHERE email = ?1", params![email])
            .await?;
        self.execute(
            "DELETE FROM guest_sessions WHERE email = ?1",
            params![email],
        )
        .await
    }

    pub async fn guests(&self) -> anyhow::Result<Vec<String>> {
        let _order = self.order.lock().await;
        let mut rows = self
            .connect()
            .await?
            .query("SELECT email FROM guests ORDER BY email", ())
            .await?;
        let mut emails = Vec::new();
        while let Some(row) = rows.next().await? {
            emails.push(row.get(0)?);
        }
        Ok(emails)
    }

    pub async fn share_guests(&self, share_id: &str, guests: &[String]) -> anyhow::Result<()> {
        let guests = serde_json::to_string(guests)?;
        self.execute(
            "UPDATE shares SET guests = ?2 WHERE share_id = ?1",
            params![share_id, guests],
        )
        .await
    }

    pub async fn add_session(
        &self,
        token_hash: &str,
        email: &str,
        expires_at_unix: u64,
    ) -> anyhow::Result<()> {
        let expires_at = i64::try_from(expires_at_unix.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.execute(
            "INSERT INTO guest_sessions (token_hash, email, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, email, unix_millis(), expires_at],
        )
        .await
    }

    pub async fn session_email(&self, token_hash: &str) -> anyhow::Result<Option<String>> {
        self.text(
            "SELECT email FROM guest_sessions WHERE token_hash = ?1 AND expires_at > ?2",
            params![token_hash, unix_millis()],
        )
        .await
    }

    pub async fn delete_session(&self, token_hash: &str) -> anyhow::Result<()> {
        self.execute(
            "DELETE FROM guest_sessions WHERE token_hash = ?1",
            params![token_hash],
        )
        .await
    }

    pub async fn share_ended(&self, share_id: &str, reason: &str) -> anyhow::Result<()> {
        self.execute(
            "UPDATE shares SET ended_at = ?2, end_reason = ?3 WHERE share_id = ?1",
            params![share_id, unix_millis(), reason],
        )
        .await
    }

    pub async fn viewer_joined(
        &self,
        share_id: &str,
        viewer_id: u64,
        email: &str,
    ) -> anyhow::Result<()> {
        self.execute(
            "INSERT INTO viewers (share_id, viewer_id, email, joined_at) VALUES (?1, ?2, ?3, ?4)",
            params![share_id, viewer_id as i64, email, unix_millis()],
        )
        .await
    }

    pub async fn viewer_left(&self, share_id: &str, viewer_id: u64) -> anyhow::Result<()> {
        self.execute(
            "UPDATE viewers SET left_at = ?3 WHERE share_id = ?1 AND viewer_id = ?2",
            params![share_id, viewer_id as i64, unix_millis()],
        )
        .await
    }

    pub async fn input(&self, share_id: &str, email: &str, data: &str) -> anyhow::Result<()> {
        self.execute(
            "INSERT INTO inputs (share_id, email, data, ts) VALUES (?1, ?2, ?3, ?4)",
            params![share_id, email, data, unix_millis()],
        )
        .await
    }

    pub async fn prompt(
        &self,
        share_id: &str,
        share: &Share,
        email: &str,
        prompt: &str,
    ) -> anyhow::Result<()> {
        self.execute(
            "INSERT INTO prompts (share_id, session_id, session_label, block_id, email, prompt, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                share_id, share.session_id.as_str(), share.session_label.as_str(), share.block_id.as_str(),
                email, prompt, unix_millis()
            ],
        )
        .await
    }

    async fn connect(&self) -> anyhow::Result<Connection> {
        Ok(self.database.connect()?)
    }

    async fn text(&self, sql: &str, values: impl IntoParams) -> anyhow::Result<Option<String>> {
        let values = values.into_params()?;
        let _order = self.order.lock().await;
        match self.read_text(sql, &values).await {
            Ok(text) => Ok(text),
            Err(error) => {
                tracing::warn!(%error, "share database read failed, retrying");
                self.read_text(sql, &values).await
            }
        }
    }

    async fn read_text(&self, sql: &str, values: &Params) -> anyhow::Result<Option<String>> {
        let mut rows = self.connect().await?.query(sql, values).await?;
        match rows.next().await? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    async fn execute(&self, sql: &str, values: impl IntoParams) -> anyhow::Result<()> {
        let values = values.into_params()?;
        let _order = self.order.lock().await;
        if let Err(error) = self.connect().await?.execute(sql, &values).await {
            tracing::warn!(%error, "share database write failed, retrying");
            self.connect().await?.execute(sql, &values).await?;
        }
        Ok(())
    }
}

fn share_from(row: &libsql::Row) -> anyhow::Result<Share> {
    let mode: String = row.get(6)?;
    let guests: String = row.get(7)?;
    let expires_at: i64 = row.get(8)?;
    Ok(Share {
        session_id: row.get(1)?,
        session_label: row.get(2)?,
        block_id: row.get(3)?,
        label: row.get(4)?,
        host: row.get(5)?,
        mode: serde_json::from_value(serde_json::Value::String(mode))?,
        guests: serde_json::from_str(&guests)?,
        expires_at_unix: u64::try_from(expires_at / 1000)?,
        cols: u16::try_from(row.get::<i64>(9)?)?,
        rows: u16::try_from(row.get::<i64>(10)?)?,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a running sqld; set SQLD_URL"]
    async fn survives_idle_remote_streams() {
        let url = std::env::var("SQLD_URL").expect("SQLD_URL");
        let store = Store::open(&url, None).await.unwrap();
        let share_id = crate::auth::random_token::<16>().unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let email = format!("{share_id}@example.com");
        store.set_guest(&email, "hash").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        let hash = store.guest_hash(&email).await.unwrap();
        assert_eq!(hash.as_deref(), Some("hash"));
    }
}
