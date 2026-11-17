//! SQL Server driver: `tiberius` (TDS over TCP, TLS through rustls).

use crate::traits::*;
use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::ddl::create_from_structure;
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, ObjectInfo, ObjectKind, TableRef, TableStructure, TriggerInfo};
use dbzilla_core::value::Value;
use futures_util::TryStreamExt;
use chrono::{Duration as ChronoDuration, Timelike};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, FromSql, QueryItem, Row};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// Default SQL Server port.
const DEFAULT_PORT: u16 = 1433;
/// Name the server shows for our sessions.
const APPLICATION_NAME: &str = "DBZilla";
/// How long opening the socket and logging in may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Schemas of the engine itself, hidden from the sidebar.
const SYSTEM_SCHEMAS: &str = "N'sys', N'INFORMATION_SCHEMA', N'guest'";

/// The tiberius client over a tokio socket.
type MsClient = Client<Compat<TcpStream>>;

/// The SQL Server driver.
pub struct MsSqlDriver {
    caps: Capabilities,
}

impl MsSqlDriver {
    /// Creates the driver.
    pub fn new() -> Self {
        Self { caps: Capabilities::for_driver(DriverId::SqlServer) }
    }
}

impl Default for MsSqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MsSqlDriver {
    fn id(&self) -> DriverId {
        DriverId::SqlServer
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn dialect(&self) -> Box<dyn Dialect> {
        dialect_for(DriverId::SqlServer)
    }

    async fn connect(&self, cfg: &ConnectConfig) -> DriverResult<Box<dyn Conn>> {
        Ok(Box::new(MsConn::open(cfg.clone()).await?))
    }
}

/// Maps a driver error to the text the server gave, when it gave one.
fn map_err(e: tiberius::error::Error) -> DriverError {
    DriverError::Query(error_text(&e))
}

/// The server message of an error, or its description for transport errors.
fn error_text(e: &tiberius::error::Error) -> String {
    match e {
        tiberius::error::Error::Server(t) => t.message().to_owned(),
        other => other.to_string(),
    }
}

/// True when the error means the socket is gone, so the next call must reconnect.
fn is_broken(e: &tiberius::error::Error) -> bool {
    matches!(e, tiberius::error::Error::Io { .. } | tiberius::error::Error::Tls(_))
}

/// Quotes an identifier as `[name]`.
fn q(name: &str) -> String {
    dialect_for(DriverId::SqlServer).quote_ident(name)
}

/// Quotes a string literal as `N'text'`.
fn lit(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

/// Text of a cell; catalog queries cast everything they read to a string type.
fn text(r: &Row, i: usize) -> String {
    r.get::<&str, _>(i).unwrap_or_default().to_owned()
}

/// Type name as written in DDL: `nvarchar(50)`, `decimal(10,2)`, `datetime2(7)`. `max_length` is in bytes, so
/// the two-byte types halve it; -1 means `max`.
fn type_text(name: &str, max_length: i32, precision: i32, scale: i32) -> String {
    let length = |len: i32| if len < 0 { "max".to_owned() } else { len.to_string() };
    match name {
        "nvarchar" | "nchar" => format!("{name}({})", length(if max_length < 0 { -1 } else { max_length / 2 })),
        "varchar" | "char" | "varbinary" | "binary" => format!("{name}({})", length(max_length)),
        "decimal" | "numeric" => format!("{name}({precision},{scale})"),
        "datetime2" | "datetimeoffset" | "time" => format!("{name}({scale})"),
        _ => name.to_owned(),
    }
}

/// SQL Server stores a default as `(N'x')` or `((0))`: removes the wrapping parentheses, as long as one pair
/// encloses the whole text.
fn strip_default_parens(def: &str) -> String {
    let mut text = def.trim();
    while text.starts_with('(') && text.ends_with(')') && wraps_whole(text) {
        text = text[1..text.len() - 1].trim();
    }
    text.to_owned()
}

/// True when the first `(` of `text` closes at its last character (quoted text is skipped).
fn wraps_whole(text: &str) -> bool {
    let (mut depth, mut quoted) = (0i32, false);
    for (i, c) in text.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth == 0 && i != text.len() - 1 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// First word of a statement, upper-cased, after leading comments (empty for an empty statement).
fn first_word(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').find(|w| !w.is_empty()).unwrap_or_default().to_ascii_uppercase()
}

/// True for a statement that starts with `INSERT`, `UPDATE`, `DELETE` or `MERGE`. Those get a trailing
/// `SELECT ROWCOUNT_BIG()`: it is the only way to read how many rows the statement itself changed (what
/// triggers do is not counted, which `execute` would) while keeping the session scope of the batch.
fn starts_with_dml(sql: &str) -> bool {
    matches!(first_word(sql).as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE")
}

/// Column name of the row count appended to a DML statement.
const COUNT_COLUMN: &str = "__dbzilla_rowcount";

/// The statement followed by the query that reads its row count.
fn with_row_count(sql: &str) -> String {
    format!("{sql}\n;SELECT ROWCOUNT_BIG() AS {COUNT_COLUMN}")
}

/// Rounds to the nearest millisecond.
fn round_to_millis(d: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    let millis = (u64::from(d.nanosecond()) + 500_000) / 1_000_000;
    d.with_nanosecond(0).unwrap_or(d) + ChronoDuration::milliseconds(millis as i64)
}

/// Converts one cell of a row to a [`Value`].
fn to_value(row: &Row, i: usize) -> Value {
    let Ok(data) = row.get_column_data(i) else { return Value::Null };
    match data {
        ColumnData::U8(v) => v.map_or(Value::Null, |n| Value::I64(i64::from(n))),
        ColumnData::I16(v) => v.map_or(Value::Null, |n| Value::I64(i64::from(n))),
        ColumnData::I32(v) => v.map_or(Value::Null, |n| Value::I64(i64::from(n))),
        ColumnData::I64(v) => v.map_or(Value::Null, Value::I64),
        // Going through the shortest text keeps 1.1f32 as 1.1 instead of 1.100000023841858.
        ColumnData::F32(v) => v.map_or(Value::Null, |f| Value::F64(f.to_string().parse().unwrap_or(f64::from(f)))),
        // tiberius reads money and smallmoney as f64; they always carry four decimals.
        ColumnData::F64(v) if matches!(row.columns()[i].column_type(), ColumnType::Money | ColumnType::Money4) => v.map_or(Value::Null, |f| Value::Decimal(format!("{f:.4}"))),
        ColumnData::F64(v) => v.map_or(Value::Null, Value::F64),
        ColumnData::Bit(v) => v.map_or(Value::Null, Value::Bool),
        ColumnData::String(v) => v.as_ref().map_or(Value::Null, |s| Value::Text(s.to_string())),
        ColumnData::Guid(v) => v.map_or(Value::Null, Value::Uuid),
        ColumnData::Binary(v) => v.as_ref().map_or(Value::Null, |b| Value::Bytes(b.to_vec())),
        ColumnData::Numeric(v) => v.map_or(Value::Null, |n| Value::Decimal(n.to_string())),
        ColumnData::Xml(v) => v.as_ref().map_or(Value::Null, |x| Value::Text(x.to_string())),
        // The legacy datetime counts 1/300 s, which tiberius turns into nanoseconds (.997 arrives as .99666...):
        // back to the milliseconds the server stores.
        ColumnData::DateTime(_) => chrono::NaiveDateTime::from_sql(data).ok().flatten().map_or(Value::Null, |d| Value::DateTime(round_to_millis(d))),
        ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => chrono::NaiveDateTime::from_sql(data).ok().flatten().map_or(Value::Null, Value::DateTime),
        ColumnData::Date(_) => chrono::NaiveDate::from_sql(data).ok().flatten().map_or(Value::Null, Value::Date),
        ColumnData::Time(_) => chrono::NaiveTime::from_sql(data).ok().flatten().map_or(Value::Null, Value::Time),
        ColumnData::DateTimeOffset(_) => chrono::DateTime::<chrono::FixedOffset>::from_sql(data).ok().flatten().map_or(Value::Null, Value::DateTimeTz),
    }
}

/// What the rows that follow a metadata item belong to.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Rows,
    Count,
    Ignored,
}

/// Runs one statement and streams its first result set; later result sets are read and dropped. For a DML
/// statement the last result set is the appended row count, which becomes `affected`.
async fn run_statement(client: &mut MsClient, sql: &str, opts: &QueryOpts, tx: &RowSink) -> Result<QuerySummary, tiberius::error::Error> {
    let mut summary = QuerySummary::default();
    let dml = starts_with_dml(sql);
    let mut stream = if dml { client.simple_query(with_row_count(sql)).await? } else { client.simple_query(sql).await? };
    let batch_size = opts.batch_size.max(1);
    let (mut batch, mut total, mut width) = (Vec::with_capacity(batch_size), 0u64, 0usize);
    // `skip`: ignore the rows that follow (limit reached or nobody listens); `closed`: the receiver is gone.
    let (mut skip, mut closed) = (false, false);
    let (mut section, mut streamed) = (Section::Ignored, false);
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(meta) => {
                let is_count = dml && meta.columns().len() == 1 && meta.columns()[0].name() == COUNT_COLUMN;
                section = if is_count {
                    Section::Count
                } else if streamed {
                    Section::Ignored
                } else {
                    Section::Rows
                };
                if section == Section::Rows {
                    streamed = true;
                    let columns: Vec<ColumnMeta> = meta.columns().iter().enumerate().map(|(i, c)| ColumnMeta { name: c.name().to_owned(), type_name: format!("{:?}", c.column_type()).to_lowercase(), nullable: true, ordinal: i, ..Default::default() }).collect();
                    width = columns.len();
                    closed = tx.send(RowBatch { columns: Some(columns), rows: vec![] }).await.is_err();
                    skip = closed;
                }
            }
            QueryItem::Row(row) => match section {
                Section::Count => summary.affected = row.get::<i64, _>(0).and_then(|n| u64::try_from(n).ok()),
                Section::Rows if !skip => {
                    batch.push((0..width).map(|i| to_value(&row, i)).collect());
                    total += 1;
                    if batch.len() >= batch_size && tx.send(RowBatch { columns: None, rows: std::mem::take(&mut batch) }).await.is_err() {
                        closed = true;
                    }
                    skip = closed || opts.limit.is_some_and(|l| total >= l);
                }
                _ => {}
            },
        }
    }
    if !batch.is_empty() && !closed {
        let _ = tx.send(RowBatch { columns: None, rows: batch }).await;
    }
    Ok(summary)
}

/// Runs a DML statement without a sink and returns the rows it changed.
async fn run_dml_counted(client: &mut MsClient, sql: &str) -> Result<u64, tiberius::error::Error> {
    let sets = client.simple_query(with_row_count(sql)).await?.into_results().await?;
    let count = sets.last().and_then(|rows| rows.first()).and_then(|row| row.get::<i64, _>(0)).unwrap_or(0);
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Builds the tiberius settings. Encryption follows the TLS mode: PREFERRED and REQUIRED encrypt without
/// checking the certificate (servers usually have a self-signed one), the VERIFY modes check it, and
/// DISABLED sends no encryption request (the server may still encrypt the login itself).
fn build_config(cfg: &ConnectConfig, database: &str, ssl: SslMode) -> Config {
    let mut c = Config::new();
    c.host(&cfg.host);
    c.port(cfg.port.unwrap_or(DEFAULT_PORT));
    c.application_name(APPLICATION_NAME);
    c.authentication(AuthMethod::sql_server(&cfg.user, cfg.password.as_deref().unwrap_or("")));
    if !database.is_empty() {
        c.database(database);
    }
    match ssl {
        SslMode::Disabled => c.encryption(EncryptionLevel::NotSupported),
        SslMode::Preferred => {
            c.encryption(EncryptionLevel::On);
            c.trust_cert();
        }
        SslMode::Required => {
            c.encryption(EncryptionLevel::Required);
            c.trust_cert();
        }
        SslMode::VerifyCa | SslMode::VerifyFull => c.encryption(EncryptionLevel::Required),
    }
    c
}

/// Why a connection attempt failed: the server answered and said no (wrong password, no access), or the
/// transport or the TLS negotiation did not work.
struct ConnectFailure {
    error: DriverError,
    refused_by_server: bool,
}

/// Opens the socket and logs in.
async fn connect_client(cfg: &ConnectConfig, database: &str, ssl: SslMode) -> Result<MsClient, ConnectFailure> {
    let config = build_config(cfg, database, ssl);
    let addr = config.get_addr();
    let failure = |message: String, refused_by_server: bool| ConnectFailure { error: DriverError::Connect(message), refused_by_server };
    let attempt = async {
        let tcp = TcpStream::connect(&addr).await.map_err(|e| failure(format!("cannot reach {addr}: {e}"), false))?;
        tcp.set_nodelay(true).map_err(|e| failure(e.to_string(), false))?;
        Client::connect(config, tcp.compat_write()).await.map_err(|e| failure(error_text(&e), matches!(e, tiberius::error::Error::Server(_))))
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(failure(format!("timed out connecting to {addr}"), false)),
    }
}

/// Opens a client; PREFERRED falls back to no encryption when the encrypted attempt could not even be made
/// (as the other drivers do), but never after the server refused the login: that would send the password
/// again over a link that is not encrypted.
async fn open_client(cfg: &ConnectConfig, database: &str) -> DriverResult<(MsClient, SslMode)> {
    match connect_client(cfg, database, cfg.ssl).await {
        Ok(c) => Ok((c, cfg.ssl)),
        Err(first) if cfg.ssl == SslMode::Preferred && !first.refused_by_server => match connect_client(cfg, database, SslMode::Disabled).await {
            Ok(c) => Ok((c, SslMode::Disabled)),
            Err(second) => Err(second.error),
        },
        Err(failure) => Err(failure.error),
    }
}

/// What a fresh session tells about itself.
struct SessionInfo {
    spid: i32,
    current_db: String,
    /// `login_time` of the session in ISO 8601, to recognise it again when cancelling.
    login_time: String,
}

/// Session setup: reliable row counts, year-month-day dates whatever the login language, the bootstrap
/// commands, then the session id and current database.
async fn init_session(client: &mut MsClient, cfg: &ConnectConfig) -> DriverResult<SessionInfo> {
    let setup = ["SET NOCOUNT OFF", "SET DATEFORMAT ymd"].into_iter().chain(cfg.bootstrap.iter().map(String::as_str));
    for stmt in setup {
        let outcome = match client.simple_query(stmt).await {
            Ok(stream) => stream.into_results().await.map(|_| ()),
            Err(e) => Err(e),
        };
        outcome.map_err(|e| DriverError::Connect(format!("bootstrap command failed: {}", error_text(&e))))?;
    }
    let row = client
        .simple_query("SELECT CAST(@@SPID AS int), DB_NAME(), CONVERT(varchar(30), login_time, 126) FROM sys.dm_exec_sessions WHERE session_id = @@SPID")
        .await
        .map_err(map_err)?
        .into_row()
        .await
        .map_err(map_err)?;
    let row = row.ok_or_else(|| DriverError::Other("the server did not return its session id".into()))?;
    Ok(SessionInfo { spid: row.get::<i32, _>(0).unwrap_or_default(), current_db: text(&row, 1), login_time: text(&row, 2) })
}

/// State shared between a connection and the cancel tokens made for it.
#[derive(Default)]
struct CancelState {
    /// Wakes the running query so it can give up locally.
    abort: Notify,
    /// True while a statement runs: a token used before or after that does nothing.
    active: AtomicBool,
    /// Set when a token fired, so a broken socket reads as a cancel.
    cancelled: AtomicBool,
}

/// Cancels a running statement. The statement is abandoned locally and the socket is dropped, which makes the
/// server stop the request and roll back its transaction; that needs no permission. Then, best effort, a
/// second connection also runs `KILL <spid>` (SPEC 5.3) to stop work the server would not notice, but only if
/// the session id still belongs to our session: ids are reused quickly.
struct MsCancel {
    cfg: ConnectConfig,
    ssl: SslMode,
    spid: i32,
    login_time: String,
    state: Arc<CancelState>,
    /// A token acts once.
    fired: AtomicBool,
}

#[async_trait]
impl CancelToken for MsCancel {
    async fn cancel(&self) -> DriverResult<()> {
        if !self.state.active.load(Ordering::SeqCst) || self.fired.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.state.cancelled.store(true, Ordering::SeqCst);
        self.state.abort.notify_one();
        let kill = format!(
            "IF EXISTS (SELECT 1 FROM sys.dm_exec_sessions WHERE session_id = {id} AND CONVERT(varchar(30), login_time, 126) = '{time}') KILL {id}",
            id = self.spid,
            time = self.login_time.replace('\'', "")
        );
        if let Ok(mut client) = connect_client(&self.cfg, &self.cfg.database, self.ssl).await {
            if let Ok(stream) = client.simple_query(kill).await {
                let _ = stream.into_results().await;
            }
        }
        Ok(())
    }
}

/// An open connection. `client` is `None` after the socket was dropped on purpose (a cancel) or was lost.
struct MsConn {
    client: Option<MsClient>,
    cfg: ConnectConfig,
    /// TLS mode that actually worked.
    ssl: SslMode,
    /// Database the session works in; restored after a reconnect.
    current_db: String,
    /// Server session id and login time, used to cancel from a second connection.
    spid: i32,
    login_time: String,
    /// State of the next (or running) statement, shared with its cancel token.
    cancel: Arc<CancelState>,
    /// The socket died by itself: like the other drivers, report it instead of opening a new session behind
    /// the user's back (an open transaction and the session settings would be gone).
    lost: bool,
}

impl MsConn {
    /// Connects and prepares the session.
    async fn open(cfg: ConnectConfig) -> DriverResult<Self> {
        let (mut client, ssl) = open_client(&cfg, &cfg.database).await?;
        let info = init_session(&mut client, &cfg).await?;
        Ok(Self { client: Some(client), cfg, ssl, current_db: info.current_db, spid: info.spid, login_time: info.login_time, cancel: Arc::new(CancelState::default()), lost: false })
    }

    /// The live client. After a cancel it reconnects (and returns to `current_db`); after a lost connection it
    /// reports the loss, and the session actor opens a new connection.
    async fn ensure_client(&mut self) -> DriverResult<&mut MsClient> {
        if self.client.is_none() {
            if self.lost {
                return Err(DriverError::Connect("the connection to the server was lost".into()));
            }
            let mut client = connect_client(&self.cfg, &self.current_db, self.ssl).await.map_err(|f| f.error)?;
            let info = init_session(&mut client, &self.cfg).await?;
            self.spid = info.spid;
            self.login_time = info.login_time;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("client was just set"))
    }

    /// Turns a tiberius error into a driver error, dropping the client when the socket is gone.
    fn fail(&mut self, e: tiberius::error::Error) -> DriverError {
        if is_broken(&e) {
            self.client = None;
            self.lost = true;
        }
        map_err(e)
    }

    /// Runs a query and returns the rows of its first result set (introspection helper).
    async fn rows(&mut self, sql: &str) -> DriverResult<Vec<Row>> {
        let client = self.ensure_client().await?;
        let result = match client.simple_query(sql).await {
            Ok(stream) => stream.into_first_result().await,
            Err(e) => Err(e),
        };
        result.map_err(|e| self.fail(e))
    }

    /// Database (quoted and plain), schema and the `OBJECT_ID(..)` expression of a table reference. The
    /// schema defaults to `dbo` and the database to the current one.
    fn locate(&self, t: &TableRef) -> (String, String, String, String) {
        let db = t.database.clone().unwrap_or_else(|| self.current_db.clone());
        let schema = t.schema.clone().unwrap_or_else(|| "dbo".to_owned());
        let oid = format!("OBJECT_ID({})", lit(&format!("{}.{}.{}", q(&db), q(&schema), q(&t.name))));
        (q(&db), db, schema, oid)
    }

    /// Runs one statement and returns the rows it changed (0 when the server does not report a count).
    async fn run_counted(&mut self, sql: &str) -> DriverResult<u64> {
        if !starts_with_dml(sql) {
            return self.run(sql).await.map(|()| 0);
        }
        let client = self.ensure_client().await?;
        let result = run_dml_counted(client, sql).await;
        result.map_err(|e| self.fail(e))
    }

    /// Reads the database the session is in, after a `USE` typed in the editor.
    async fn refresh_current_db(&mut self) {
        if let Some(name) = self.rows("SELECT DB_NAME()").await.ok().and_then(|rows| rows.first().map(|r| text(r, 0))) {
            self.current_db = name;
        }
    }

    /// Leaves the database `name` for `master` when the session is inside it, so it can be renamed or dropped.
    async fn leave_database(&mut self, name: &str) -> DriverResult<()> {
        if self.current_db.eq_ignore_ascii_case(name) {
            self.set_database("master").await?;
        }
        Ok(())
    }

    /// Runs a statement and discards its results.
    async fn run(&mut self, sql: &str) -> DriverResult<()> {
        let client = self.ensure_client().await?;
        let result = match client.simple_query(sql).await {
            Ok(stream) => stream.into_results().await.map(|_| ()),
            Err(e) => Err(e),
        };
        result.map_err(|e| self.fail(e))
    }
}

#[async_trait]
impl Conn for MsConn {
    async fn server_version(&mut self) -> DriverResult<String> {
        let rows = self.rows("SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(50))").await?;
        Ok(format!("Microsoft SQL Server {}", rows.first().map(|r| text(r, 0)).unwrap_or_default()))
    }

    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>> {
        let rows = self.rows("SELECT CAST(name AS nvarchar(256)) FROM sys.databases WHERE HAS_DBACCESS(name) = 1 ORDER BY name").await?;
        Ok(rows.iter().map(|r| DatabaseInfo { name: text(r, 0) }).collect())
    }

    async fn list_schemas(&mut self, db: &str) -> DriverResult<Vec<String>> {
        let sql = format!("SELECT CAST(name AS nvarchar(256)) FROM {}.sys.schemas WHERE name NOT IN ({SYSTEM_SCHEMAS}) AND name NOT LIKE N'db[_]%' ORDER BY name", q(db));
        Ok(self.rows(&sql).await?.iter().map(|r| text(r, 0)).collect())
    }

    async fn list_objects(&mut self, db: &str, schema: Option<&str>) -> DriverResult<Vec<ObjectInfo>> {
        let only_schema = schema.map(|s| format!(" AND s.name = {}", lit(s))).unwrap_or_default();
        let sql = format!(
            "SELECT CAST(s.name AS nvarchar(256)), CAST(o.name AS nvarchar(256)), RTRIM(o.type) FROM {d}.sys.objects o JOIN {d}.sys.schemas s ON s.schema_id = o.schema_id \
             WHERE o.type IN ('U', 'V', 'FN', 'IF', 'TF', 'P') AND o.is_ms_shipped = 0 AND s.name NOT IN ({SYSTEM_SCHEMAS}){only_schema} ORDER BY o.name",
            d = q(db)
        );
        let mut out = Vec::new();
        for r in self.rows(&sql).await? {
            let kind = match text(&r, 2).as_str() {
                "U" => ObjectKind::Table,
                "V" => ObjectKind::View,
                "P" => ObjectKind::Procedure,
                _ => ObjectKind::Function,
            };
            out.push(ObjectInfo { kind, table: TableRef { database: Some(db.to_owned()), schema: Some(text(&r, 0)), name: text(&r, 1) } });
        }
        Ok(out)
    }

    async fn table_structure(&mut self, t: &TableRef) -> DriverResult<TableStructure> {
        let (qdb, db, schema, oid) = self.locate(t);
        let mut st = TableStructure { table: TableRef { database: Some(db), schema: Some(schema), name: t.name.clone() }, ..Default::default() };
        let cols = self
            .rows(&format!(
                "SELECT CAST(c.name AS nvarchar(256)), CAST(t.name AS nvarchar(128)), CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int), \
                 CAST(c.is_nullable AS int), CAST(c.is_identity AS int), CAST(dc.definition AS nvarchar(4000)), CAST(c.collation_name AS nvarchar(128)), CAST(c.is_computed AS int) \
                 FROM {qdb}.sys.columns c JOIN {qdb}.sys.types t ON t.user_type_id = c.user_type_id \
                 LEFT JOIN {qdb}.sys.default_constraints dc ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
                 WHERE c.object_id = {oid} ORDER BY c.column_id"
            ))
            .await?;
        for (i, r) in cols.iter().enumerate() {
            let int = |n: usize| r.get::<i32, _>(n).unwrap_or_default();
            let mut meta = ColumnMeta {
                name: text(r, 0),
                type_name: type_text(&text(r, 1), int(2), int(3), int(4)),
                nullable: int(5) == 1,
                default: r.get::<&str, _>(7).map(strip_default_parens),
                ordinal: i,
                ..Default::default()
            };
            if int(6) == 1 {
                meta.extra.insert("identity".into(), "true".into());
            }
            if int(9) == 1 {
                meta.extra.insert("computed".into(), "true".into());
            }
            if let Some(collation) = r.get::<&str, _>(8).filter(|c| !c.is_empty()) {
                meta.extra.insert("collation".into(), collation.to_owned());
            }
            st.columns.push(meta);
        }
        let key_columns = |filter: &str| {
            format!(
                "SELECT CAST(i.name AS nvarchar(256)), CAST(i.is_unique AS int), CAST(c.name AS nvarchar(256)), CAST(i.is_primary_key AS int), CAST(i.filter_definition AS nvarchar(4000)) \
                 FROM {qdb}.sys.indexes i JOIN {qdb}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
                 JOIN {qdb}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                 WHERE i.object_id = {oid} AND i.type > 0 AND ic.is_included_column = 0 {filter} ORDER BY i.index_id, ic.key_ordinal"
            )
        };
        for r in self.rows(&key_columns("")).await? {
            let (name, col) = (text(&r, 0), text(&r, 2));
            if r.get::<i32, _>(3) == Some(1) {
                st.primary_key.push(col.clone());
                if let Some(c) = st.columns.iter_mut().find(|c| c.name == col) {
                    c.is_pk = true;
                }
            }
            match st.indexes.last_mut() {
                Some(last) if last.name == name => last.columns.push(col),
                _ => st.indexes.push(IndexInfo { name, unique: r.get::<i32, _>(1) == Some(1), columns: vec![col], condition: r.get::<&str, _>(4).map(str::to_owned) }),
            }
        }
        let fks = self
            .rows(&format!(
                "SELECT CAST(fk.name AS nvarchar(256)), CAST(pc.name AS nvarchar(256)), CAST(rs.name AS nvarchar(256)), CAST(rt.name AS nvarchar(256)), CAST(rc.name AS nvarchar(256)), \
                 CAST(fk.update_referential_action_desc AS nvarchar(30)), CAST(fk.delete_referential_action_desc AS nvarchar(30)) \
                 FROM {qdb}.sys.foreign_keys fk JOIN {qdb}.sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
                 JOIN {qdb}.sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
                 JOIN {qdb}.sys.tables rt ON rt.object_id = fkc.referenced_object_id JOIN {qdb}.sys.schemas rs ON rs.schema_id = rt.schema_id \
                 JOIN {qdb}.sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
                 WHERE fk.parent_object_id = {oid} ORDER BY fk.object_id, fkc.constraint_column_id"
            ))
            .await?;
        let referenced_db = st.table.database.clone();
        for r in &fks {
            let col = text(r, 1);
            if let Some(c) = st.columns.iter_mut().find(|c| c.name == col) {
                c.fk = Some(ForeignKeyRef {
                    table: TableRef { database: referenced_db.clone(), schema: Some(text(r, 2)), name: text(r, 3) },
                    columns: vec![text(r, 4)],
                    on_update: text(r, 5).replace('_', " "),
                    on_delete: text(r, 6).replace('_', " "),
                    name: Some(text(r, 0)),
                });
            }
        }
        let triggers = self
            .rows(&format!(
                "SELECT CAST(tr.name AS nvarchar(256)), CASE WHEN tr.is_instead_of_trigger = 1 THEN N'INSTEAD OF' ELSE N'AFTER' END, \
                 CAST(STUFF((SELECT N', ' + te.type_desc FROM {qdb}.sys.trigger_events te WHERE te.object_id = tr.object_id ORDER BY te.type FOR XML PATH(N'')), 1, 2, N'') AS nvarchar(100)), \
                 CAST(m.definition AS nvarchar(max)) FROM {qdb}.sys.triggers tr LEFT JOIN {qdb}.sys.sql_modules m ON m.object_id = tr.object_id \
                 WHERE tr.parent_id = {oid} ORDER BY tr.name"
            ))
            .await?;
        st.triggers = triggers
            .iter()
            .map(|r| TriggerInfo { name: text(r, 0), timing: text(r, 1), event: text(r, 2), statement: text(r, 3), level: "STATEMENT".into(), function: None, editable: false })
            .collect();
        let kind = self.rows(&format!("SELECT RTRIM(type) FROM {qdb}.sys.objects WHERE object_id = {oid}")).await?;
        st.ddl = if kind.first().is_some_and(|r| text(r, 0) == "V") {
            self.rows(&format!("SELECT CAST(definition AS nvarchar(max)) FROM {qdb}.sys.sql_modules WHERE object_id = {oid}")).await?.first().map(|r| text(r, 0)).unwrap_or_default()
        } else {
            create_from_structure(&st, dialect_for(DriverId::SqlServer).as_ref())
        };
        Ok(st)
    }

    async fn query_stream(&mut self, sql: &str, opts: QueryOpts, tx: RowSink) -> DriverResult<QuerySummary> {
        let start = Instant::now();
        self.ensure_client().await?;
        let state = Arc::clone(&self.cancel);
        state.active.store(true, Ordering::SeqCst);
        // No timeout here: like the other drivers, how long a query may run is up to the session (it asks the user).
        let outcome = {
            let client = self.client.as_mut().expect("client was just ensured");
            tokio::select! {
                result = run_statement(client, sql, &opts, &tx) => Some(result),
                () = state.abort.notified() => None,
            }
        };
        state.active.store(false, Ordering::SeqCst);
        let cancelled = state.cancelled.load(Ordering::SeqCst);
        // Tokens made so far are spent; the next statement gets a fresh state.
        self.cancel = Arc::new(CancelState::default());
        match outcome {
            None => {
                self.client = None;
                Err(DriverError::Cancelled)
            }
            Some(Err(_)) if cancelled => {
                self.client = None;
                Err(DriverError::Cancelled)
            }
            Some(Err(e)) => Err(self.fail(e)),
            Some(Ok(mut summary)) => {
                summary.elapsed = start.elapsed();
                if cancelled {
                    // A cancel that arrived as the statement ended: the session is being killed.
                    self.client = None;
                } else if first_word(sql) == "USE" {
                    self.refresh_current_db().await;
                }
                Ok(summary)
            }
        }
    }

    async fn execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>> {
        if atomic {
            self.run("BEGIN TRANSACTION").await?;
        }
        let mut counts = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.iter().enumerate() {
            match self.run_counted(stmt).await {
                Ok(n) => counts.push(n),
                Err(e) => {
                    if atomic {
                        let _ = self.run("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
                    }
                    let message = match e {
                        DriverError::Query(m) => m,
                        other => other.to_string(),
                    };
                    return Err(DriverError::Query(format!("statement {} failed: {message}", i + 1)));
                }
            }
        }
        if atomic {
            self.run("COMMIT TRANSACTION").await?;
        }
        Ok(counts)
    }

    async fn estimate_count(&mut self, t: &TableRef) -> DriverResult<Option<u64>> {
        let (qdb, _, _, oid) = self.locate(t);
        let rows = self.rows(&format!("SELECT CAST(SUM(p.rows) AS bigint) FROM {qdb}.sys.partitions p WHERE p.object_id = {oid} AND p.index_id IN (0, 1)")).await?;
        Ok(rows.first().and_then(|r| r.get::<i64, _>(0)).and_then(|n| u64::try_from(n).ok()))
    }

    async fn cancel_handle(&mut self) -> DriverResult<Box<dyn CancelToken>> {
        // After a cancel the session is gone: reconnect first, so the token holds the id of the live one.
        self.ensure_client().await?;
        Ok(Box::new(MsCancel { cfg: self.cfg.clone(), ssl: self.ssl, spid: self.spid, login_time: self.login_time.clone(), state: Arc::clone(&self.cancel), fired: AtomicBool::new(false) }))
    }

    async fn ping(&mut self) -> DriverResult<()> {
        self.run("SELECT 1").await
    }

    async fn set_database(&mut self, db: &str) -> DriverResult<()> {
        self.run(&format!("USE {}", q(db))).await?;
        self.current_db = db.to_owned();
        Ok(())
    }

    async fn create_database(&mut self, req: &NewDatabase) -> DriverResult<()> {
        let collate = match req.collation.as_deref().filter(|c| !c.is_empty()) {
            // The collation name is part of the statement text, so it must be a plain name.
            Some(c) if !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') => return Err(DriverError::Query(format!("'{c}' is not a valid collation name"))),
            Some(c) => format!(" COLLATE {c}"),
            None => String::new(),
        };
        self.run(&format!("CREATE DATABASE {}{collate}", q(&req.name))).await
    }

    async fn rename_database(&mut self, from: &str, to: &str) -> DriverResult<()> {
        self.leave_database(from).await?;
        // The server refuses while other sessions use the database: that is reported, nobody is disconnected.
        self.run(&format!("ALTER DATABASE {} MODIFY NAME = {}", q(from), q(to))).await
    }

    async fn drop_database(&mut self, name: &str) -> DriverResult<()> {
        self.leave_database(name).await?;
        let db = q(name);
        self.run(&format!("ALTER DATABASE {db} SET SINGLE_USER WITH ROLLBACK IMMEDIATE")).await?;
        let dropped = self.run(&format!("DROP DATABASE {db}")).await;
        if dropped.is_err() {
            let _ = self.run(&format!("ALTER DATABASE {db} SET MULTI_USER")).await;
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_text_shows_length_precision_and_scale() {
        assert_eq!(type_text("nvarchar", 100, 0, 0), "nvarchar(50)");
        assert_eq!(type_text("nvarchar", -1, 0, 0), "nvarchar(max)");
        assert_eq!(type_text("varchar", 20, 0, 0), "varchar(20)");
        assert_eq!(type_text("varbinary", -1, 0, 0), "varbinary(max)");
        assert_eq!(type_text("decimal", 9, 10, 2), "decimal(10,2)");
        assert_eq!(type_text("datetime2", 8, 27, 7), "datetime2(7)");
        assert_eq!(type_text("int", 4, 10, 0), "int");
        assert_eq!(type_text("bit", 1, 1, 0), "bit");
    }

    #[test]
    fn dml_is_told_apart_by_its_first_word() {
        for sql in ["UPDATE t SET a = 1", "  -- note\n insert into t values (1)", "/* x */DELETE FROM t", "merge t using s on 1=1 when matched then delete;", "INSERT INTO t OUTPUT inserted.id VALUES (1)"] {
            assert!(starts_with_dml(sql), "{sql}");
        }
        for sql in ["SELECT 1", "WITH x AS (SELECT 1) UPDATE t SET a = 1", "EXEC p", "USE db", "CREATE TABLE t (a int)", ""] {
            assert!(!starts_with_dml(sql), "{sql}");
        }
        assert_eq!(first_word("/* c */ use  [db]"), "USE");
    }

    #[test]
    fn the_legacy_datetime_is_rounded_to_milliseconds() {
        let d = chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
        assert_eq!(round_to_millis(d.and_hms_nano_opt(3, 4, 5, 996_666_666).unwrap()), d.and_hms_milli_opt(3, 4, 5, 997).unwrap());
        assert_eq!(round_to_millis(d.and_hms_nano_opt(3, 4, 5, 3_333_333).unwrap()), d.and_hms_milli_opt(3, 4, 5, 3).unwrap());
        assert_eq!(round_to_millis(d.and_hms_nano_opt(3, 4, 5, 999_700_000).unwrap()), d.and_hms_opt(3, 4, 6).unwrap());
    }

    #[test]
    fn default_loses_the_parentheses_sql_server_adds() {
        assert_eq!(strip_default_parens("(N'x')"), "N'x'");
        assert_eq!(strip_default_parens("((0))"), "0");
        assert_eq!(strip_default_parens("(getdate())"), "getdate()");
        assert_eq!(strip_default_parens("(1)+(2)"), "(1)+(2)");
        assert_eq!(strip_default_parens("N'x'"), "N'x'");
    }
}
