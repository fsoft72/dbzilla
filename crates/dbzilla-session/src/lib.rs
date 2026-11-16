//! Session actor: one task per open connection, talking to the UI through channels.

pub mod actor;
pub mod commands;
pub mod console;
pub mod gate;

pub use actor::{default_factory, spawn, ConnFactory, Repaint, SessionConfig, SessionHandle};
pub use commands::{Command, DbOp, Event, PageRequest, RequestId};
