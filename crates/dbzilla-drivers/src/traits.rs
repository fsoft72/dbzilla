//! Driver traits and shared request/response types (SPEC 5).

use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{Dialect, DriverId};
use dbzilla_core::model::{ColumnMeta, ObjectInfo, Row, TableRef, TableStructure};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

/// Default number of rows per streamed batch.
pub const DEFAULT_BATCH_SIZE: usize = 200;

/// Errors raised by drivers.
#[derive(Debug, Error)]
pub enum DriverError {
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("{0}")]
    Query(String),
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

/// Result alias for driver calls.
pub type DriverResult<T> = Result<T, DriverError>;

pub use dbzilla_core::model::SslMode;

/// Everything needed to open a connection. `Debug` hides the password (NFR-05).
#[derive(Clone)]
pub struct ConnectConfig {
    pub driver: DriverId,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: Option<String>,
    /// Database name, or file path for SQLite.
    pub database: String,
    pub ssl: SslMode,
    /// SQL run right after connecting.
    pub bootstrap: Vec<String>,
}

impl std::fmt::Debug for ConnectConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectConfig")
            .field("driver", &self.driver)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("database", &self.database)
            .field("ssl", &self.ssl)
            .field("bootstrap", &self.bootstrap)
            .finish()
    }
}

/// One database (or SQLite file) on the server.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseInfo {
    pub name: String,
}

/// Request to create a database.
#[derive(Debug, Clone)]
pub struct NewDatabase {
    pub name: String,
    pub encoding: Option<String>,
    pub collation: Option<String>,
}

/// A chunk of streamed rows. The first batch of every row-returning query carries the columns.
#[derive(Debug, Clone, Default)]
pub struct RowBatch {
    pub columns: Option<Vec<ColumnMeta>>,
    pub rows: Vec<Row>,
}

/// Bounded channel used for back-pressure while streaming.
pub type RowSink = mpsc::Sender<RowBatch>;

/// Options of one query.
#[derive(Debug, Clone)]
pub struct QueryOpts {
    /// Hard cap on streamed rows.
    pub limit: Option<u64>,
    pub timeout: Duration,
    pub batch_size: usize,
}

impl Default for QueryOpts {
    /// No limit, 300 s timeout, 200 rows per batch.
    fn default() -> Self {
        Self { limit: None, timeout: Duration::from_secs(300), batch_size: DEFAULT_BATCH_SIZE }
    }
}

/// Outcome of a query.
#[derive(Debug, Clone, Default)]
pub struct QuerySummary {
    pub affected: Option<u64>,
    pub elapsed: Duration,
    pub messages: Vec<String>,
}

/// Cancels the query currently running on a connection.
#[async_trait]
pub trait CancelToken: Send + Sync {
    /// Requests cancellation; the running query fails with [`DriverError::Cancelled`].
    async fn cancel(&self) -> DriverResult<()>;
}

/// A database driver.
#[async_trait]
pub trait Driver: Send + Sync {
    /// Driver id.
    fn id(&self) -> DriverId;
    /// Capabilities that drive the UI.
    fn capabilities(&self) -> &Capabilities;
    /// SQL dialect.
    fn dialect(&self) -> Box<dyn Dialect>;
    /// Opens a connection and runs the bootstrap commands.
    async fn connect(&self, cfg: &ConnectConfig) -> DriverResult<Box<dyn Conn>>;
}

/// An open connection. Calls are serialised by the owning session actor.
#[async_trait]
pub trait Conn: Send {
    async fn server_version(&mut self) -> DriverResult<String>;
    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>>;
    async fn list_schemas(&mut self, db: &str) -> DriverResult<Vec<String>>;
    async fn list_objects(&mut self, db: &str, schema: Option<&str>) -> DriverResult<Vec<ObjectInfo>>;
    async fn table_structure(&mut self, t: &TableRef) -> DriverResult<TableStructure>;
    /// Runs one statement, streaming rows through `tx` (awaiting for back-pressure).
    async fn query_stream(&mut self, sql: &str, opts: QueryOpts, tx: RowSink) -> DriverResult<QuerySummary>;
    /// Runs statements in order; with `atomic` inside one transaction. Returns affected rows per statement.
    async fn execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>>;
    /// Cheap row count estimate, when the server has one.
    async fn estimate_count(&mut self, t: &TableRef) -> DriverResult<Option<u64>>;
    /// Token that cancels the running query from another task.
    async fn cancel_handle(&mut self) -> DriverResult<Box<dyn CancelToken>>;
    async fn ping(&mut self) -> DriverResult<()>;
    async fn set_database(&mut self, db: &str) -> DriverResult<()>;
    async fn create_database(&mut self, req: &NewDatabase) -> DriverResult<()>;
    async fn rename_database(&mut self, from: &str, to: &str) -> DriverResult<()>;
    async fn drop_database(&mut self, name: &str) -> DriverResult<()>;
}
