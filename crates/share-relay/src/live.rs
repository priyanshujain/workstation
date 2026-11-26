use std::collections::BTreeMap;
use std::sync::Mutex;

use share_protocol::{
    AgentMessage, GuestTab, Mode, ProgramStatus, RelayMessage, Share, Theme, ViewerMessage,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::queue::{Outbound, ViewerSender};

pub const ACCESS_REMOVED: &str = "the host removed your access";

pub struct LiveShare {
    pub id: String,
    state: Mutex<State>,
}

struct State {
    engine: UnboundedSender<RelayMessage>,
    orphaned: Option<u64>,
    generation: u64,
    share: Share,
    theme: Option<Theme>,
    status: Option<Option<ProgramStatus>>,
    viewers: BTreeMap<u64, Viewer>,
    ended: bool,
}

struct Viewer {
    email: String,
    ready: bool,
    tx: ViewerSender,
}

impl LiveShare {
    pub fn new(id: String, share: Share, engine: UnboundedSender<RelayMessage>) -> Self {
        let state = State {
            share,
            theme: None,
            status: None,
            viewers: BTreeMap::new(),
            ended: false,
            engine,
            orphaned: None,
            generation: 0,
        };
        Self {
            id,
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

    pub fn add_guests(&self, guests: &[String]) -> Vec<String> {
        let mut state = self.state();
        for guest in guests {
            if !state.share.guests.contains(guest) {
                state.share.guests.push(guest.clone());
            }
        }
        state.share.guests.sort();
        state.share.guests.clone()
    }

    pub fn remove_guest(&self, email: &str) -> Vec<String> {
        let mut state = self.state();
        state.share.guests.retain(|guest| guest != email);
        let removed: Vec<u64> = state
            .viewers
            .iter()
            .filter(|(_, viewer)| viewer.email == email)
            .map(|(id, _)| *id)
            .collect();
        for viewer_id in removed {
            if let Some(viewer) = state.viewers.remove(&viewer_id) {
                viewer.tx.send(Outbound::Message(ViewerMessage::Ended {
                    reason: ACCESS_REMOVED.to_string(),
                }));
                viewer.tx.send(Outbound::Close);
                state.send_engine(RelayMessage::ViewerLeft {
                    viewer_id,
                    email: viewer.email,
                });
            }
        }
        state.broadcast_viewers();
        state.share.guests.clone()
    }

    pub fn watching(&self) -> Vec<String> {
        let mut emails: Vec<String> = self
            .state()
            .viewers
            .values()
            .map(|viewer| viewer.email.clone())
            .collect();
        emails.sort();
        emails.dedup();
        emails
    }

    pub fn guest_tab(&self) -> GuestTab {
        let state = self.state();
        GuestTab {
            id: self.id.clone(),
            label: state.share.label.clone(),
            session_label: state.share.session_label.clone(),
            host: state.share.host.clone(),
            mode: state.share.mode,
            expires_at_unix: state.share.expires_at_unix,
            status: state.status.clone().flatten(),
        }
    }

    pub fn expired(&self, now_unix: u64) -> bool {
        now_unix >= self.state().share.expires_at_unix
    }

    pub fn join(&self, viewer_id: u64, email: &str, tx: ViewerSender) -> bool {
        let mut state = self.state();
        if state.ended {
            return false;
        }
        for message in state.greeting(email) {
            tx.send(Outbound::Message(message));
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
        if state.orphaned.is_some() {
            state.viewers[&viewer_id]
                .tx
                .send(Outbound::Message(ViewerMessage::Reconnecting));
        }
        state.send_engine(RelayMessage::ViewerJoined {
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
        state.send_engine(RelayMessage::ViewerLeft {
            viewer_id,
            email: viewer.email,
        });
    }

    pub fn accepts_input(&self) -> bool {
        let state = self.state();
        state.share.mode == Mode::Input && state.orphaned.is_none() && !state.ended
    }

    pub fn is_ended(&self) -> bool {
        self.state().ended
    }

    pub fn input(&self, viewer_id: u64, email: &str, data: String) -> bool {
        if !self.accepts_input() {
            return false;
        }
        let state = self.state();
        state.send_engine(RelayMessage::Input {
            viewer_id,
            email: email.to_string(),
            data,
        });
        true
    }

    pub fn bytes(&self, data: &[u8]) {
        for viewer in self.state().viewers.values().filter(|viewer| viewer.ready) {
            viewer.tx.send(Outbound::Bytes(data.to_vec()));
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
            | AgentMessage::Ended { .. }
            | AgentMessage::Manage { .. } => {}
        }
    }

    pub fn end(&self, reason: &str) {
        let mut state = self.state();
        state.ended = true;
        state.broadcast(ViewerMessage::Ended {
            reason: reason.to_string(),
        });
        for viewer in std::mem::take(&mut state.viewers).into_values() {
            viewer.tx.send(Outbound::Close);
        }
    }

    pub fn send_engine(&self, message: RelayMessage) {
        self.state().send_engine(message);
    }

    pub fn orphan(&self) -> u64 {
        let mut state = self.state();
        state.generation += 1;
        state.orphaned = Some(state.generation);
        state.broadcast(ViewerMessage::Reconnecting);
        state.generation
    }

    pub fn still_orphaned(&self, generation: u64) -> bool {
        self.state().orphaned == Some(generation)
    }

    pub fn adopt(&self, engine: UnboundedSender<RelayMessage>) -> bool {
        let mut state = self.state();
        if state.orphaned.is_none() || state.ended {
            return false;
        }
        state.orphaned = None;
        state.engine = engine;
        state.send_engine(RelayMessage::Resumed {
            share_id: self.id.clone(),
        });
        let joined: Vec<(u64, String)> = state
            .viewers
            .iter()
            .map(|(id, viewer)| (*id, viewer.email.clone()))
            .collect();
        for (viewer_id, email) in joined {
            let hello = ViewerMessage::Hello {
                share: state.share.clone(),
                email: email.clone(),
            };
            let viewer = state.viewers.get_mut(&viewer_id).expect("listed viewer");
            viewer.ready = false;
            viewer.tx.send(Outbound::Message(hello));
            state.send_engine(RelayMessage::ViewerJoined { viewer_id, email });
        }
        true
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl State {
    fn send_engine(&self, message: RelayMessage) {
        let _ = self.engine.send(message);
    }

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
            viewer
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
            viewer.tx.send(Outbound::Message(message.clone()));
        }
    }
}
