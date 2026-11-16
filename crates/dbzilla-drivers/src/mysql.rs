//! MySQL / MariaDB driver: `mysql_async` (text protocol, typed by column metadata).

use crate::traits::*;
use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, ObjectInfo, ObjectKind, TableRef, TableStructure, TriggerInfo};
use dbzilla_core::value::Value;
use mysql_async::consts::ColumnType;
use mysql_async::prelude::*;
use mysql_async::{Column, Conn as MyConnection, Opts, OptsBuilder, Row, SslOpts, Value as MyValue};
use std::time::Instant;

/// Default MySQL port.
const DEFAULT_PORT: u16 = 3306;
/// Character set id of binary strings.
const BINARY_CHARSET: u16 = 63;
/// Server error: query execution was interrupted (KILL QUERY).
const ER_QUERY_INTERRUPTED: u16 = 1317;

/// The MySQL / MariaDB driver.
pub struct MySqlDriver {
    caps: Capabilities,
}

impl MySqlDriver {
    /// Creates the driver.
    pub fn new() -> Self {
        Self { caps: Capabilities::for_driver(DriverId::MySql) }
    }
}

impl Default for MySqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MySqlDriver {
    fn id(&self) -> DriverId {
        DriverId::MySql
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn dialect(&self) -> Box<dyn Dialect> {
        dialect_for(DriverId::MySql)
    }

    async fn connect(&self, cfg: &ConnectConfig) -> DriverResult<Box<dyn Conn>> {
        Ok(Box::new(MyConn::open(cfg.clone()).await?))
    }
}

/// Maps a driver error, recognising KILL QUERY as cancellation.
fn map_err(e: mysql_async::Error) -> DriverError {
    match &e {
        mysql_async::Error::Server(s) if s.code == ER_QUERY_INTERRUPTED => DriverError::Cancelled,
        mysql_async::Error::Server(s) => DriverError::Query(format!("{} (error {})", s.message, s.code)),
        other => DriverError::Query(other.to_string()),
    }
}

/// True when the error is the KILL QUERY interruption.
fn is_interrupted(e: &mysql_async::Error) -> bool {
    matches!(e, mysql_async::Error::Server(s) if s.code == ER_QUERY_INTERRUPTED)
}

/// Connection options for a config, with or without TLS.
fn build_opts(cfg: &ConnectConfig, ssl: SslMode) -> Opts {
    let tls = match ssl {
        SslMode::Disabled => None,
        SslMode::Preferred | SslMode::Required => Some(SslOpts::default().with_danger_accept_invalid_certs(true).with_danger_skip_domain_validation(true)),
        SslMode::VerifyCa | SslMode::VerifyFull => Some(SslOpts::default()),
    };
    OptsBuilder::default()
        .ip_or_hostname(if cfg.host.is_empty() { "localhost".to_owned() } else { cfg.host.clone() })
        .tcp_port(cfg.port.unwrap_or(DEFAULT_PORT))
        .user(Some(if cfg.user.is_empty() { "root".to_owned() } else { cfg.user.clone() }))
        .pass(cfg.password.clone())
        .db_name(if cfg.database.is_empty() { None } else { Some(cfg.database.clone()) })
        .prefer_socket(false)
        .ssl_opts(tls)
        .into()
}

/// An open MySQL connection.
struct MyConn {
    conn: MyConnection,
    cfg: ConnectConfig,
    /// TLS mode that actually worked (PREFERRED may fall back to plain).
    ssl: SslMode,
}

impl MyConn {
    /// Connects (PREFERRED falls back to plain) and runs the bootstrap commands.
    async fn open(cfg: ConnectConfig) -> DriverResult<Self> {
        let mut ssl = cfg.ssl;
        let conn = match MyConnection::new(build_opts(&cfg, ssl)).await {
            Ok(c) => c,
            Err(_) if ssl == SslMode::Preferred => {
                ssl = SslMode::Disabled;
                MyConnection::new(build_opts(&cfg, ssl)).await.map_err(|e| DriverError::Connect(connect_message(&e)))?
            }
            Err(e) => return Err(DriverError::Connect(connect_message(&e))),
        };
        let mut me = Self { conn, cfg: cfg.clone(), ssl };
        me.drop_no_backslash_escapes().await?;
        for stmt in &cfg.bootstrap {
            me.conn.query_drop(stmt).await.map_err(|e| DriverError::Connect(format!("bootstrap command failed: {}", match map_err(e) { DriverError::Query(m) => m, other => other.to_string() })))?;
        }
        Ok(me)
    }

