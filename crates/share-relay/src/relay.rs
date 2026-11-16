use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use share_protocol::{RelayMessage, Share, VIEWER_PATH_PREFIX};
use tokio::sync::mpsc::UnboundedSender;

use crate::RelayConfig;
use crate::access::{AccessVerifier, cloudflare_certs_url};
use crate::live::LiveShare;
use crate::store::{Store, unix_now};

pub struct Relay {
    pub config: RelayConfig,
    pub verifier: AccessVerifier,
    pub store: Store,
    shares: Mutex<HashMap<String, Arc<LiveShare>>>,
    next_viewer: AtomicU64,
}

impl Relay {
    pub async fn new(config: RelayConfig) -> anyhow::Result<Self> {
        let certs_url = config
            .access_certs_url
            .clone()
            .unwrap_or_else(|| cloudflare_certs_url(&config.access_team_domain));
        let verifier = AccessVerifier::new(
            &config.access_team_domain,
            &config.access_audience,
            certs_url,
        );
        let token = config
            .db_token_file
            .as_ref()
            .map(std::fs::read_to_string)
            .transpose()?;
        let store =
            Store::open(&config.db_url, token.map(|token| token.trim().to_string())).await?;
        Ok(Self {
            config,
            verifier,
            store,
            shares: Mutex::default(),
            next_viewer: AtomicU64::new(1),
        })
    }

    pub async fn register(
        &self,
        share: Share,
        engine: UnboundedSender<RelayMessage>,
    ) -> anyhow::Result<Arc<LiveShare>> {
        let id = share_id()?;
        self.store.share_registered(&id, &share).await;
        let live = Arc::new(LiveShare::new(id.clone(), share, engine));
        self.shares().insert(id.clone(), live.clone());
        Ok(live)
    }

    pub fn find(&self, id: &str) -> Option<Arc<LiveShare>> {
        self.shares()
            .get(id)
            .filter(|live| !live.expired(unix_now()))
            .cloned()
    }

    pub async fn end(&self, live: &LiveShare, reason: &str) {
        self.shares().remove(&live.id);
        live.end(reason);
        self.store.share_ended(&live.id, reason).await;
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

fn share_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("random share id: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
