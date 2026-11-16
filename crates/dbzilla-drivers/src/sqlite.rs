//! SQLite driver: `rusqlite` wrapped in `spawn_blocking`.

use crate::traits::*;
use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, ObjectInfo, ObjectKind, TableRef, TableStructure, TriggerInfo};
use dbzilla_core::value::Value;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, InterruptHandle, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// File extensions listed as databases.
const DB_EXTENSIONS: [&str; 3] = ["sqlite", "sqlite3", "db"];
/// Extension of newly created database files.
const NEW_DB_EXTENSION: &str = "sqlite3";
/// Special path of an in-memory database.
const MEMORY_PATH: &str = ":memory:";

/// The SQLite driver.
pub struct SqliteDriver {
    caps: Capabilities,
}

impl SqliteDriver {
    /// Creates the driver.
    pub fn new() -> Self {
        Self { caps: Capabilities::for_driver(DriverId::Sqlite) }
    }
}

impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    fn id(&self) -> DriverId {
        DriverId::Sqlite
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn dialect(&self) -> Box<dyn Dialect> {
        dialect_for(DriverId::Sqlite)
    }

    async fn connect(&self, cfg: &ConnectConfig) -> DriverResult<Box<dyn Conn>> {
        let path = cfg.database.clone();
        let bootstrap = cfg.bootstrap.clone();
        let conn = tokio::task::spawn_blocking(move || -> DriverResult<SqliteConn> {
            let conn = open_file(&path)?;
            for stmt in &bootstrap {
                conn.execute_batch(stmt).map_err(|e| DriverError::Connect(format!("bootstrap command failed: {e}")))?;
            }
            Ok(SqliteConn::new(conn, &path))
        })
        .await
        .map_err(|e| DriverError::Other(e.to_string()))??;
        Ok(Box::new(conn))
    }
}

/// Opens an existing database file (never creates it).
fn open_file(path: &str) -> DriverResult<Connection> {
    if path == MEMORY_PATH {
        return Connection::open_in_memory().map_err(map_err);
    }
    if !Path::new(path).is_file() {
        return Err(DriverError::Connect(format!("database file not found: {path}")));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| DriverError::Connect(format!("cannot open {path}: {e}")))
}

/// Maps a rusqlite error, recognising interrupts as cancellation.
fn map_err(e: rusqlite::Error) -> DriverError {
    if let rusqlite::Error::SqliteFailure(f, _) = &e {
        if f.code == ErrorCode::OperationInterrupted {
            return DriverError::Cancelled;
        }
    }
    DriverError::Query(e.to_string())
}

/// An open SQLite connection.
struct SqliteConn {
    conn: Arc<Mutex<Connection>>,
    path: String,
    interrupt: Arc<InterruptHandle>,
}

impl SqliteConn {
    /// Wraps a connection.
    fn new(conn: Connection, path: &str) -> Self {
        let interrupt = Arc::new(conn.get_interrupt_handle());
        Self { conn: Arc::new(Mutex::new(conn)), path: path.to_owned(), interrupt }
    }

    /// Runs `f` on the blocking pool with the connection locked.
    async fn run<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> DriverResult<T> + Send + 'static) -> DriverResult<T> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| DriverError::Other("connection lock poisoned".into()))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| DriverError::Other(e.to_string()))?
    }

    /// Directory holding the sibling database files.
    fn dir(&self) -> Option<PathBuf> {
        if self.path == MEMORY_PATH {
            return None;
        }
        Path::new(&self.path).parent().map(|p| if p.as_os_str().is_empty() { PathBuf::from(".") } else { p.to_path_buf() })
    }

    /// File name of the open database.
    fn file_name(&self) -> String {
        Path::new(&self.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "main".into())
    }
}

/// Cancels through SQLite's interrupt handle.
struct SqliteCancel(Arc<InterruptHandle>);

#[async_trait]
impl CancelToken for SqliteCancel {
    async fn cancel(&self) -> DriverResult<()> {
        self.0.interrupt();
        Ok(())
    }
}

