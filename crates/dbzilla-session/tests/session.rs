//! Session actor tests on real SQLite files, plus a fake connection for keep-alive.

use async_trait::async_trait;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::filter::{Filter, FilterColumn, FilterOp};
use dbzilla_core::model::{ColumnMeta, ObjectInfo, TableRef, TableStructure};
use dbzilla_core::paging::SortKey;
use dbzilla_core::value::Value;
use dbzilla_drivers::*;
use dbzilla_session::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const LONG_QUERY: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 50000000) SELECT count(*) FROM c";

fn make_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.sqlite3");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE orders (id INTEGER PRIMARY KEY, name TEXT); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 10) INSERT INTO orders SELECT i, 'n' || i FROM n;").unwrap();
    (dir, path)
}

fn sqlite_cfg(path: &Path) -> ConnectConfig {
    ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: path.to_string_lossy().into_owned(), ssl: SslMode::Disabled, bootstrap: vec![] }
}

fn start(cfg: SessionConfig) -> (SessionHandle, Arc<AtomicUsize>) {
    let repaints = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&repaints);
    let h = spawn(&tokio::runtime::Handle::current(), cfg, default_factory(), Arc::new(move || { r.fetch_add(1, Ordering::SeqCst); }));
    (h, repaints)
}

async fn connect(h: &mut SessionHandle, path: &Path) {
    h.send(Command::Connect(sqlite_cfg(path)));
    let ev = until(h, |e| matches!(e, Event::Connected { .. } | Event::ConnectFailed { .. })).await;
    assert!(matches!(ev.last(), Some(Event::Connected { .. })), "{ev:?}");
}

async fn next(h: &mut SessionHandle) -> Event {
    tokio::time::timeout(Duration::from_secs(30), h.recv()).await.expect("timed out waiting for an event").expect("event channel closed")
}

/// Collects events up to and including the first one matching `pred`.
async fn until(h: &mut SessionHandle, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let e = next(h).await;
        let done = pred(&e);
        out.push(e);
        if done {
            return out;
        }
    }
}

fn rows(events: &[Event]) -> Vec<Vec<Value>> {
    events.iter().filter_map(|e| if let Event::RowBatch { rows, .. } = e { Some(rows.clone()) } else { None }).flatten().collect()
}

fn page(req: RequestId, filters: Vec<Filter>, sort: Vec<SortKey>, limit: u64, offset: u64) -> Command {
    Command::FetchPage(Box::new(PageRequest { req, table: TableRef::simple("orders"), columns: vec![ColumnMeta { name: "id".into(), type_name: "INTEGER".into(), ..Default::default() }], filters, filters_or: false, sort, limit, offset }))
}

fn ge(v: &str) -> Filter {
    Filter { enabled: true, column: FilterColumn::Column("id".into()), op: FilterOp::Ge, value: v.into() }
}

#[tokio::test]
async fn connect_then_introspect() {
    let (_d, path) = make_db();
    let (mut h, repaints) = start(SessionConfig::default());
    h.send(Command::Connect(sqlite_cfg(&path)));
    let ev = until(&mut h, |e| matches!(e, Event::Connected { .. })).await;
    assert!(matches!(&ev[0], Event::Connected { server_version } if server_version.starts_with("SQLite")), "{ev:?}");
    h.send(Command::LoadDatabases);
    assert!(matches!(next(&mut h).await, Event::Databases(d) if d.iter().any(|x| x.name == "main.sqlite3")));
    h.send(Command::LoadSchema { db: "main".into(), schema: None });
    assert!(matches!(next(&mut h).await, Event::Schema { objects, .. } if objects.iter().any(|o: &ObjectInfo| o.table.name == "orders")));
    h.send(Command::LoadStructure(TableRef::simple("orders")));
    assert!(matches!(next(&mut h).await, Event::Structure(st) if { let st: &TableStructure = &st; st.columns.len() == 2 }));
    assert!(repaints.load(Ordering::SeqCst) >= 4, "every event requests a repaint");
}

