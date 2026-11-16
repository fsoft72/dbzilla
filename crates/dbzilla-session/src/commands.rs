//! Commands sent to the session actor and events it emits (SPEC 25).

use dbzilla_core::filter::Filter;
use dbzilla_core::model::{ColumnMeta, ObjectInfo, Row, TableRef, TableStructure};
use dbzilla_core::paging::SortKey;
use dbzilla_drivers::{ConnectConfig, DatabaseInfo, NewDatabase, QueryOpts, QuerySummary};
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifier echoed by every event of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

/// Source of request ids.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

impl RequestId {
    /// A fresh, process-unique id.
    pub fn next() -> Self {
        Self(NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
    }
}

/// A page of a table.
#[derive(Debug, Clone)]
pub struct PageRequest {
    pub req: RequestId,
    pub table: TableRef,
    pub columns: Vec<ColumnMeta>,
    pub filters: Vec<Filter>,
    pub filters_or: bool,
    pub sort: Vec<SortKey>,
    pub limit: u64,
    pub offset: u64,
}

/// Database level operation.
#[derive(Debug, Clone)]
pub enum DbOp {
    Create(NewDatabase),
    Rename { from: String, to: String },
    Drop(String),
}

/// Requests to the actor.
#[derive(Debug, Clone)]
pub enum Command {
    Connect(ConnectConfig),
    Disconnect,
    Reconnect,
    LoadDatabases,
    LoadSchema { db: String, schema: Option<String> },
    LoadStructure(TableRef),
    EstimateCount { req: RequestId, table: TableRef },
    UseDatabase(String),
    FetchPage(Box<PageRequest>),
    CountExact { req: RequestId, table: TableRef, columns: Vec<ColumnMeta>, filters: Vec<Filter>, filters_or: bool },
    RunSql { req: RequestId, statements: Vec<String>, opts: QueryOpts, stop_on_error: bool },
    Commit { req: RequestId, statements: Vec<String> },
    Cancel(RequestId),
    DbOp(DbOp),
}

/// Notifications from the actor.
#[derive(Debug, Clone)]
pub enum Event {
    Connected { server_version: String },
    ConnectFailed { error: String },
    Disconnected { reason: String },
    Databases(Vec<DatabaseInfo>),
    Schema { db: String, schema: Option<String>, schemas: Vec<String>, objects: Vec<ObjectInfo> },
    Structure(Box<TableStructure>),
    DatabaseChanged(String),
    StatementStarted { req: RequestId, index: usize, sql: String },
    RowBatch { req: RequestId, index: usize, columns: Option<Vec<ColumnMeta>>, rows: Vec<Row> },
    QueryDone { req: RequestId, index: usize, summary: QuerySummary },
    QueryError { req: RequestId, index: usize, error: String, cancelled: bool },
    RunFinished { req: RequestId },
    Count { req: RequestId, count: u64 },
    Estimate { req: RequestId, count: Option<u64> },
    CommitDone { req: RequestId, counts: Vec<u64> },
    CommitError { req: RequestId, error: String },
    DbOpDone { what: String },
    DbOpFailed { what: String, error: String },
    /// A statement sent to the server, for the console log.
    Console { ts_ms: i64, sql: String },
    /// The running query exceeded the timeout; it keeps running until cancelled.
    Timeout { req: RequestId },
    Error { error: String },
}