/// Converts a SQLite value.
fn value_from(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::I64(i),
        ValueRef::Real(f) => Value::F64(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

/// Sends a batch; false when the receiver is gone.
fn send(tx: &RowSink, batch: RowBatch) -> bool {
    tx.blocking_send(batch).is_ok()
}

/// Streams one statement synchronously.
fn stream_query(conn: &Connection, sql: &str, opts: &QueryOpts, tx: &RowSink) -> DriverResult<QuerySummary> {
    let start = Instant::now();
    let mut stmt = conn.prepare(sql).map_err(map_err)?;
    let ncols = stmt.column_count();
    if ncols == 0 {
        let n = stmt.execute([]).map_err(map_err)?;
        return Ok(QuerySummary { affected: Some(n as u64), elapsed: start.elapsed(), messages: vec![] });
    }
    let cols: Vec<ColumnMeta> = stmt
        .columns()
        .enumerate()
        .map(|(i, c)| ColumnMeta {
            name: c.name().to_owned(),
            type_name: c.decl_type().unwrap_or("").to_owned(),
            nullable: true,
            ordinal: i,
            ..Default::default()
        })
        .collect();
    let summary = |elapsed| QuerySummary { affected: None, elapsed, messages: vec![] };
    if !send(tx, RowBatch { columns: Some(cols), rows: vec![] }) {
        return Ok(summary(start.elapsed()));
    }
    let batch_size = opts.batch_size.max(1);
    let mut rows = stmt.query([]).map_err(map_err)?;
    let mut batch = Vec::with_capacity(batch_size);
    let mut total = 0u64;
    while let Some(row) = rows.next().map_err(map_err)? {
        batch.push((0..ncols).map(|i| row.get_ref(i).map(value_from).unwrap_or(Value::Null)).collect());
        total += 1;
        if batch.len() >= batch_size && !send(tx, RowBatch { columns: None, rows: std::mem::take(&mut batch) }) {
            return Ok(summary(start.elapsed()));
        }
        if opts.limit.is_some_and(|l| total >= l) {
            break;
        }
    }
    if !batch.is_empty() {
        send(tx, RowBatch { columns: None, rows: batch });
    }
    Ok(summary(start.elapsed()))
}

/// Quotes an identifier for use inside PRAGMA calls.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Reads the structure of one table or view.
fn read_structure(conn: &Connection, t: &TableRef) -> DriverResult<TableStructure> {
    let name = quote(&t.name);
    let mut st = TableStructure { table: t.clone(), ..Default::default() };
    let mut pk: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(&format!("PRAGMA table_xinfo({name})")).map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?))
            })
            .map_err(map_err)?;
        for row in rows {
            let (cname, ctype, notnull, default, pkpos, hidden) = row.map_err(map_err)?;
            if hidden == 1 {
                continue;
            }
            if pkpos > 0 {
                pk.push((pkpos, cname.clone()));
            }
            st.columns.push(ColumnMeta {
                name: cname,
                type_name: ctype,
                nullable: notnull == 0 && pkpos == 0,
                default,
                is_pk: pkpos > 0,
                ordinal: st.columns.len(),
                ..Default::default()
            });
        }
    }
    pk.sort();
    st.primary_key = pk.into_iter().map(|(_, n)| n).collect();
    read_foreign_keys(conn, &name, &mut st)?;
    read_indexes(conn, &name, &mut st)?;
    st.ddl = conn
        .query_row("SELECT sql FROM sqlite_master WHERE name = ?1 AND type IN ('table','view')", [&t.name], |r| r.get::<_, Option<String>>(0))
        .map_err(map_err)?
        .unwrap_or_default();
    let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name").map_err(map_err)?;
    let triggers = stmt.query_map([&t.name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).map_err(map_err)?;
    for tr in triggers {
        let (tname, sql) = tr.map_err(map_err)?;
        let sql = sql.unwrap_or_default();
        let upper = sql.to_ascii_uppercase();
        let timing = ["INSTEAD OF", "BEFORE", "AFTER"].iter().find(|k| upper.contains(*k)).map_or("", |k| k);
        let event = ["INSERT", "UPDATE", "DELETE"].iter().find(|k| upper.contains(*k)).map_or("", |k| k);
        st.triggers.push(TriggerInfo { name: tname, timing: timing.to_owned(), event: event.to_owned(), statement: sql });
    }
    Ok(st)
}

/// Fills the `fk` field of columns from `PRAGMA foreign_key_list`.
fn read_foreign_keys(conn: &Connection, quoted: &str, st: &mut TableStructure) -> DriverResult<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA foreign_key_list({quoted})")).map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
        })
        .map_err(map_err)?;
    for row in rows {
        let (table, from, to, on_update, on_delete) = row.map_err(map_err)?;
        let Some(col) = st.columns.iter_mut().find(|c| c.name == from) else { continue };
        col.fk = Some(ForeignKeyRef { table: TableRef::simple(&table), columns: to.into_iter().collect(), on_update, on_delete });
    }
    Ok(())
}

/// Fills the index list from `PRAGMA index_list` / `index_info`.
fn read_indexes(conn: &Connection, quoted: &str, st: &mut TableStructure) -> DriverResult<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA index_list({quoted})")).map_err(map_err)?;
    let list: Vec<(String, bool, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0, r.get::<_, String>(3)?)))
        .map_err(map_err)?
        .collect::<Result<_, _>>()
        .map_err(map_err)?;
    for (name, unique, origin) in list {
        if origin == "pk" || name.starts_with("sqlite_autoindex") {
            continue;
        }
        let mut info = conn.prepare(&format!("PRAGMA index_info({})", quote(&name))).map_err(map_err)?;
        let columns: Vec<String> = info
            .query_map([], |r| r.get::<_, Option<String>>(2))
            .map_err(map_err)?
            .map(|c| c.map(|o| o.unwrap_or_else(|| "<expression>".into())))
            .collect::<Result<_, _>>()
            .map_err(map_err)?;
        st.indexes.push(IndexInfo { name, unique, columns, condition: None });
    }
    Ok(())
}

