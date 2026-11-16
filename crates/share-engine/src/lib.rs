mod socket;

use share_protocol::Mode;

pub struct Tab {
    pub session_id: String,
    pub session_label: String,
    pub window_label: String,
    pub block_id: String,
    pub title: String,
}

pub struct ShareRequest {
    pub session_id: String,
    pub block_id: String,
    pub mode: Mode,
    pub guests: Vec<String>,
    pub ttl_secs: u64,
    pub relay_url: String,
    pub secret: String,
}

pub fn list_tabs() -> anyhow::Result<Vec<Tab>> {
    anyhow::bail!("not built yet")
}

pub fn run(_request: ShareRequest, _on_url: impl FnOnce(&str)) -> anyhow::Result<()> {
    anyhow::bail!("not built yet")
}
