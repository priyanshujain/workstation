use serde::{Deserialize, Serialize};

pub const AGENT_PATH: &str = "/agent";
pub const VIEWER_PATH_PREFIX: &str = "/s/";
pub const VIEWER_SOCKET_SUFFIX: &str = "/ws";
pub const AGENT_SECRET_HEADER: &str = "x-wsctl-share-secret";

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
    Manage { request: ManageRequest },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ManageRequest {
    State,
    AddGuests {
        share_id: String,
        guests: Vec<String>,
    },
    RemoveGuest {
        email: String,
        share_id: Option<String>,
    },
    ForgetGuest {
        email: String,
    },
    ResetPassword {
        email: String,
    },
    StopShare {
        share_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedTab {
    pub share_id: String,
    pub url: String,
    pub share: Share,
    pub watching: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ManageReply {
    State {
        guests: Vec<String>,
        shares: Vec<SharedTab>,
    },
    Credentials {
        credentials: Vec<GuestCredential>,
        existing: Vec<String>,
    },
    Done,
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestCredential {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMessage {
    Registered {
        share_id: String,
        url: String,
        credentials: Vec<GuestCredential>,
        #[serde(default)]
        existing: Vec<String>,
    },
    Managed {
        reply: ManageReply,
    },
    Resumed {
        share_id: String,
    },
    ResumeRejected {
        share_id: String,
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
    Paste {
        viewer_id: u64,
        email: String,
        text: String,
    },
    Upload {
        viewer_id: u64,
        email: String,
        file: UploadFile,
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
    Reconnecting,
    Resize { cols: u16, rows: u16 },
    Theme { theme: Theme },
    Status { status: Option<ProgramStatus> },
    Viewers { emails: Vec<String> },
    Ended { reason: String },
    Tabs { tabs: Vec<GuestTab> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestTab {
    pub id: String,
    pub label: String,
    pub session_label: String,
    pub host: String,
    pub mode: Mode,
    pub expires_at_unix: u64,
    pub status: Option<ProgramStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerInput {
    Input { data: String },
    Paste { text: String },
    Upload { file: UploadFile },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadFile {
    pub batch: String,
    pub index: u32,
    pub count: u32,
    pub name: String,
    pub mime: String,
    pub data: String,
}
