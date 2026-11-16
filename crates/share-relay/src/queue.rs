use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use share_protocol::ViewerMessage;
use tokio::sync::Notify;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub const VIEWER_BACKLOG_BYTES: usize = 4 * 1024 * 1024;
const MESSAGE_BYTES: usize = 256;

pub enum Outbound {
    Message(ViewerMessage),
    Bytes(Vec<u8>),
    Close,
}

pub enum Received {
    Outbound(Outbound),
    Lagged,
    Closed,
}

struct Backlog {
    limit: usize,
    pending: AtomicUsize,
    lagged: AtomicBool,
    notify: Notify,
}

pub struct ViewerSender {
    tx: UnboundedSender<Outbound>,
    backlog: Arc<Backlog>,
}

pub struct ViewerReceiver {
    rx: UnboundedReceiver<Outbound>,
    backlog: Arc<Backlog>,
}

pub fn viewer_queue(limit: usize) -> (ViewerSender, ViewerReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let backlog = Arc::new(Backlog {
        limit,
        pending: AtomicUsize::new(0),
        lagged: AtomicBool::new(false),
        notify: Notify::new(),
    });
    (
        ViewerSender {
            tx,
            backlog: backlog.clone(),
        },
        ViewerReceiver { rx, backlog },
    )
}

fn size(outbound: &Outbound) -> usize {
    match outbound {
        Outbound::Bytes(bytes) => bytes.len(),
        Outbound::Message(ViewerMessage::Screen { vt }) => vt.len(),
        Outbound::Message(_) => MESSAGE_BYTES,
        Outbound::Close => 0,
    }
}

impl ViewerSender {
    pub fn send(&self, outbound: Outbound) {
        let backlog = &self.backlog;
        if backlog.lagged.load(Ordering::Acquire) {
            return;
        }
        let bytes = size(&outbound);
        if backlog.pending.fetch_add(bytes, Ordering::AcqRel) + bytes > backlog.limit {
            backlog.lagged.store(true, Ordering::Release);
            backlog.notify.notify_one();
            return;
        }
        let _ = self.tx.send(outbound);
    }
}

impl ViewerReceiver {
    pub async fn recv(&mut self) -> Received {
        if self.backlog.lagged.load(Ordering::Acquire) {
            return Received::Lagged;
        }
        tokio::select! {
            () = self.backlog.notify.notified() => Received::Lagged,
            outbound = self.rx.recv() => match outbound {
                Some(outbound) => {
                    self.backlog.pending.fetch_sub(size(&outbound), Ordering::AcqRel);
                    Received::Outbound(outbound)
                }
                None => Received::Closed,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delivers_within_limit() {
        let (sender, mut receiver) = viewer_queue(10);
        sender.send(Outbound::Bytes(vec![0; 6]));
        sender.send(Outbound::Bytes(vec![0; 4]));
        assert!(
            matches!(receiver.recv().await, Received::Outbound(Outbound::Bytes(bytes)) if bytes.len() == 6)
        );
        sender.send(Outbound::Bytes(vec![0; 6]));
        assert!(matches!(receiver.recv().await, Received::Outbound(_)));
        assert!(matches!(receiver.recv().await, Received::Outbound(_)));
    }

    #[tokio::test]
    async fn reports_lag_past_limit() {
        let (sender, mut receiver) = viewer_queue(10);
        sender.send(Outbound::Bytes(vec![0; 8]));
        sender.send(Outbound::Bytes(vec![0; 8]));
        assert!(matches!(receiver.recv().await, Received::Lagged));
        sender.send(Outbound::Bytes(vec![0; 1]));
        assert!(matches!(receiver.recv().await, Received::Lagged));
    }
}
