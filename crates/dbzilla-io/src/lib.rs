//! Import and export of table data (SPEC 12): streaming writers and batch importers.

pub mod backup;
pub mod convert;
pub mod export;
pub mod import;
pub mod naming;

use dbzilla_drivers::DriverError;
use thiserror::Error;

/// Rows per generated INSERT statement and per transaction step.
pub const BATCH_ROWS: usize = 500;

/// Errors of import and export.
#[derive(Debug, Error)]
pub enum IoError {
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Driver(#[from] DriverError),
    #[error("{0}")]
    Format(String),
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("not supported: {0}")]
    Unsupported(String),
}

/// Progress report of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// What is being processed (table or file).
    pub item: String,
    pub rows: u64,
    pub done_items: usize,
    pub total_items: usize,
}

/// Progress callback.
pub type ProgressFn<'a> = &'a (dyn Fn(Progress) + Send + Sync);