#[tokio::test]
async fn load_trigger_functions_on_sqlite_returns_an_empty_list() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    h.send(Command::Connect(sqlite_cfg(&path)));
    until(&mut h, |e| matches!(e, Event::Connected { .. })).await;
    h.send(Command::LoadTriggerFunctions { schema: None });
    let events = until(&mut h, |e| matches!(e, Event::TriggerFunctions { .. })).await;
    assert!(matches!(events.last(), Some(Event::TriggerFunctions { schema: None, functions }) if functions.is_empty()));
}

#[tokio::test]
async fn connect_failure_is_reported() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    h.send(Command::Connect(sqlite_cfg(&path.with_file_name("missing.sqlite3"))));
    let ev = until(&mut h, |e| matches!(e, Event::ConnectFailed { .. })).await;
    assert!(matches!(ev.last(), Some(Event::ConnectFailed { error }) if error.contains("missing.sqlite3")));
}

#[tokio::test]
async fn fetch_page_applies_filter_sort_and_offset() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    let req = RequestId::next();
    h.send(page(req, vec![ge("3")], vec![SortKey { column: "id".into(), desc: true }], 3, 1));
    let ev = until(&mut h, |e| matches!(e, Event::QueryDone { .. } | Event::QueryError { .. })).await;
    let ids: Vec<i64> = rows(&ev).iter().map(|r| if let Value::I64(i) = r[0] { i } else { -1 }).collect();
    assert_eq!(ids, vec![9, 8, 7]);
    assert!(matches!(ev.last(), Some(Event::QueryDone { req: r, .. }) if *r == req));
    assert!(ev.iter().any(|e| matches!(e, Event::Console { sql, .. } if sql.contains("ORDER BY \"id\" DESC LIMIT 3 OFFSET 1"))));
}

#[tokio::test]
async fn exact_count_with_filter() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    let req = RequestId::next();
    h.send(Command::CountExact { req, table: TableRef::simple("orders"), columns: vec![], filters: vec![ge("3")], filters_or: false });
    let ev = until(&mut h, |e| matches!(e, Event::Count { .. } | Event::QueryError { .. })).await;
    assert!(matches!(ev.last(), Some(Event::Count { count: 8, .. })), "{ev:?}");
}

#[tokio::test]
async fn run_sql_continues_or_stops_on_error() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    let stmts = vec!["SELECT 1".to_owned(), "SELEC bad".to_owned(), "SELECT 3".to_owned()];
    let run = |stop| Command::RunSql { req: RequestId::next(), statements: stmts.clone(), opts: QueryOpts::default(), stop_on_error: stop };
    h.send(run(false));
    let ev = until(&mut h, |e| matches!(e, Event::RunFinished { .. })).await;
    let done: Vec<usize> = ev.iter().filter_map(|e| if let Event::QueryDone { index, .. } = e { Some(*index) } else { None }).collect();
    let failed: Vec<usize> = ev.iter().filter_map(|e| if let Event::QueryError { index, .. } = e { Some(*index) } else { None }).collect();
    assert_eq!((done, failed), (vec![0, 2], vec![1]));
    h.send(run(true));
    let ev = until(&mut h, |e| matches!(e, Event::RunFinished { .. })).await;
    let done: Vec<usize> = ev.iter().filter_map(|e| if let Event::QueryDone { index, .. } = e { Some(*index) } else { None }).collect();
    assert_eq!(done, vec![0], "execution stops at the failing statement");
    assert!(ev.iter().any(|e| matches!(e, Event::StatementStarted { index: 0, sql, .. } if sql == "SELECT 1")));
}

