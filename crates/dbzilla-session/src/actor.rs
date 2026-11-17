//! The session actor: owns one connection and serialises all work on it.

use crate::commands::{Command, DbOp, Event, PageRequest, RequestId};
use crate::console::display_for_console;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::filter::Filter;
use dbzilla_core::model::{ColumnMeta, Row, TableRef};
use dbzilla_core::paging::{build_count, build_select};
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, DriverResult, QueryOpts, QuerySummary, RowBatch};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{interval_at, Instant, Interval};

/// Capacity of the actor to UI event channel; a slow UI slows the actor (back-pressure).
const EVENT_CAPACITY: usize = 64;
/// Capacity of the driver to actor row channel.
const ROW_CHANNEL_CAPACITY: usize = 4;
/// Default query timeout (SPEC 14.7).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// Default keep-alive interval (SPEC 14.7).
const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(30);

/// Callback asking the UI for a repaint.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// Creates connections; replaceable in tests.
pub type ConnFactory = Arc<dyn Fn(ConnectConfig) -> Pin<Box<dyn Future<Output = DriverResult<Box<dyn Conn>>> + Send>> + Send + Sync>;

/// The factory using the compiled-in drivers.
pub fn default_factory() -> ConnFactory {
    Arc::new(|cfg| {
        Box::pin(async move {
            let driver = driver_for(cfg.driver)?;
            driver.connect(&cfg).await
        })
    })
}

/// Timing options of a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Signal `Event::Timeout` when a query runs longer; `None` disables it.
    pub query_timeout: Option<Duration>,
    /// Ping interval while idle; `None` disables keep-alive.
    pub keep_alive: Option<Duration>,
}

impl Default for SessionConfig {
    /// 300 s timeout and 30 s keep-alive.
    fn default() -> Self {
        Self { query_timeout: Some(DEFAULT_TIMEOUT), keep_alive: Some(DEFAULT_KEEP_ALIVE) }
    }
}

/// UI side of a session.
pub struct SessionHandle {
    cmd_tx: mpsc::UnboundedSender<Command>,
    ev_rx: mpsc::Receiver<Event>,
}

impl SessionHandle {
    /// Queues a command; ignored when the actor has stopped.
    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Next event if one is ready (never blocks; call each frame).
    pub fn try_recv(&mut self) -> Option<Event> {
        self.ev_rx.try_recv().ok()
    }

    /// Waits for the next event; `None` when the actor has stopped.
    pub async fn recv(&mut self) -> Option<Event> {
        self.ev_rx.recv().await
    }
}

/// Starts an actor on `rt` and returns its handle. The actor stops when the handle is dropped.
pub fn spawn(rt: &tokio::runtime::Handle, cfg: SessionConfig, factory: ConnFactory, repaint: Repaint) -> SessionHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel(EVENT_CAPACITY);
    let actor = Actor { cfg, factory, out: Out { ev: ev_tx, repaint }, cmd_rx, queue: VecDeque::new(), conn: None, last_cfg: None, dialect: None };
    rt.spawn(actor.run());
    SessionHandle { cmd_tx, ev_rx }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Waits for the next keep-alive tick (never when disabled).