#[async_trait]
impl Conn for SqliteConn {
    async fn server_version(&mut self) -> DriverResult<String> {
        self.run(|c| c.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0)).map(|v| format!("SQLite {v}")).map_err(map_err)).await
    }

    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>> {
        let Some(dir) = self.dir() else { return Ok(vec![DatabaseInfo { name: "main".into() }]) };
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| DriverError::Other(format!("cannot read {}: {e}", dir.display())))?
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| DB_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str())))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names.into_iter().map(|name| DatabaseInfo { name }).collect())
    }

    async fn list_schemas(&mut self, _db: &str) -> DriverResult<Vec<String>> {
        Ok(vec!["main".to_owned()])
    }

    async fn list_objects(&mut self, db: &str, _schema: Option<&str>) -> DriverResult<Vec<ObjectInfo>> {
        if db != self.file_name() && db != "main" {
            self.set_database(db).await?;
        }
        self.run(|c| {
            let mut stmt = c
                .prepare("SELECT type, name FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name COLLATE NOCASE")
                .map_err(map_err)?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                let (kind, name) = row.map_err(map_err)?;
                out.push(ObjectInfo { kind: if kind == "view" { ObjectKind::View } else { ObjectKind::Table }, table: TableRef::simple(&name) });
            }
            Ok(out)
        })
        .await
    }

    async fn table_structure(&mut self, t: &TableRef) -> DriverResult<TableStructure> {
        let t = t.clone();
        self.run(move |c| read_structure(c, &t)).await
    }

    async fn query_stream(&mut self, sql: &str, opts: QueryOpts, tx: RowSink) -> DriverResult<QuerySummary> {
        let sql = sql.to_owned();
        self.run(move |c| stream_query(c, &sql, &opts, &tx)).await
    }

    async fn execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>> {
        let stmts = stmts.to_vec();
        self.run(move |c| {
            if atomic {
                c.execute_batch("BEGIN").map_err(map_err)?;
            }
            let mut counts = Vec::with_capacity(stmts.len());
            for (i, s) in stmts.iter().enumerate() {
                let result = c.prepare(s).and_then(|mut st| if st.column_count() == 0 { st.execute([]) } else { st.query([]).map(|mut r| { while let Ok(Some(_)) = r.next() {} 0 }) });
                match result {
                    Ok(n) => counts.push(n as u64),
                    Err(e) => {
                        if atomic {
                            let _ = c.execute_batch("ROLLBACK");
                        }
                        return Err(DriverError::Query(format!("statement {} failed: {e}", i + 1)));
                    }
                }
            }
            if atomic {
                c.execute_batch("COMMIT").map_err(map_err)?;
            }
            Ok(counts)
        })
        .await
    }

    async fn estimate_count(&mut self, _t: &TableRef) -> DriverResult<Option<u64>> {
        Ok(None)
    }

    async fn cancel_handle(&self) -> DriverResult<Box<dyn CancelToken>> {
        Ok(Box::new(SqliteCancel(Arc::clone(&self.interrupt))))
    }

    async fn ping(&mut self) -> DriverResult<()> {
        self.run(|c| c.query_row("SELECT 1", [], |_| Ok(())).map_err(map_err)).await
    }

    async fn set_database(&mut self, db: &str) -> DriverResult<()> {
        let Some(dir) = self.dir() else { return Ok(()) };
        let path = dir.join(db).to_string_lossy().into_owned();
        let new_path = path.clone();
        let conn = tokio::task::spawn_blocking(move || open_file(&new_path)).await.map_err(|e| DriverError::Other(e.to_string()))??;
        self.interrupt = Arc::new(conn.get_interrupt_handle());
        self.conn = Arc::new(Mutex::new(conn));
        self.path = path;
        Ok(())
    }

    async fn create_database(&mut self, req: &NewDatabase) -> DriverResult<()> {
        let dir = self.dir().ok_or_else(|| DriverError::Unsupported("cannot create a database next to an in-memory database".into()))?;
        let file = if Path::new(&req.name).extension().is_some() { req.name.clone() } else { format!("{}.{NEW_DB_EXTENSION}", req.name) };
        let path = dir.join(file);
        if path.exists() {
            return Err(DriverError::Other(format!("{} already exists", path.display())));
        }
        tokio::task::spawn_blocking(move || Connection::open(&path).map(|_| ()).map_err(map_err)).await.map_err(|e| DriverError::Other(e.to_string()))?
    }

    async fn rename_database(&mut self, from: &str, to: &str) -> DriverResult<()> {
        if from == self.file_name() {
            return Err(DriverError::Other("cannot rename the database that is currently open".into()));
        }
        let dir = self.dir().ok_or_else(|| DriverError::Unsupported("in-memory database".into()))?;
        std::fs::rename(dir.join(from), dir.join(to)).map_err(|e| DriverError::Other(format!("rename failed: {e}")))
    }

    async fn drop_database(&mut self, name: &str) -> DriverResult<()> {
        if name == self.file_name() {
            return Err(DriverError::Other("cannot drop the database that is currently open".into()));
        }
        let dir = self.dir().ok_or_else(|| DriverError::Unsupported("in-memory database".into()))?;
        std::fs::remove_file(dir.join(name)).map_err(|e| DriverError::Other(format!("drop failed: {e}")))
    }
}
