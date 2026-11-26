use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;

use share_protocol::{
    GuestCredential, GuestTab, ManageReply, ManageRequest, RelayMessage, Share, SharedTab,
    VIEWER_PATH_PREFIX,
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::watch;

use crate::RelayConfig;
use crate::auth::{generate_password, hash_password, random_token};
use crate::limits::FailureLimiter;
use crate::live::LiveShare;
use crate::store::{Store, unix_now};

pub const AUDIT_FAILED: &str = "the relay could not record guest input";

const ENGINE_GRACE: Duration = Duration::from_secs(60);
const HOST_STOPPED: &str = "the host stopped sharing";
const NO_GUESTS_LEFT: &str = "no guests are left";

pub struct Accounts {
    pub credentials: Vec<GuestCredential>,
    pub existing: Vec<String>,
}

pub struct Relay {
    pub config: RelayConfig,
    pub failures: FailureLimiter,
    pub store: Store,
    shares: Mutex<HashMap<String, Arc<LiveShare>>>,
    next_viewer: AtomicU64,
    changes: watch::Sender<()>,
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
            changes: watch::Sender::new(()),
            engine_grace: ENGINE_GRACE,
        })
    }

    pub async fn register(
        &self,
        share: Share,
        engine: UnboundedSender<RelayMessage>,
    ) -> anyhow::Result<(Arc<LiveShare>, Accounts)> {
        let id = random_token::<16>()?;
        let mut share = share;
        share.guests = guest_emails(&share.guests);
        let accounts = self.ensure_accounts(&share.guests).await?;
        self.store.share_registered(&id, &share).await?;
        let live = Arc::new(LiveShare::new(id.clone(), share, engine));
        self.shares().insert(id, live.clone());
        self.changed();
        Ok((live, accounts))
    }

    async fn ensure_accounts(&self, guests: &[String]) -> anyhow::Result<Accounts> {
        let mut accounts = Accounts {
            credentials: Vec::new(),
            existing: Vec::new(),
        };
        for email in guests {
            match self.store.guest_hash(email).await? {
                Some(_) => accounts.existing.push(email.clone()),
                None => accounts.credentials.push(self.new_password(email).await?),
            }
        }
        Ok(accounts)
    }

    async fn new_password(&self, email: &str) -> anyhow::Result<GuestCredential> {
        let password = generate_password()?;
        let hashed = password.clone();
        let hash = tokio::task::spawn_blocking(move || hash_password(&hashed)).await??;
        self.store.set_guest(email, &hash).await?;
        Ok(GuestCredential {
            email: email.to_string(),
            password,
        })
    }

    pub async fn manage(&self, request: ManageRequest) -> ManageReply {
        match self.try_manage(request).await {
            Ok(reply) => reply,
            Err(error) => {
                tracing::error!(%error, "could not manage shares");
                ManageReply::Failed {
                    reason: format!("{error:#}"),
                }
            }
        }
    }

    async fn try_manage(&self, request: ManageRequest) -> anyhow::Result<ManageReply> {
        match request {
            ManageRequest::State => Ok(ManageReply::State {
                guests: self.store.guests().await?,
                shares: self.shared_tabs(),
            }),
            ManageRequest::AddGuests { share_id, guests } => {
                let live = self.find(&share_id).context("that share has ended")?;
                let guests = guest_emails(&guests);
                let accounts = self.ensure_accounts(&guests).await?;
                let all = live.add_guests(&guests);
                self.store.share_guests(&live.id, &all).await?;
                self.changed();
                Ok(ManageReply::Credentials {
                    credentials: accounts.credentials,
                    existing: accounts.existing,
                })
            }
            ManageRequest::RemoveGuest { email, share_id } => {
                self.remove_guest(&normalize(&email), share_id.as_deref())
                    .await?;
                Ok(ManageReply::Done)
            }
            ManageRequest::ForgetGuest { email } => {
                let email = normalize(&email);
                self.remove_guest(&email, None).await?;
                self.store.forget_guest(&email).await?;
                Ok(ManageReply::Done)
            }
            ManageRequest::ResetPassword { email } => Ok(ManageReply::Credentials {
                credentials: vec![self.new_password(&normalize(&email)).await?],
                existing: Vec::new(),
            }),
            ManageRequest::StopShare { share_id } => {
                let live = self.find(&share_id).context("that share has ended")?;
                self.stop(&live, HOST_STOPPED).await;
                Ok(ManageReply::Done)
            }
        }
    }

    async fn remove_guest(&self, email: &str, share_id: Option<&str>) -> anyhow::Result<()> {
        let targets: Vec<Arc<LiveShare>> = self
            .live_shares()
            .into_iter()
            .filter(|live| share_id.is_none_or(|id| id == live.id))
            .filter(|live| live.admits(email))
            .collect();
        for live in targets {
            let left = live.remove_guest(email);
            self.store.share_guests(&live.id, &left).await?;
            if left.is_empty() {
                self.stop(&live, NO_GUESTS_LEFT).await;
            }
        }
        self.changed();
        Ok(())
    }

    async fn stop(&self, live: &LiveShare, reason: &str) {
        live.send_engine(RelayMessage::Stop {
            reason: reason.to_string(),
        });
        self.end(live, reason).await;
    }

    fn shared_tabs(&self) -> Vec<SharedTab> {
        let mut tabs: Vec<SharedTab> = self
            .live_shares()
            .into_iter()
            .map(|live| SharedTab {
                share_id: live.id.clone(),
                url: self.viewer_url(&live.id),
                share: live.share(),
                watching: live.watching(),
            })
            .collect();
        tabs.sort_by(|a, b| a.share.session_label.cmp(&b.share.session_label));
        tabs
    }

    pub fn guest_tabs(&self, email: &str) -> Vec<GuestTab> {
        let mut tabs: Vec<GuestTab> = self
            .live_shares()
            .into_iter()
            .filter(|live| live.admits(email))
            .map(|live| live.guest_tab())
            .collect();
        tabs.sort_by(|a, b| (&a.session_label, &a.label).cmp(&(&b.session_label, &b.label)));
        tabs
    }

    fn live_shares(&self) -> Vec<Arc<LiveShare>> {
        let now = unix_now();
        self.shares()
            .values()
            .filter(|live| !live.expired(now))
            .cloned()
            .collect()
    }

    pub fn changed(&self) {
        self.changes.send_replace(());
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub fn find(&self, id: &str) -> Option<Arc<LiveShare>> {
        self.shares()
            .get(id)
            .filter(|live| !live.expired(unix_now()))
            .cloned()
    }

    pub async fn end(&self, live: &LiveShare, reason: &str) {
        if live.is_ended() {
            return;
        }
        self.shares().remove(&live.id);
        if let Err(error) = self.store.share_ended(&live.id, reason).await {
            tracing::error!(%error, share_id = %live.id, "could not record the share end");
        }
        live.end(reason);
        self.changed();
    }

    pub async fn stop_unaudited(&self, live: &LiveShare) {
        tracing::error!(share_id = %live.id, "input could not be recorded; ending the share");
        self.stop(live, AUDIT_FAILED).await;
    }

    pub fn orphan(self: &Arc<Self>, live: Arc<LiveShare>) {
        if live.is_ended() {
            return;
        }
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

pub fn normalize(email: &str) -> String {
    email.trim().to_lowercase()
}

fn guest_emails(guests: &[String]) -> Vec<String> {
    let mut emails: Vec<String> = guests.iter().map(|guest| normalize(guest)).collect();
    emails.sort();
    emails.dedup();
    emails.retain(|email| !email.is_empty());
    emails
}
