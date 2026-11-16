pub struct RelayConfig {
    pub viewer_listen: String,
    pub agent_listen: String,
    pub public_url: String,
    pub secret: String,
    pub access_team_domain: String,
    pub access_audience: String,
    pub audit_log: std::path::PathBuf,
}

pub fn serve(_config: RelayConfig) -> anyhow::Result<()> {
    anyhow::bail!("not built yet")
}