    /// Quoting and statement splitting assume backslash escapes, so a session running with
    /// `NO_BACKSLASH_ESCAPES` would store `C:\\tmp` wrongly and misread string literals: switch it off for this session.
    async fn drop_no_backslash_escapes(&mut self) -> DriverResult<()> {
        let mode: Option<String> = self.conn.query_first("SELECT @@SESSION.sql_mode").await.map_err(map_err)?;
        let Some(mode) = mode.filter(|m| m.contains("NO_BACKSLASH_ESCAPES")) else { return Ok(()) };
        let kept: Vec<&str> = mode.split(',').filter(|m| !m.is_empty() && *m != "NO_BACKSLASH_ESCAPES").collect();
        self.conn.query_drop(format!("SET SESSION sql_mode = '{}'", kept.join(","))).await.map_err(map_err)
    }

    /// Runs a query returning rows as plain strings (introspection helper).
    async fn rows(&mut self, sql: &str) -> DriverResult<Vec<Row>> {
        self.conn.query(sql).await.map_err(map_err)
    }

    /// Name of the selected database.
    async fn current_db(&mut self) -> DriverResult<String> {
        let db: Option<Option<String>> = self.conn.query_first("SELECT DATABASE()").await.map_err(map_err)?;
        Ok(db.flatten().unwrap_or_default())
    }
}

/// Friendly connect error text.
fn connect_message(e: &mysql_async::Error) -> String {
    match e {
        mysql_async::Error::Server(s) => format!("{} (error {})", s.message, s.code),
        other => other.to_string(),
    }
}

/// Quotes a MySQL identifier.
fn q(name: &str) -> String {
    dialect_for(DriverId::MySql).quote_ident(name)
}

/// Quotes a string literal.
fn lit(s: &str) -> String {
    dialect_for(DriverId::MySql).literal(&Value::Text(s.to_owned()))
}

/// Reads a string column of an introspection row.
fn text(row: &Row, i: usize) -> String {
    match row.as_ref(i) {
        Some(MyValue::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(MyValue::NULL) | None => String::new(),
        Some(other) => format!("{other:?}"),
    }
}

/// Reads an optional string column.
fn opt_text(row: &Row, i: usize) -> Option<String> {
    match row.as_ref(i) {
        Some(MyValue::NULL) | None => None,
        _ => Some(text(row, i)),
    }
}

/// Friendly SQL type name of a result column.
fn type_name(c: &Column) -> String {
    use ColumnType::*;
    let binary = c.character_set() == BINARY_CHARSET;
    match c.column_type() {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => if binary { "varbinary" } else { "varchar" },
        MYSQL_TYPE_STRING => if binary { "binary" } else { "char" },
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => if binary { "blob" } else { "text" },
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_GEOMETRY => "geometry",
        _ => "unknown",
    }
    .to_owned()
}

/// Converts a driver value, using the column metadata to type text-protocol bytes.
fn to_value(v: &MyValue, col: &Column) -> Value {
    match v {
        MyValue::NULL => Value::Null,
        MyValue::Int(i) => Value::I64(*i),
        MyValue::UInt(u) => i64::try_from(*u).map_or(Value::U64(*u), Value::I64),
        MyValue::Float(f) => Value::F64(f64::from(*f)),
        MyValue::Double(d) => Value::F64(*d),
        MyValue::Date(y, m, d, h, mi, s, us) => {
            let date = NaiveDate::from_ymd_opt(i32::from(*y), u32::from(*m), u32::from(*d));
            let time = NaiveTime::from_hms_micro_opt(u32::from(*h), u32::from(*mi), u32::from(*s), *us);
            match (date, time) {
                (Some(d), Some(t)) => Value::DateTime(NaiveDateTime::new(d, t)),
                _ => Value::Other(format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")),
            }
        }
        MyValue::Time(neg, days, h, m, s, _) => {
            let hours = days * 24 + u32::from(*h);
            Value::Other(format!("{}{hours:02}:{m:02}:{s:02}", if *neg { "-" } else { "" }))
        }
        MyValue::Bytes(b) => from_text(b, col),
    }
}

/// Parses text-protocol bytes according to the column type.
fn from_text(b: &[u8], col: &Column) -> Value {
    use ColumnType::*;
    let s = String::from_utf8_lossy(b);
    let binary = col.character_set() == BINARY_CHARSET;
    match col.column_type() {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => {
            if let Ok(i) = s.parse::<i64>() {
                Value::I64(i)
            } else if let Ok(u) = s.parse::<u64>() {
                Value::U64(u)
            } else {
                Value::Text(s.into_owned())
            }
        }
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => s.parse::<f64>().map_or_else(|_| Value::Text(s.to_string()), Value::F64),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Value::Decimal(s.into_owned()),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_or_else(|_| Value::Other(s.to_string()), Value::Date),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 | MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => {
            NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f").map_or_else(|_| Value::Other(s.to_string()), Value::DateTime)
        }
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => NaiveTime::parse_from_str(&s, "%H:%M:%S%.f").map_or_else(|_| Value::Other(s.to_string()), Value::Time),
        MYSQL_TYPE_JSON => serde_json::from_str(&s).map_or_else(|_| Value::Text(s.to_string()), Value::Json),
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY => Value::Bytes(b.to_vec()),
        _ if binary => Value::Bytes(b.to_vec()),
        _ => Value::Text(s.into_owned()),
    }
}

/// Turns the information_schema default into an SQL expression usable in DDL.
fn normalize_default(raw: Option<String>, extra: &str) -> Option<String> {
    let raw = raw?;
    let upper = raw.to_ascii_uppercase();
    if upper == "NULL" {
        return None;
    }
    let is_expr = raw.starts_with('\'') || extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") || raw.parse::<f64>().is_ok() || upper.starts_with("CURRENT_TIMESTAMP") || raw.ends_with(')');
    Some(if is_expr { raw } else { lit(&raw) })
}

/// Cancels by running `KILL QUERY` on a second connection.
struct MyCancel {
    cfg: ConnectConfig,
    ssl: SslMode,
    id: u32,
}

#[async_trait]
impl CancelToken for MyCancel {
    async fn cancel(&self) -> DriverResult<()> {
        let mut conn = MyConnection::new(build_opts(&self.cfg, self.ssl)).await.map_err(|e| DriverError::Connect(connect_message(&e)))?;
        conn.query_drop(format!("KILL QUERY {}", self.id)).await.map_err(map_err)?;
        let _ = conn.disconnect().await;
        Ok(())
    }
}

#[async_trait]
impl Conn for MyConn {
    async fn server_version(&mut self) -> DriverResult<String> {
        let v: Option<String> = self.conn.query_first("SELECT VERSION()").await.map_err(map_err)?;
        let v = v.unwrap_or_default();
        let number = v.split('-').next().unwrap_or(&v);
        Ok(format!("{} {number}", if v.to_ascii_lowercase().contains("mariadb") { "MariaDB" } else { "MySQL" }))
    }

    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>> {
        let rows = self.rows("SHOW DATABASES").await?;
        let mut names: Vec<String> = rows.iter().map(|r| text(r, 0)).collect();
        names.sort_by_key(|n| n.to_lowercase());
        Ok(names.into_iter().map(|name| DatabaseInfo { name }).collect())
    }

    async fn list_schemas(&mut self, db: &str) -> DriverResult<Vec<String>> {
        Ok(vec![db.to_owned()])
    }

    async fn list_objects(&mut self, db: &str, _schema: Option<&str>) -> DriverResult<Vec<ObjectInfo>> {
        let tref = |name: String| TableRef { database: Some(db.to_owned()), schema: None, name };
        let mut out = Vec::new();
        for r in self.rows(&format!("SELECT table_name, table_type FROM information_schema.tables WHERE table_schema = {} ORDER BY table_name", lit(db))).await? {
            let kind = if text(&r, 1) == "VIEW" { ObjectKind::View } else { ObjectKind::Table };
            out.push(ObjectInfo { kind, table: tref(text(&r, 0)) });
        }
        for r in self.rows(&format!("SELECT routine_name, routine_type FROM information_schema.routines WHERE routine_schema = {} ORDER BY routine_name", lit(db))).await? {
            let kind = if text(&r, 1) == "PROCEDURE" { ObjectKind::Procedure } else { ObjectKind::Function };
            out.push(ObjectInfo { kind, table: tref(text(&r, 0)) });
        }
        Ok(out)
    }

    async fn table_structure(&mut self, t: &TableRef) -> DriverResult<TableStructure> {
        let db = match &t.database {
            Some(d) => d.clone(),
            None => self.current_db().await?,
        };
        let (sdb, stable) = (lit(&db), lit(&t.name));
        let mut st = TableStructure { table: TableRef { database: Some(db.clone()), schema: None, name: t.name.clone() }, ..Default::default() };
        let cols = self
            .rows(&format!(
                "SELECT column_name, column_type, is_nullable, column_default, column_key, extra FROM information_schema.columns \
                 WHERE table_schema = {sdb} AND table_name = {stable} ORDER BY ordinal_position"
            ))
            .await?;
        for (i, r) in cols.iter().enumerate() {
            let extra = text(r, 5);
            let mut meta = ColumnMeta {
                name: text(r, 0),
                type_name: text(r, 1),
                nullable: text(r, 2) == "YES",
                default: normalize_default(opt_text(r, 3), &extra),
                is_pk: text(r, 4) == "PRI",
                ordinal: i,
                ..Default::default()
            };
            if !extra.is_empty() {
                meta.extra.insert("extra".into(), extra);
            }
            st.columns.push(meta);
        }
        let pk = self
            .rows(&format!(
                "SELECT column_name FROM information_schema.key_column_usage WHERE constraint_name = 'PRIMARY' AND table_schema = {sdb} AND table_name = {stable} ORDER BY ordinal_position"
            ))
            .await?;
        st.primary_key = pk.iter().map(|r| text(r, 0)).collect();
        let fks = self
            .rows(&format!(
                "SELECT k.column_name, k.referenced_table_schema, k.referenced_table_name, k.referenced_column_name, r.update_rule, r.delete_rule, k.constraint_name \
                 FROM information_schema.key_column_usage k JOIN information_schema.referential_constraints r \
                 ON r.constraint_schema = k.constraint_schema AND r.constraint_name = k.constraint_name AND r.table_name = k.table_name \
                 WHERE k.table_schema = {sdb} AND k.table_name = {stable} AND k.referenced_table_name IS NOT NULL ORDER BY k.ordinal_position"
            ))
            .await?;
        for r in &fks {
            let col = text(r, 0);
            if let Some(c) = st.columns.iter_mut().find(|c| c.name == col) {
                c.fk = Some(ForeignKeyRef {
                    table: TableRef { database: Some(text(r, 1)), schema: None, name: text(r, 2) },
                    columns: vec![text(r, 3)],
                    on_update: text(r, 4),
                    on_delete: text(r, 5),
                    name: opt_text(r, 6),
                });
            }
        }
        let ixs = self
            .rows(&format!(
                "SELECT index_name, non_unique, column_name FROM information_schema.statistics \
                 WHERE table_schema = {sdb} AND table_name = {stable} AND index_name <> 'PRIMARY' ORDER BY index_name, seq_in_index"
            ))
            .await?;
        for r in &ixs {
            let name = text(r, 0);
            let col = opt_text(r, 2).unwrap_or_else(|| "<expression>".into());
            match st.indexes.last_mut() {
                Some(last) if last.name == name => last.columns.push(col),
                _ => st.indexes.push(IndexInfo { name, unique: text(r, 1) == "0", columns: vec![col], condition: None }),
            }
        }
        let trs = self
            .rows(&format!(
                "SELECT trigger_name, action_timing, event_manipulation, action_statement FROM information_schema.triggers \
                 WHERE event_object_schema = {sdb} AND event_object_table = {stable} ORDER BY trigger_name"
            ))
            .await?;
        st.triggers = trs.iter().map(|r| TriggerInfo { name: text(r, 0), timing: text(r, 1), event: text(r, 2), statement: text(r, 3) }).collect();
        let kind = self.rows(&format!("SELECT table_type FROM information_schema.tables WHERE table_schema = {sdb} AND table_name = {stable}")).await?;
        let is_view = kind.first().is_some_and(|r| text(r, 0) == "VIEW");
        let show = format!("SHOW CREATE {} {}.{}", if is_view { "VIEW" } else { "TABLE" }, q(&db), q(&t.name));
        st.ddl = self.rows(&show).await?.first().map(|r| text(r, 1)).unwrap_or_default();
        Ok(st)
    }

    async fn query_stream(&mut self, sql: &str, opts: QueryOpts, tx: RowSink) -> DriverResult<QuerySummary> {
        let start = Instant::now();
        let cancel = MyCancel { cfg: self.cfg.clone(), ssl: self.ssl, id: self.conn.id() };
        let mut result = self.conn.query_iter(sql).await.map_err(map_err)?;
        let columns = result.columns();
        let Some(columns) = columns.filter(|c| !c.is_empty()) else {
            let affected = result.affected_rows();
            let warnings = result.warnings();
            result.drop_result().await.map_err(map_err)?;
            let messages = if warnings > 0 { vec![format!("{warnings} warning(s)")] } else { vec![] };
            return Ok(QuerySummary { affected: Some(affected), elapsed: start.elapsed(), messages });
        };
        let metas: Vec<ColumnMeta> = columns.iter().enumerate().map(|(i, c)| ColumnMeta { name: c.name_str().into_owned(), type_name: type_name(c), nullable: true, ordinal: i, ..Default::default() }).collect();
        let summary = |elapsed| QuerySummary { affected: None, elapsed, messages: vec![] };
        if tx.send(RowBatch { columns: Some(metas), rows: vec![] }).await.is_err() {
            result.drop_result().await.map_err(map_err)?;
            return Ok(summary(start.elapsed()));
        }
        let batch_size = opts.batch_size.max(1);
        let mut batch = Vec::with_capacity(batch_size);
        let (mut total, mut skip, mut killed) = (0u64, false, false);
        loop {
            let next = match result.next().await {
                Ok(n) => n,
                Err(e) if killed && is_interrupted(&e) => break,
                Err(e) => return Err(map_err(e)),
            };
            let Some(row) = next else { break };
            if skip {
                continue;
            }
            batch.push((0..columns.len()).map(|i| row.as_ref(i).map_or(Value::Null, |v| to_value(v, &columns[i]))).collect());
            total += 1;
            if batch.len() >= batch_size && tx.send(RowBatch { columns: None, rows: std::mem::take(&mut batch) }).await.is_err() {
                skip = true;
                killed = true;
                let _ = cancel.cancel().await;
            }
            skip = skip || opts.limit.is_some_and(|l| total >= l);
        }
        if !batch.is_empty() && !killed {
            let _ = tx.send(RowBatch { columns: None, rows: batch }).await;
        }
        Ok(summary(start.elapsed()))
    }

    async fn execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>> {
        if atomic {
            self.conn.query_drop("START TRANSACTION").await.map_err(map_err)?;
        }
        let mut counts = Vec::with_capacity(stmts.len());
        for (i, s) in stmts.iter().enumerate() {
            let run = async {
                let r = self.conn.query_iter(s.as_str()).await?;
                let n = r.affected_rows();
                r.drop_result().await?;
                Ok::<u64, mysql_async::Error>(n)
            };
            match run.await {
                Ok(n) => counts.push(n),
                Err(e) => {
                    if atomic {
                        let _ = self.conn.query_drop("ROLLBACK").await;
                    }
                    let msg = match map_err(e) {
                        DriverError::Query(m) => m,
                        other => other.to_string(),
                    };
                    return Err(DriverError::Query(format!("statement {} failed: {msg}", i + 1)));
                }
            }
        }
        if atomic {
            self.conn.query_drop("COMMIT").await.map_err(map_err)?;
        }
        Ok(counts)
    }

    async fn estimate_count(&mut self, t: &TableRef) -> DriverResult<Option<u64>> {
        let db = match &t.database {
            Some(d) => d.clone(),
            None => self.current_db().await?,
        };
        let rows = self.rows(&format!("SELECT table_rows FROM information_schema.tables WHERE table_schema = {} AND table_name = {}", lit(&db), lit(&t.name))).await?;
        Ok(rows.first().and_then(|r| opt_text(r, 0)).and_then(|s| s.parse().ok()))
    }

    async fn cancel_handle(&mut self) -> DriverResult<Box<dyn CancelToken>> {
        Ok(Box::new(MyCancel { cfg: self.cfg.clone(), ssl: self.ssl, id: self.conn.id() }))
    }

    async fn ping(&mut self) -> DriverResult<()> {
        self.conn.ping().await.map_err(map_err)
    }

    async fn set_database(&mut self, db: &str) -> DriverResult<()> {
        self.conn.query_drop(format!("USE {}", q(db))).await.map_err(map_err)?;
        self.cfg.database = db.to_owned();
        Ok(())
    }

    async fn create_database(&mut self, req: &NewDatabase) -> DriverResult<()> {
        let mut sql = format!("CREATE DATABASE {}", q(&req.name));
        if let Some(e) = &req.encoding {
            sql.push_str(&format!(" CHARACTER SET {}", lit(e)));
        }
        if let Some(c) = &req.collation {
            sql.push_str(&format!(" COLLATE {}", lit(c)));
        }
        self.conn.query_drop(sql).await.map_err(map_err)
    }

    async fn rename_database(&mut self, _from: &str, _to: &str) -> DriverResult<()> {
        Err(DriverError::Unsupported("MySQL and MariaDB cannot rename a database".into()))
    }

    async fn drop_database(&mut self, name: &str) -> DriverResult<()> {
        self.conn.query_drop(format!("DROP DATABASE {}", q(name))).await.map_err(map_err)
    }
}
