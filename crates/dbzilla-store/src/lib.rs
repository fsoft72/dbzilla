//! DBZilla store: settings, connections, history, saved queries and secrets.

pub mod atomic;
pub mod connections;
pub mod diagrams;
pub mod history;
pub mod paths;
pub mod queries;
pub mod secrets;
pub mod settings;
pub mod tags;

use thiserror::Error;

/// Errors raised by the store.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("{0}")]
    Invalid(String),
    #[error("secret store error: {0}")]
    Secret(String),
}

/// Result alias of the store.
pub type StoreResult<T> = Result<T, StoreError>;