#[tokio::test]
async fn failed_commit_changes_nothing() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    let req = RequestId::next();
    h.send(Command::Commit { req, statements: vec!["INSERT INTO orders VALUES (100, 'x')".into(), "INSERT INTO nope VALUES (1)".into()] });
    let ev = until(&mut h, |e| matches!(e, Event::CommitDone { .. } | Event::CommitError { .. })).await;
    assert!(matches!(ev.last(), Some(Event::CommitError { error, .. }) if error.contains("statement 2")), "{ev:?}");
    let count = RequestId::next();
    h.send(Command::CountExact { req: count, table: TableRef::simple("orders"), columns: vec![], filters: vec![], filters_or: false });
    assert!(matches!(until(&mut h, |e| matches!(e, Event::Count { .. })).await.last(), Some(Event::Count { count: 10, .. })));
    h.send(Command::Commit { req, statements: vec!["INSERT INTO orders VALUES (100, 'x')".into()] });
    assert!(matches!(until(&mut h, |e| matches!(e, Event::CommitDone { .. } | Event::CommitError { .. })).await.last(), Some(Event::CommitDone { counts, .. }) if counts == &vec![1]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_a_running_statement_and_the_session_survives() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    let req = RequestId::next();
    h.send(Command::RunSql { req, statements: vec![LONG_QUERY.into()], opts: QueryOpts::default(), stop_on_error: false });
    until(&mut h, |e| matches!(e, Event::StatementStarted { .. })).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.send(Command::Cancel(req));
    let ev = until(&mut h, |e| matches!(e, Event::RunFinished { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, Event::QueryError { cancelled: true, .. })), "{ev:?}");
    h.send(Command::RunSql { req: RequestId::next(), statements: vec!["SELECT 7".into()], opts: QueryOpts::default(), stop_on_error: false });
    let ev = until(&mut h, |e| matches!(e, Event::RunFinished { .. })).await;
    assert!(rows(&ev).iter().any(|r| r[0] == Value::I64(7)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_is_signalled_while_the_query_keeps_running() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig { query_timeout: Some(Duration::from_millis(150)), keep_alive: None });
    connect(&mut h, &path).await;
    let req = RequestId::next();
    h.send(Command::RunSql { req, statements: vec![LONG_QUERY.into()], opts: QueryOpts::default(), stop_on_error: false });
    let ev = until(&mut h, |e| matches!(e, Event::Timeout { .. })).await;
    assert!(matches!(ev.last(), Some(Event::Timeout { req: r }) if *r == req));
    h.send(Command::Cancel(req));
    until(&mut h, |e| matches!(e, Event::RunFinished { .. })).await;
}

#[tokio::test]
async fn database_operations() {
    let (_d, path) = make_db();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    h.send(Command::DbOp(DbOp::Create(NewDatabase { name: "other".into(), encoding: None, collation: None })));
    assert!(matches!(next(&mut h).await, Event::DbOpDone { .. }));
    h.send(Command::LoadDatabases);
    assert!(matches!(next(&mut h).await, Event::Databases(d) if d.iter().any(|x| x.name == "other.sqlite3")));
    h.send(Command::UseDatabase("other.sqlite3".into()));
    assert!(matches!(next(&mut h).await, Event::DatabaseChanged(n) if n == "other.sqlite3"));
    h.send(Command::DbOp(DbOp::Drop("main.sqlite3".into())));
    assert!(matches!(next(&mut h).await, Event::DbOpDone { .. }));
    h.send(Command::DbOp(DbOp::Drop("other.sqlite3".into())));
    assert!(matches!(next(&mut h).await, Event::DbOpFailed { .. }), "the open database cannot be dropped");
}

/// Connection whose ping fails from the second call on.
struct FakeConn {
    pings: usize,
}

#[async_trait]
impl Conn for FakeConn {
    async fn server_version(&mut self) -> DriverResult<String> {
        Ok("Fake 1".into())
    }
    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>> {
        Ok(vec![])
    }
    async fn list_schemas(&mut self, _: &str) -> DriverResult<Vec<String>> {
        Ok(vec![])
    }
    async fn list_objects(&mut self, _: &str, _: Option<&str>) -> DriverResult<Vec<ObjectInfo>> {
        Ok(vec![])
    }
    async fn table_structure(&mut self, _: &TableRef) -> DriverResult<TableStructure> {
        Err(DriverError::Unsupported("fake".into()))
    }
    async fn query_stream(&mut self, _: &str, _: QueryOpts, _: RowSink) -> DriverResult<QuerySummary> {
        Err(DriverError::Unsupported("fake".into()))
    }
    async fn execute_batch(&mut self, _: &[String], _: bool) -> DriverResult<Vec<u64>> {
        Err(DriverError::Unsupported("fake".into()))
    }
    async fn estimate_count(&mut self, _: &TableRef) -> DriverResult<Option<u64>> {
        Ok(None)
    }
    async fn cancel_handle(&mut self) -> DriverResult<Box<dyn CancelToken>> {
        Err(DriverError::Unsupported("fake".into()))
    }
    async fn ping(&mut self) -> DriverResult<()> {
        self.pings += 1;
        if self.pings >= 2 { Err(DriverError::Other("server has gone away".into())) } else { Ok(()) }
    }
    async fn set_database(&mut self, _: &str) -> DriverResult<()> {
        Ok(())
    }
    async fn create_database(&mut self, _: &NewDatabase) -> DriverResult<()> {
        Ok(())
    }
    async fn rename_database(&mut self, _: &str, _: &str) -> DriverResult<()> {
        Ok(())
    }
    async fn drop_database(&mut self, _: &str) -> DriverResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn keep_alive_detects_a_dead_connection_and_reconnect_recovers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let factory: ConnFactory = Arc::new(move |_cfg| {
        c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Box::new(FakeConn { pings: 0 }) as Box<dyn Conn>) })
    });
    let mut h = spawn(&tokio::runtime::Handle::current(), SessionConfig { query_timeout: None, keep_alive: Some(Duration::from_millis(40)) }, factory, Arc::new(|| {}));
    h.send(Command::Connect(sqlite_cfg(&PathBuf::from("x"))));
    until(&mut h, |e| matches!(e, Event::Connected { .. })).await;
    let ev = until(&mut h, |e| matches!(e, Event::Disconnected { .. })).await;
    assert!(matches!(ev.last(), Some(Event::Disconnected { reason }) if reason.contains("server has gone away")), "{ev:?}");
    h.send(Command::Reconnect);
    until(&mut h, |e| matches!(e, Event::Connected { .. })).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Review I8: a statement that touches more than one row, or no row, rolls the whole commit back.
