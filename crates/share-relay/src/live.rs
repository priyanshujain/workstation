use std::collections::BTreeMap;
use std::sync::Mutex;

use share_protocol::{
    AgentMessage, Mode, ProgramStatus, RelayMessage, Share, Theme, ViewerMessage,
};
use tokio::sync::mpsc::UnboundedSender;

pub enum Outbound {
    Message(ViewerMessage),
    Bytes(Vec<u8>),
    Close,
}

pub struct LiveShare {
    pub id: String,
    engine: UnboundedSender<RelayMessage>,
    state: Mutex<State>,
}

struct State {
    share: Share,
    theme: Option<Theme>,
    status: Option<Option<ProgramStatus>>,
    viewers: BTreeMap<u64, Viewer>,
    ended: bool,
}

struct Viewer {
    email: String,
    ready: bool,
    tx: UnboundedSender<Outbound>,
}

impl LiveShare {
    pub fn new(id: String, share: Share, engine: UnboundedSender<RelayMessage>) -> Self {
        let state = State {
            share,
            theme: None,
            status: None,
            viewers: BTreeMap::new(),
            ended: false,
        };
        Self {
            id,
            engine,
            state: Mutex::new(state),
        }
    }

    pub fn share(&self) -> Share {
        self.state().share.clone()
    }

    pub fn admits(&self, email: &str) -> bool {
        self.state()
            .share
            .guests
            .iter()
            .any(|guest| guest.eq_ignore_ascii_case(email))
    }

    pub fn expired(&self, now_unix: u64) -> bool {
        now_unix >= self.state().share.expires_at_unix
    }

    pub fn join(&self, viewer_id: u64, email: &str, tx: UnboundedSender<Outbound>) -> bool {
        let mut state = self.state();
        if state.ended {
            return false;
        }
        for message in state.greeting(email) {
            let _ = tx.send(Outbound::Message(message));
        }
        state.viewers.insert(
            viewer_id,
            Viewer {
                email: email.to_string(),
                ready: false,
                tx,
            },
        );
        state.broadcast_viewers();
        self.send_engine(RelayMessage::ViewerJoined {
            viewer_id,
            email: email.to_string(),
        });
        true
    }

    pub fn leave(&self, viewer_id: u64) {
        let mut state = self.state();
        let Some(viewer) = state.viewers.remove(&viewer_id) else {
            return;
        };
        state.broadcast_viewers();
        self.send_engine(RelayMessage::ViewerLeft {
            viewer_id,
            email: viewer.email,
        });
    }

    pub fn input(&self, viewer_id: u64, email: &str, data: String) -> bool {
        if self.state().share.mode != Mode::Input {
            return false;
        }
        self.send_engine(RelayMessage::Input {
            viewer_id,
            email: email.to_string(),
            data,
        });
        true
    }

    pub fn bytes(&self, data: &[u8]) {
        for viewer in self.state().viewers.values().filter(|viewer| viewer.ready) {
            let _ = viewer.tx.send(Outbound::Bytes(data.to_vec()));
        }
    }

    pub fn apply(&self, message: AgentMessage) {
        let mut state = self.state();
        match message {
            AgentMessage::Screen { viewer_id, vt } => state.screen(viewer_id, vt),
            AgentMessage::Resize { cols, rows } => {
                state.share.cols = cols;
                state.share.rows = rows;
                state.broadcast(ViewerMessage::Resize { cols, rows });
            }
            AgentMessage::Theme { theme } => {
                state.theme = Some(theme.clone());
                state.broadcast(ViewerMessage::Theme { theme });
            }
            AgentMessage::Status { status } => {
                state.status = Some(status.clone());
                state.broadcast(ViewerMessage::Status { status });
            }
            AgentMessage::Register { .. }
            | AgentMessage::Resume { .. }
            | AgentMessage::Ended { .. } => {}
        }
    }

    pub fn end(&self, reason: &str) {
        let mut state = self.state();
        state.ended = true;
        state.broadcast(ViewerMessage::Ended {
            reason: reason.to_string(),
        });
        for viewer in std::mem::take(&mut state.viewers).into_values() {
            let _ = viewer.tx.send(Outbound::Close);
        }
    }

    pub fn send_engine(&self, message: RelayMessage) {
        let _ = self.engine.send(message);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl State {
    fn greeting(&self, email: &str) -> Vec<ViewerMessage> {
        let mut messages = vec![
            ViewerMessage::Hello {
                share: self.share.clone(),
                email: email.to_string(),
            },
            ViewerMessage::Resize {
                cols: self.share.cols,
                rows: self.share.rows,
            },
        ];
        messages.extend(
            self.theme
                .clone()
                .map(|theme| ViewerMessage::Theme { theme }),
        );
        messages.extend(
            self.status
                .clone()
                .map(|status| ViewerMessage::Status { status }),
        );
        messages
    }

    fn screen(&mut self, viewer_id: u64, vt: String) {
        if let Some(viewer) = self.viewers.get_mut(&viewer_id) {
            viewer.ready = true;
            let _ = viewer
                .tx
                .send(Outbound::Message(ViewerMessage::Screen { vt }));
        }
    }

    fn broadcast_viewers(&self) {
        let emails = self
            .viewers
            .values()
            .map(|viewer| viewer.email.clone())
            .collect();
        self.broadcast(ViewerMessage::Viewers { emails });
    }

    fn broadcast(&self, message: ViewerMessage) {
        for viewer in self.viewers.values() {
            let _ = viewer.tx.send(Outbound::Message(message.clone()));
        }
    }
}
