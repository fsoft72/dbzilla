//! An open connection: session handle and per-workspace state.

use dbzilla_session::{Command, Event, SessionHandle};
use dbzilla_store::connections::StoredConnection;

/// Connection state shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum WsStatus {
    Connecting,
    Connected(String),
    Failed(String),
    Disconnected(String),
}

/// One open database.
pub struct Workspace {
    pub conn: StoredConnection,
    pub session: SessionHandle,
    pub status: WsStatus,
}

impl Workspace {
    /// Drains the events that arrived since the last frame.
    pub fn poll(&mut self) {
        while let Some(ev) = self.session.try_recv() {
            self.apply(ev);
        }
    }

    /// Applies one event to the state.
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Connected { server_version } => self.status = WsStatus::Connected(server_version),
            Event::ConnectFailed { error } => self.status = WsStatus::Failed(error),
            Event::Disconnected { reason } => self.status = WsStatus::Disconnected(reason),
            _ => {}
        }
    }

    /// Asks the session to reconnect.
    pub fn reconnect(&mut self) {
        self.status = WsStatus::Connecting;
        self.session.send(Command::Reconnect);
    }
}
