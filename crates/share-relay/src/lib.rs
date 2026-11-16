mod access;
mod agent;
mod assets;
mod live;
mod prompt;
mod queue;
mod relay;
mod store;
#[cfg(test)]
mod tests;
mod viewer;

use std::sync::Arc;

use tokio::net::TcpListener;

use crate::relay::Relay;

pub struct RelayConfig {
    pub viewer_listen: String,
    pub agent_listen: String,
    pub public_url: String,
    pub secret: String,
    pub access_team_domain: String,
    pub access_audience: String,
    pub access_certs_url: Option<String>,
    pub db_url: String,
    pub db_token_file: Option<std::path::PathBuf>,
    #[cfg(debug_assertions)]
    pub dev_email: Option<String>,
}

pub fn serve(config: RelayConfig) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        let viewers = TcpListener::bind(&config.viewer_listen).await?;
        let agents = TcpListener::bind(&config.agent_listen).await?;
        run(config, viewers, agents).await
    })
}

async fn run(config: RelayConfig, viewers: TcpListener, agents: TcpListener) -> anyhow::Result<()> {
    serve_relay(Relay::new(config).await?, viewers, agents).await
}

async fn serve_relay(
    relay: Relay,
    viewers: TcpListener,
    agents: TcpListener,
) -> anyhow::Result<()> {
    let relay = Arc::new(relay);
    relay.restore().await?;
    let viewer_server = axum::serve(viewers, viewer::router(relay.clone()));
    let agent_server = axum::serve(agents, agent::router(relay));
    tokio::try_join!(async { viewer_server.await }, async { agent_server.await })?;
    Ok(())
}