#[tokio::test]
async fn commit_checks_affected_rows() {
    let (_d, path) = make_db();
    rusqlite::Connection::open(&path).unwrap().execute_batch("INSERT INTO orders VALUES (11, 'n1');").unwrap();
    let (mut h, _) = start(SessionConfig::default());
    connect(&mut h, &path).await;
    for (sql, word) in [("DELETE FROM orders WHERE name = 'n1'", "2 rows"), ("UPDATE orders SET name = 'x' WHERE id = 999", "no row")] {
        let req = RequestId::next();
        h.send(Command::CommitRows { req, statements: vec!["INSERT INTO orders VALUES (100, 'x')".into(), sql.into()] });
        let ev = until(&mut h, |e| matches!(e, Event::CommitDone { .. } | Event::CommitError { .. })).await;
        assert!(matches!(ev.last(), Some(Event::CommitError { error, .. }) if error.contains("statement 2") && error.contains(word)), "{sql}: {ev:?}");
    }
    let count = RequestId::next();
    h.send(Command::CountExact { req: count, table: TableRef::simple("orders"), columns: vec![], filters: vec![], filters_or: false });
    assert!(matches!(until(&mut h, |e| matches!(e, Event::Count { .. })).await.last(), Some(Event::Count { count: 11, .. })), "both commits were rolled back");
}