async fn tick(t: &mut Option<Interval>) {
    match t {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// Reads an unsigned count from a COUNT(*) cell.
fn as_count(v: &Value) -> Option<u64> {
    match v {
        Value::I64(n) => u64::try_from(*n).ok(),
        Value::U64(n) => Some(*n),
        Value::Decimal(s) | Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

/// Event sink shared with row-forwarding futures; `Sync`, unlike the actor itself.
#[derive(Clone)]
struct Out {
    ev: mpsc::Sender<Event>,
    repaint: Repaint,
}

impl Out {
    /// Sends an event and requests a repaint; false when the UI is gone.
    async fn send(&self, e: Event) -> bool {
        let ok = self.ev.send(e).await.is_ok();
        (self.repaint)();
        ok
    }

    /// Like [`Out::send`] for callers that do not care whether the UI is still there.
    async fn emit(&self, e: Event) {
        self.send(e).await;
    }
}

struct Actor {
    cfg: SessionConfig,
    factory: ConnFactory,
    out: Out,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    queue: VecDeque<Command>,
    conn: Option<Box<dyn Conn>>,
    last_cfg: Option<ConnectConfig>,
    dialect: Option<Box<dyn Dialect>>,
}

impl Actor {
    /// Main loop: commands, queued commands and keep-alive.
    async fn run(mut self) {
        let mut ticker = self.cfg.keep_alive.map(|d| {
            let mut i = interval_at(Instant::now() + d, d);
            i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            i
        });
        loop {
            let cmd = match self.queue.pop_front() {
                Some(c) => c,
                None => tokio::select! {
                    c = self.cmd_rx.recv() => match c { Some(c) => c, None => return },
                    _ = tick(&mut ticker) => {
                        self.keep_alive().await;
                        continue;
                    }
                },
            };
            self.handle(cmd).await;
        }
    }

    /// Pings the server; a failure drops the connection and tells the UI.
    async fn keep_alive(&mut self) {
        let Some(conn) = self.conn.as_mut() else { return };
        if let Err(e) = conn.ping().await {
            self.conn = None;
            self.out.emit(Event::Disconnected { reason: e.to_string() }).await;
        }
    }

    /// Opens a connection through the factory.
    async fn connect(&mut self, cfg: ConnectConfig) {
        match (self.factory)(cfg.clone()).await {
            Ok(mut conn) => {
                let version = conn.server_version().await.unwrap_or_else(|_| "unknown".into());
                self.dialect = Some(dialect_for(cfg.driver));
                self.last_cfg = Some(cfg);
                self.conn = Some(conn);
                self.out.emit(Event::Connected { server_version: version }).await;
            }
            Err(e) => self.out.emit(Event::ConnectFailed { error: e.to_string() }).await,
        }
    }

    /// Dispatches one command.
    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect(cfg) => return self.connect(cfg).await,
            Command::Disconnect => {
                self.conn = None;
                return self.out.emit(Event::Disconnected { reason: "disconnected".into() }).await;
            }
            Command::Reconnect => {
                self.conn = None;
                return match self.last_cfg.clone() {
                    Some(cfg) => self.connect(cfg).await,
                    None => self.out.emit(Event::Error { error: "there is nothing to reconnect to".into() }).await,
                };
            }
            Command::Cancel(_) => return,
            _ => {}
        }
        let Some(mut conn) = self.conn.take() else {
            return self.not_connected(&cmd).await;
        };
        match cmd {
            Command::LoadDatabases => match conn.list_databases().await {
                Ok(d) => self.out.emit(Event::Databases(d)).await,
                Err(e) => self.out.emit(Event::Error { error: e.to_string() }).await,
            },
            Command::LoadSchema { db, schema } => {
                let schemas = conn.list_schemas(&db).await.unwrap_or_default();
                match conn.list_objects(&db, schema.as_deref()).await {
                    Ok(objects) => self.out.emit(Event::Schema { db, schema, schemas, objects }).await,
                    Err(e) => self.out.emit(Event::Error { error: e.to_string() }).await,
                }
            }
            Command::LoadStructure(t) => match conn.table_structure(&t).await {
                Ok(st) => self.out.emit(Event::Structure(Box::new(st))).await,
                Err(e) => self.out.emit(Event::Error { error: format!("cannot read the structure of {}: {e}", t.name) }).await,
            },
            Command::LoadDiagram { db, schema } => match conn.schema_structures(&db, schema.as_deref()).await {
                Ok((tables, skipped)) => self.out.emit(Event::Diagram { schema, tables, skipped }).await,
                Err(e) => self.out.emit(Event::Error { error: format!("cannot read the schema for the diagram: {e}") }).await,
            },
            Command::LoadTriggerFunctions { schema } => match conn.trigger_functions(schema.as_deref()).await {
                Ok(functions) => self.out.emit(Event::TriggerFunctions { schema, functions }).await,
                Err(e) => self.out.emit(Event::Error { error: format!("cannot list the trigger functions: {e}") }).await,
            },
            Command::EstimateCount { req, table } => {
                let count = conn.estimate_count(&table).await.unwrap_or(None);
                self.out.emit(Event::Estimate { req, count }).await;
            }
            Command::UseDatabase(db) => match conn.set_database(&db).await {
                Ok(()) => {
                    self.remember_database(&db);
                    self.out.emit(Event::DatabaseChanged(db)).await;
                }
                Err(e) => self.out.emit(Event::Error { error: e.to_string() }).await,
            },
            Command::DbOp(op) => self.db_op(&mut conn, op).await,
            Command::FetchPage(p) => self.fetch_page(&mut conn, *p).await,
            Command::CountExact { req, table, columns, filters, filters_or } => self.count(&mut conn, req, &table, &columns, &filters, filters_or).await,
            Command::RunSql { req, statements, opts, stop_on_error } => self.run_sql(&mut conn, req, &statements, opts, stop_on_error).await,
            Command::Commit { req, statements } => self.commit(&mut conn, req, &statements, false).await,
            Command::CommitRows { req, statements } => self.commit(&mut conn, req, &statements, true).await,
            Command::Connect(_) | Command::Disconnect | Command::Reconnect | Command::Cancel(_) => {}
        }
        self.conn = Some(conn);
    }

    /// Answers a command that needs a connection when there is none.
    async fn not_connected(&mut self, cmd: &Command) {
        let error = "not connected to a database".to_owned();
        match cmd {
            Command::RunSql { req, .. } => {
                self.out.emit(Event::QueryError { req: *req, index: 0, error, cancelled: false }).await;
                self.out.emit(Event::RunFinished { req: *req }).await;
            }
            Command::FetchPage(p) => self.out.emit(Event::QueryError { req: p.req, index: 0, error, cancelled: false }).await,
            Command::CountExact { req, .. } => self.out.emit(Event::QueryError { req: *req, index: 0, error, cancelled: false }).await,
            Command::Commit { req, .. } | Command::CommitRows { req, .. } => self.out.emit(Event::CommitError { req: *req, error }).await,
            _ => self.out.emit(Event::Error { error }).await,
        }
    }

    /// Keeps the reconnect configuration pointing at the selected database.
    fn remember_database(&mut self, db: &str) {
        let Some(cfg) = self.last_cfg.as_mut() else { return };
        if cfg.driver == dbzilla_core::dialect::DriverId::Sqlite {
            cfg.database = std::path::Path::new(&cfg.database).with_file_name(db).to_string_lossy().into_owned();
        } else {
            cfg.database = db.to_owned();
        }
    }

    /// Database create / rename / drop.
    async fn db_op(&mut self, conn: &mut Box<dyn Conn>, op: DbOp) {
        let (what, result) = match op {
            DbOp::Create(req) => (format!("create database {}", req.name), conn.create_database(&req).await),
            DbOp::Rename { from, to } => (format!("rename database {from} to {to}"), conn.rename_database(&from, &to).await),
            DbOp::Drop(name) => (format!("drop database {name}"), conn.drop_database(&name).await),
        };
        match result {
            Ok(()) => self.out.emit(Event::DbOpDone { what }).await,
            Err(e) => self.out.emit(Event::DbOpFailed { what, error: e.to_string() }).await,
        }
    }

    /// Runs the paged SELECT of a table tab.
    async fn fetch_page(&mut self, conn: &mut Box<dyn Conn>, p: PageRequest) {
        let Some(d) = self.dialect.as_deref() else { return };
        let sql = build_select(d, &p.table, &p.columns, &p.filters, p.filters_or, &p.sort, p.limit, p.offset);
        self.out.emit(Event::Console { ts_ms: now_ms(), sql: display_for_console(&sql) }).await;
        match self.run_one(conn, p.req, 0, &sql, QueryOpts::default(), true).await {
            Ok((summary, _)) => self.out.emit(Event::QueryDone { req: p.req, index: 0, summary }).await,
            Err((error, cancelled)) => self.out.emit(Event::QueryError { req: p.req, index: 0, error, cancelled }).await,
        }
    }

    /// Exact row count.
    async fn count(&mut self, conn: &mut Box<dyn Conn>, req: RequestId, table: &TableRef, cols: &[ColumnMeta], filters: &[Filter], or: bool) {
        let Some(d) = self.dialect.as_deref() else { return };
        let sql = build_count(d, table, cols, filters, or);
        self.out.emit(Event::Console { ts_ms: now_ms(), sql: display_for_console(&sql) }).await;
        match self.run_one(conn, req, 0, &sql, QueryOpts::default(), false).await {
            Ok((_, rows)) => match rows.first().and_then(|r| r.first()).and_then(as_count) {
                Some(count) => self.out.emit(Event::Count { req, count }).await,
                None => self.out.emit(Event::QueryError { req, index: 0, error: "the server returned no count".into(), cancelled: false }).await,
            },
            Err((error, cancelled)) => self.out.emit(Event::QueryError { req, index: 0, error, cancelled }).await,
        }
    }

    /// Runs the statements of the SQL editor one after another.
    async fn run_sql(&mut self, conn: &mut Box<dyn Conn>, req: RequestId, statements: &[String], opts: QueryOpts, stop_on_error: bool) {
        for (index, sql) in statements.iter().enumerate() {
            self.out.emit(Event::StatementStarted { req, index, sql: sql.clone() }).await;
            self.out.emit(Event::Console { ts_ms: now_ms(), sql: display_for_console(sql) }).await;
            match self.run_one(conn, req, index, sql, opts.clone(), true).await {
                Ok((summary, _)) => self.out.emit(Event::QueryDone { req, index, summary }).await,
                Err((error, cancelled)) => {
                    self.out.emit(Event::QueryError { req, index, error, cancelled }).await;
                    if stop_on_error || cancelled {
                        break;
                    }
                }
            }
        }
        self.out.emit(Event::RunFinished { req }).await;
    }

    /// Commits pending-change statements atomically.
    async fn commit(&mut self, conn: &mut Box<dyn Conn>, req: RequestId, statements: &[String], check_rows: bool) {
        for sql in statements {
            self.out.emit(Event::Console { ts_ms: now_ms(), sql: display_for_console(sql) }).await;
        }
        let atomic = self.last_cfg.as_ref().is_none_or(|c| Capabilities::for_driver(c.driver).transactions);
        let driver = self.last_cfg.as_ref().map(|c| c.driver);
        let result = if atomic && check_rows { commit_checked(conn.as_mut(), driver, statements).await } else { conn.execute_batch(statements, atomic).await.map_err(|e| e.to_string()) };
        match result {
            Ok(counts) => self.out.emit(Event::CommitDone { req, counts }).await,
            Err(error) => self.out.emit(Event::CommitError { req, error }).await,
        }
    }

    /// Runs one statement, forwarding or capturing rows, while still serving `Cancel` and timeouts.
    /// Error is `(message, cancelled)`.
    async fn run_one(&mut self, conn: &mut Box<dyn Conn>, req: RequestId, index: usize, sql: &str, opts: QueryOpts, forward: bool) -> Result<(QuerySummary, Vec<Row>), (String, bool)> {
        let token = conn.cancel_handle().await.ok();
        let (dtx, mut drx) = mpsc::channel::<RowBatch>(ROW_CHANNEL_CAPACITY);
        let out = self.out.clone();
        let mut captured: Vec<Row> = Vec::new();
        let forward_fut = async {
            while let Some(b) = drx.recv().await {
                if !forward {
                    captured.extend(b.rows);
                    continue;
                }
                if !out.send(Event::RowBatch { req, index, columns: b.columns, rows: b.rows }).await {
                    break;
                }
            }
        };
        let timeout = self.cfg.query_timeout;
        let result = {
            let joined = async { tokio::join!(conn.query_stream(sql, opts, dtx), forward_fut).0 };
            tokio::pin!(joined);
            let mut timer = timeout.map(|d| Box::pin(tokio::time::sleep(d)));
            let mut cmds_open = true;
            loop {
                tokio::select! {
                    res = &mut joined => break res,
                    cmd = self.cmd_rx.recv(), if cmds_open => match cmd {
                        Some(Command::Cancel(r)) if r == req => {
                            if let Some(t) = &token {
                                let _ = t.cancel().await;
                            }
                        }
                        Some(Command::Cancel(_)) => {}
                        Some(other) => self.queue.push_back(other),
                        None => cmds_open = false,
                    },
                    _ = async { match timer.as_mut() { Some(t) => t.as_mut().await, None => std::future::pending().await } } => {
                        self.out.emit(Event::Timeout { req }).await;
                        if let (Some(t), Some(d)) = (timer.as_mut(), timeout) {
                            t.as_mut().reset(Instant::now() + d);
                        }
                    }
                }
            }
        };
        match result {
            Ok(summary) => Ok((summary, captured)),
            Err(DriverError::Cancelled) => Err(("the statement was cancelled".into(), true)),
            Err(e) => Err((e.to_string(), false)),
        }
    }
}

/// The statement that opens a transaction: MySQL has `START TRANSACTION`, and in T-SQL a bare `BEGIN` starts a block.
fn begin_statement(driver: Option<DriverId>) -> &'static str {
    driver.map_or("BEGIN", |d| dbzilla_core::dialect::dialect_for(d).begin_transaction())
}

/// Runs the statements in one transaction, one at a time. A row statement (UPDATE or DELETE) that
/// touches more than one row, or none, rolls everything back: the key did not identify one row.
/// MySQL reports changed rows rather than matched rows, so "no row" is only checked elsewhere.
async fn commit_checked(conn: &mut dyn Conn, driver: Option<DriverId>, statements: &[String]) -> Result<Vec<u64>, String> {
    conn.execute_batch(&[begin_statement(driver).to_owned()], false).await.map_err(|e| e.to_string())?;
    let mut counts = Vec::with_capacity(statements.len());
    for (i, sql) in statements.iter().enumerate() {
        let outcome = match conn.execute_batch(std::slice::from_ref(sql), false).await {
            Ok(c) => c.first().copied().unwrap_or(0),
            Err(e) => {
                let _ = conn.execute_batch(&["ROLLBACK".to_owned()], false).await;
                return Err(format!("statement {} failed: {}", i + 1, e.to_string().trim_start_matches("statement 1 failed: ")));
            }
        };
        let first = sql.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        let is_row_statement = first == "UPDATE" || first == "DELETE";
        let problem = match outcome {
            n if is_row_statement && n > 1 => Some(format!("it affected {n} rows, the key columns do not identify a single row")),
            0 if is_row_statement && driver != Some(DriverId::MySql) => Some("it matched no row, the data changed since it was loaded".to_owned()),
            _ => None,
        };
        if let Some(p) = problem {
            let _ = conn.execute_batch(&["ROLLBACK".to_owned()], false).await;
            return Err(format!("statement {} failed: {p}", i + 1));
        }
        counts.push(outcome);
    }
    conn.execute_batch(&["COMMIT".to_owned()], false).await.map_err(|e| e.to_string())?;
    Ok(counts)
}

#[cfg(test)]
mod begin_tests {
    use super::*;

    #[test]
    fn each_driver_opens_a_transaction_with_its_own_statement() {
        assert_eq!(begin_statement(Some(DriverId::MySql)), "START TRANSACTION");
        assert_eq!(begin_statement(Some(DriverId::SqlServer)), "BEGIN TRANSACTION");
        assert_eq!(begin_statement(Some(DriverId::Postgres)), "BEGIN");
        assert_eq!(begin_statement(Some(DriverId::Sqlite)), "BEGIN");
        assert_eq!(begin_statement(None), "BEGIN");
    }
}
