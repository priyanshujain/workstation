use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

const MIN_REFRESH: Duration = Duration::from_secs(30);

pub struct AccessVerifier {
    certs_url: String,
    validation: Validation,
    client: reqwest::Client,
    keys: RwLock<Keys>,
}

#[derive(Default)]
struct Keys {
    by_kid: HashMap<String, DecodingKey>,
    fetched: Option<Instant>,
}

#[derive(Deserialize)]
struct Certs {
    keys: Vec<Cert>,
}

#[derive(Deserialize)]
struct Cert {
    kid: String,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    email: String,
}

pub fn cloudflare_certs_url(team_domain: &str) -> String {
    format!("https://{team_domain}/cdn-cgi/access/certs")
}

impl AccessVerifier {
    pub fn new(team_domain: &str, audience: &str, certs_url: String) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            certs_url,
            validation: validation(team_domain, audience),
            client: reqwest::Client::new(),
            keys: RwLock::default(),
        }
    }

    pub async fn verify(&self, token: &str) -> anyhow::Result<String> {
        let header = decode_header(token)?;
        anyhow::ensure!(header.alg == Algorithm::RS256, "token is not RS256");
        let kid = header.kid.context("token has no kid")?;
        let key = self.key(&kid).await?;
        Ok(decode::<Claims>(token, &key, &self.validation)?
            .claims
            .email)
    }

    async fn key(&self, kid: &str) -> anyhow::Result<DecodingKey> {
        if let Some(key) = self.keys.read().await.by_kid.get(kid) {
            return Ok(key.clone());
        }
        self.refresh().await?;
        self.keys
            .read()
            .await
            .by_kid
            .get(kid)
            .cloned()
            .context("unknown signing key")
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        let mut keys = self.keys.write().await;
        if keys.fetched.is_some_and(|at| at.elapsed() < MIN_REFRESH) {
            return Ok(());
        }
        let certs: Certs = self
            .client
            .get(&self.certs_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        keys.by_kid = certs.keys.into_iter().filter_map(rsa_key).collect();
        keys.fetched = Some(Instant::now());
        Ok(())
    }
}

fn rsa_key(cert: Cert) -> Option<(String, DecodingKey)> {
    if cert.kty != "RSA" {
        return None;
    }
    let key = DecodingKey::from_rsa_components(&cert.n?, &cert.e?).ok()?;
    Some((cert.kid, key))
}

fn validation(team_domain: &str, audience: &str) -> Validation {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[format!("https://{team_domain}")]);
    validation.set_required_spec_claims(&["exp", "aud", "iss"]);
    validation
}
