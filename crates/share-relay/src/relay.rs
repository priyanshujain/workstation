use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use share_protocol::{GuestCredential, RelayMessage, Share, VIEWER_PATH_PREFIX};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::RelayConfig;
use crate::auth::{generate_password, hash_password, random_token};
use crate::limits::FailureLimiter;
use crate::live::LiveShare;
use crate::store::{Store, unix_now};

const ENGINE_GRACE: Duration = Duration::from_secs(60);

pub struct Relay {
    pub config: RelayConfig,
    pub failures: FailureLimiter,
    pub store: Store,
    shares: Mutex<HashMap<String, Arc<LiveShare>>>,
    next_viewer: AtomicU64,
    pub engine_grace: Duration,
}

impl Relay {
    pub async fn new(config: RelayConfig) -> anyhow::Result<Self> {
        let token = config
            .db_token_file
            .as_ref()
            .map(std::fs::read_to_string)
            .transpose()?;
        let store =
            Store::open(&config.db_url, token.map(|token| token.trim().to_string())).await?;
        Ok(Self {
            config,
            failures: FailureLimiter::default(),
            store,
            shares: Mutex::default(),
            next_viewer: AtomicU64::new(1),
            engine_grace: ENGINE_GRACE,
        })
    }

    pub async fn register(
        &self,
        share: Share,
        engine: UnboundedSender<RelayMessage>,
    ) -> anyhow::Result<(Arc<LiveShare>, Vec<GuestCredential>)> {
        let id = random_token::<16>()?;
        let mut share = share;
        share.guests = guest_emails(&share.guests);
        let credentials = self.issue_credentials(&id, &share.guests).await?;
        self.store.share_registered(&id, &share).await;
        let live = Arc::new(LiveShare::new(id.clone(), share, engine));
        self.shares().insert(id, live.clone());
        Ok((live, credentials))
    }

    async fn issue_credentials(
        &self,
        id: &str,
        guests: &[String],
    ) -> anyhow::Result<Vec<GuestCredential>> {
        let mut credentials = Vec::new();
        for email in guests {
            let password = generate_password()?;
            let hashed = password.clone();
            let hash = tokio::task::spawn_blocking(move || hash_password(&hashed)).await??;
            self.store.add_credential(id, email, &hash).await;
            credentials.push(GuestCredential {
                email: email.clone(),
                password,
            });
        }
        Ok(credentials)
    }

    pub fn find(&self, id: &str) -> Option<Arc<LiveShare>> {
        self.shares()
            .get(id)
            .filter(|live| !live.expired(unix_now()))
            .cloned()
    }

    pub async fn end(&self, live: &LiveShare, reason: &str) {
        self.shares().remove(&live.id);
        self.store.share_ended(&live.id, reason).await;
        live.end(reason);
    }

    pub fn orphan(self: &Arc<Self>, live: Arc<LiveShare>) {
        let generation = live.orphan();
        let left = Duration::from_secs(live.share().expires_at_unix.saturating_sub(unix_now()));
        let wait = self.engine_grace.min(left);
        let relay = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            if live.still_orphaned(generation) {
                relay.end(&live, "engine lost").await;
            }
        });
    }

    pub async fn restore(self: &Arc<Self>) -> anyhow::Result<()> {
        for (id, share) in self.store.open_shares().await? {
            let (engine, _) = mpsc::unbounded_channel();
            let live = Arc::new(LiveShare::new(id.clone(), share, engine));
            self.shares().insert(id, live.clone());
            self.orphan(live);
        }
        Ok(())
    }

    pub fn resume(
        &self,
        id: &str,
        engine: UnboundedSender<RelayMessage>,
    ) -> Option<Arc<LiveShare>> {
        let live = self.find(id)?;
        live.adopt(engine).then_some(live)
    }

    pub fn viewer_url(&self, id: &str) -> String {
        format!(
            "{}{VIEWER_PATH_PREFIX}{id}",
            self.config.public_url.trim_end_matches('/')
        )
    }

    pub fn next_viewer_id(&self) -> u64 {
        self.next_viewer.fetch_add(1, Ordering::Relaxed)
    }

    fn shares(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<LiveShare>>> {
        self.shares
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn guest_emails(guests: &[String]) -> Vec<String> {
    let mut emails: Vec<String> = guests
        .iter()
        .map(|guest| guest.trim().to_lowercase())
        .collect();
    emails.sort();
    emails.dedup();
    emails.retain(|email| !email.is_empty());
    emails
}
