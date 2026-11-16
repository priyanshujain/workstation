use serde::{Deserialize, Serialize};

pub const AGENT_PATH: &str = "/agent";
pub const VIEWER_PATH_PREFIX: &str = "/s/";
pub const VIEWER_SOCKET_SUFFIX: &str = "/ws";
pub const AGENT_SECRET_HEADER: &str = "x-wsctl-share-secret";
pub const ACCESS_JWT_HEADER: &str = "cf-access-jwt-assertion";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    View,
    Input,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Share {
    pub session_id: String,
    pub session_label: String,
    pub block_id: String,
    pub label: String,
    pub host: String,
    pub mode: Mode,
    pub guests: Vec<String>,
    pub expires_at_unix: u64,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub palette: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub state: String,
    pub kind: Option<String>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    pub progress: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    Register { share: Share },
    Resume { share_id: String },
    Screen { viewer_id: u64, vt: String },
    Resize { cols: u16, rows: u16 },
    Theme { theme: Theme },
    Status { status: Option<ProgramStatus> },
    Ended { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMessage {
    Registered {
        share_id: String,
        url: String,
    },
    ViewerJoined {
        viewer_id: u64,
        email: String,
    },
    ViewerLeft {
        viewer_id: u64,
        email: String,
    },
    Input {
        viewer_id: u64,
        email: String,
        data: String,
    },
    Stop {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerMessage {
    Hello { share: Share, email: String },
    Screen { vt: String },
    Resize { cols: u16, rows: u16 },
    Theme { theme: Theme },
    Status { status: Option<ProgramStatus> },
    Viewers { emails: Vec<String> },
    Ended { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerInput {
    Input { data: String },
}
