//! PostgreSQL driver: `tokio-postgres` over rustls.

use crate::pg_value::PgValue;
use crate::traits::*;
use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::ddl::create_from_structure;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, ObjectInfo, ObjectKind, TableRef, TableStructure, TriggerInfo};
use dbzilla_core::value::Value;
use futures_util::{pin_mut, StreamExt, TryStreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode as PgSsl;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::ToSql;
use tokio_postgres::{AsyncMessage, CancelToken as PgCancelToken, Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Default PostgreSQL port.
const DEFAULT_PORT: u16 = 5432;
/// Application name shown in `pg_stat_activity`.
const APP_NAME: &str = "DBZilla";
/// Timeout for establishing a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Schema used when a table reference has none.
const DEFAULT_SCHEMA: &str = "public";

/// The PostgreSQL driver.
pub struct PostgresDriver {
    caps: Capabilities,
}

impl PostgresDriver {
    /// Creates the driver.
    pub fn new() -> Self {
        Self { caps: Capabilities::for_driver(DriverId::Postgres) }
    }
}

impl Default for PostgresDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for PostgresDriver {
    fn id(&self) -> DriverId {
        DriverId::Postgres
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn dialect(&self) -> Box<dyn Dialect> {
        dialect_for(DriverId::Postgres)
    }

    async fn connect(&self, cfg: &ConnectConfig) -> DriverResult<Box<dyn Conn>> {
        Ok(Box::new(PgConn::open(cfg.clone()).await?))
    }
}

/// Accepts any server certificate (PREFERRED / REQUIRED encrypt without verifying, like libpq).
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(&self, _end: &CertificateDer<'_>, _inter: &[CertificateDer<'_>], _name: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Builds the TLS connector for a mode. VERIFY-CA and VERIFY-FULL both verify chain and host name.
fn tls_for(mode: SslMode) -> DriverResult<MakeRustlsConnect> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| DriverError::Connect(format!("TLS setup failed: {e}")))?;
    let config = if matches!(mode, SslMode::VerifyCa | SslMode::VerifyFull) {
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(cert);
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder.dangerous().with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider))).with_no_client_auth()
    };
    Ok(MakeRustlsConnect::new(config))
}

/// Human readable text of a driver error, including server detail and hint.
fn pg_message(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        let mut s = db.message().to_owned();
        if let Some(d) = db.detail() {
            s.push_str(&format!("\nDETAIL: {d}"));
        }
        if let Some(h) = db.hint() {
            s.push_str(&format!("\nHINT: {h}"));
        }
        return s;
    }
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(inner) = src {
        s.push_str(&format!(": {inner}"));
        src = inner.source();
    }
    s
}

/// Maps a query error, recognising cancellation.
fn map_query_err(e: tokio_postgres::Error) -> DriverError {
    if e.code() == Some(&SqlState::QUERY_CANCELED) {
        return DriverError::Cancelled;
    }
    DriverError::Query(pg_message(&e))
}

/// An open PostgreSQL connection.
struct PgConn {
    client: Client,
    cancel: PgCancelToken,
    tls: MakeRustlsConnect,
    cfg: ConnectConfig,
    notices: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for PgConn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PgConn {
    /// Connects, starts the connection task and runs the bootstrap commands.
    async fn open(cfg: ConnectConfig) -> DriverResult<Self> {
        let mut pg = Config::new();
        pg.host(if cfg.host.is_empty() { "localhost" } else { &cfg.host });
        pg.port(cfg.port.unwrap_or(DEFAULT_PORT));
        pg.user(if cfg.user.is_empty() { "postgres" } else { &cfg.user });
        if let Some(pw) = &cfg.password {
            pg.password(pw);
        }
        if !cfg.database.is_empty() {
            pg.dbname(&cfg.database);
        }
        pg.application_name(APP_NAME);
        pg.connect_timeout(CONNECT_TIMEOUT);
        pg.ssl_mode(match cfg.ssl {
            SslMode::Disabled => PgSsl::Disable,
            SslMode::Preferred => PgSsl::Prefer,
            _ => PgSsl::Require,
        });
        let tls = tls_for(cfg.ssl)?;
        let (client, mut connection) = pg.connect(tls.clone()).await.map_err(|e| DriverError::Connect(pg_message(&e)))?;
        let notices = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        let task = tokio::spawn(async move {
            let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            while let Some(Ok(msg)) = messages.next().await {
                if let AsyncMessage::Notice(n) = msg {
                    if let Ok(mut list) = sink.lock() {
                        list.push(format!("{}: {}", n.severity(), n.message()));
                    }
                }
            }
        });
        let conn = Self { cancel: client.cancel_token(), client, tls, cfg: cfg.clone(), notices, task };
        for stmt in &cfg.bootstrap {
            conn.client.batch_execute(stmt).await.map_err(|e| DriverError::Connect(format!("bootstrap command failed: {}", pg_message(&e))))?;
        }
        Ok(conn)
    }

    /// Takes the notices collected so far.
    fn take_notices(&self) -> Vec<String> {
        self.notices.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
    }

    /// `"schema"."table"` text usable as a `regclass` input.
    fn regclass(t: &TableRef) -> String {
        let d = dialect_for(DriverId::Postgres);
        format!("{}.{}", d.quote_ident(t.schema.as_deref().unwrap_or(DEFAULT_SCHEMA)), d.quote_ident(&t.name))
    }

    /// Runs a simple-protocol statement.
    async fn simple(&self, sql: &str) -> DriverResult<()> {
        self.client.batch_execute(sql).await.map_err(map_query_err)
    }
}

/// Foreign key action letter to SQL text.
fn fk_action(code: &str) -> &'static str {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
}

/// Position-based search for the first keyword of `words` in `upper` at or after `from`.
fn first_keyword<'a>(upper: &str, words: &[&'a str], from: usize) -> Option<(usize, &'a str)> {
    words.iter().filter_map(|w| upper[from..].find(w).map(|p| (p + from, *w))).min_by_key(|(p, _)| *p)
}

#[async_trait]
impl Conn for PgConn {
    async fn server_version(&mut self) -> DriverResult<String> {
        let row = self.client.query_one("SHOW server_version", &[]).await.map_err(map_query_err)?;
        Ok(format!("PostgreSQL {}", row.get::<_, String>(0)))
    }

    async fn list_databases(&mut self) -> DriverResult<Vec<DatabaseInfo>> {
        let rows = self.client.query("SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname", &[]).await.map_err(map_query_err)?;
        Ok(rows.iter().map(|r| DatabaseInfo { name: r.get(0) }).collect())
    }

    async fn list_schemas(&mut self, _db: &str) -> DriverResult<Vec<String>> {
        let sql = "SELECT schema_name FROM information_schema.schemata WHERE schema_name NOT LIKE 'pg\\_%' AND schema_name <> 'information_schema' ORDER BY schema_name";
        let rows = self.client.query(sql, &[]).await.map_err(map_query_err)?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    async fn list_objects(&mut self, _db: &str, schema: Option<&str>) -> DriverResult<Vec<ObjectInfo>> {
        let schema = schema.unwrap_or(DEFAULT_SCHEMA).to_owned();
        let mut out = Vec::new();
        let rel_sql = "SELECT c.relkind::text, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                       WHERE n.nspname = $1 AND c.relkind IN ('r','p','f','v','m','S') ORDER BY c.relname";
        for r in self.client.query(rel_sql, &[&schema]).await.map_err(map_query_err)? {
            let kind = match r.get::<_, String>(0).as_str() {
                "v" => ObjectKind::View,
                "m" => ObjectKind::MaterializedView,
                "S" => ObjectKind::Sequence,
                _ => ObjectKind::Table,
            };
            out.push(ObjectInfo { kind, table: TableRef { database: None, schema: Some(schema.clone()), name: r.get(1) } });
        }
        let fn_sql = "SELECT DISTINCT p.prokind::text, p.proname FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                      WHERE n.nspname = $1 AND p.prokind IN ('f','p') ORDER BY p.proname";
        for r in self.client.query(fn_sql, &[&schema]).await.map_err(map_query_err)? {
            let kind = if r.get::<_, String>(0) == "p" { ObjectKind::Procedure } else { ObjectKind::Function };
            out.push(ObjectInfo { kind, table: TableRef { database: None, schema: Some(schema.clone()), name: r.get(1) } });
        }
        Ok(out)
    }

    async fn table_structure(&mut self, t: &TableRef) -> DriverResult<TableStructure> {
        let reg = Self::regclass(t);
        let schema = t.schema.clone().unwrap_or_else(|| DEFAULT_SCHEMA.to_owned());
        let mut st = TableStructure { table: TableRef { database: None, schema: Some(schema.clone()), name: t.name.clone() }, ..Default::default() };
        let col_sql = "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, pg_get_expr(d.adbin, d.adrelid) \
                       FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                       WHERE a.attrelid = $1::text::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum";
        for (i, r) in self.client.query(col_sql, &[&reg]).await.map_err(map_query_err)?.iter().enumerate() {
            st.columns.push(ColumnMeta { name: r.get(0), type_name: r.get(1), nullable: r.get(2), default: r.get(3), ordinal: i, ..Default::default() });
        }
        let pk_sql = "SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
                      WHERE i.indrelid = $1::text::regclass AND i.indisprimary ORDER BY array_position(i.indkey::int2[], a.attnum)";
        for r in self.client.query(pk_sql, &[&reg]).await.map_err(map_query_err)? {
            let name: String = r.get(0);
            if let Some(c) = st.columns.iter_mut().find(|c| c.name == name) {
                c.is_pk = true;
                c.nullable = false;
            }
            st.primary_key.push(name);
        }
        let fk_sql = "SELECT a.attname, rn.nspname, rc.relname, ra.attname, c.confupdtype::text, c.confdeltype::text FROM pg_constraint c \
                      CROSS JOIN LATERAL unnest(c.conkey, c.confkey) AS k(attnum, refattnum) \
                      JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
                      JOIN pg_class rc ON rc.oid = c.confrelid JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
                      JOIN pg_attribute ra ON ra.attrelid = c.confrelid AND ra.attnum = k.refattnum \
                      WHERE c.conrelid = $1::text::regclass AND c.contype = 'f'";
        for r in self.client.query(fk_sql, &[&reg]).await.map_err(map_query_err)? {
            let col: String = r.get(0);
            let (on_update, on_delete): (String, String) = (r.get(4), r.get(5));
            if let Some(c) = st.columns.iter_mut().find(|c| c.name == col) {
                c.fk = Some(ForeignKeyRef {
                    table: TableRef { database: None, schema: Some(r.get(1)), name: r.get(2) },
                    columns: vec![r.get(3)],
                    on_update: fk_action(&on_update).to_owned(),
                    on_delete: fk_action(&on_delete).to_owned(),
                });
            }
        }
        let ix_sql = "SELECT ic.relname, ix.indisunique, pg_get_expr(ix.indpred, ix.indrelid), \
                      (SELECT array_agg(a.attname::text ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid = ic.oid) \
                      FROM pg_index ix JOIN pg_class ic ON ic.oid = ix.indexrelid \
                      WHERE ix.indrelid = $1::text::regclass AND NOT ix.indisprimary ORDER BY ic.relname";
        for r in self.client.query(ix_sql, &[&reg]).await.map_err(map_query_err)? {
            st.indexes.push(IndexInfo { name: r.get(0), unique: r.get(1), condition: r.get(2), columns: r.get::<_, Vec<String>>(3) });
        }
        let tr_sql = "SELECT t.tgname, pg_get_triggerdef(t.oid) FROM pg_trigger t WHERE t.tgrelid = $1::text::regclass AND NOT t.tgisinternal ORDER BY t.tgname";
        for r in self.client.query(tr_sql, &[&reg]).await.map_err(map_query_err)? {
            let def: String = r.get(1);
            let upper = def.to_ascii_uppercase();
            let (pos, timing) = first_keyword(&upper, &["INSTEAD OF", "BEFORE", "AFTER"], 0).unwrap_or((0, ""));
            let event = first_keyword(&upper, &["INSERT", "UPDATE", "DELETE", "TRUNCATE"], pos).map_or("", |(_, e)| e);
            st.triggers.push(TriggerInfo { name: r.get(0), timing: timing.to_owned(), event: event.to_owned(), statement: def });
        }
        let kind: String = self.client.query_one("SELECT relkind::text FROM pg_class WHERE oid = $1::text::regclass", &[&reg]).await.map_err(map_query_err)?.get(0);
        let d = dialect_for(DriverId::Postgres);
        st.ddl = match kind.as_str() {
            "v" | "m" => {
                let def: String = self.client.query_one("SELECT pg_get_viewdef($1::text::regclass, true)", &[&reg]).await.map_err(map_query_err)?.get(0);
                let head = if kind == "v" { "CREATE OR REPLACE VIEW" } else { "CREATE MATERIALIZED VIEW" };
                format!("{head} {} AS\n{}", d.quote_table(&st.table), def.trim())
            }
            _ => create_from_structure(&st, d.as_ref()),
        };
        Ok(st)
    }

    async fn query_stream(&mut self, sql: &str, opts: QueryOpts, tx: RowSink) -> DriverResult<QuerySummary> {
        let start = Instant::now();
        self.take_notices();
        let stmt = self.client.prepare(sql).await.map_err(map_query_err)?;
        if stmt.columns().is_empty() {
            let n = self.client.execute(&stmt, &[]).await.map_err(map_query_err)?;
            return Ok(QuerySummary { affected: Some(n), elapsed: start.elapsed(), messages: self.take_notices() });
        }
        let cols: Vec<ColumnMeta> = stmt
            .columns()
            .iter()
            .enumerate()
            .map(|(i, c)| ColumnMeta { name: c.name().to_owned(), type_name: c.type_().name().to_owned(), nullable: true, ordinal: i, ..Default::default() })
            .collect();
        let ncols = cols.len();
        if tx.send(RowBatch { columns: Some(cols), rows: vec![] }).await.is_err() {
            return Ok(QuerySummary { affected: None, elapsed: start.elapsed(), messages: vec![] });
        }
        let stream = self.client.query_raw(&stmt, std::iter::empty::<&(dyn ToSql + Sync)>()).await.map_err(map_query_err)?;
        pin_mut!(stream);
        let batch_size = opts.batch_size.max(1);
        let mut batch = Vec::with_capacity(batch_size);
        let mut total = 0u64;
        let mut capped = false;
        let mut receiver_gone = false;
        while let Some(row) = stream.try_next().await.map_err(map_query_err)? {
            if capped || receiver_gone {
                continue;
            }
            let values: Result<Vec<Value>, _> = (0..ncols).map(|i| row.try_get::<usize, PgValue>(i).map(|v| v.0)).collect();
            batch.push(values.map_err(|e| DriverError::Query(format!("cannot decode a value: {e}")))?);
            total += 1;
            if batch.len() >= batch_size && tx.send(RowBatch { columns: None, rows: std::mem::take(&mut batch) }).await.is_err() {
                receiver_gone = true;
                let _ = self.cancel.cancel_query(self.tls.clone()).await;
            }
            capped = opts.limit.is_some_and(|l| total >= l);
        }
        if !batch.is_empty() && !receiver_gone {
            let _ = tx.send(RowBatch { columns: None, rows: batch }).await;
        }
        Ok(QuerySummary { affected: None, elapsed: start.elapsed(), messages: self.take_notices() })
    }

    async fn execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>> {
        if atomic {
            self.simple("BEGIN").await?;
        }
        let mut counts = Vec::with_capacity(stmts.len());
        for (i, s) in stmts.iter().enumerate() {
            match self.client.execute(s.as_str(), &[]).await {
                Ok(n) => counts.push(n),
                Err(e) => {
                    if atomic {
                        let _ = self.simple("ROLLBACK").await;
                    }
                    return Err(DriverError::Query(format!("statement {} failed: {}", i + 1, pg_message(&e))));
                }
            }
        }
        if atomic {
            self.simple("COMMIT").await?;
        }
        Ok(counts)
    }

    async fn estimate_count(&mut self, t: &TableRef) -> DriverResult<Option<u64>> {
        let reg = Self::regclass(t);
        let row = self.client.query_one("SELECT reltuples::bigint FROM pg_class WHERE oid = $1::text::regclass", &[&reg]).await.map_err(map_query_err)?;
        let n: i64 = row.get(0);
        Ok(u64::try_from(n).ok())
    }

    async fn cancel_handle(&self) -> DriverResult<Box<dyn CancelToken>> {
        Ok(Box::new(PgCancel { token: self.client.cancel_token(), tls: self.tls.clone() }))
    }

    async fn ping(&mut self) -> DriverResult<()> {
        self.simple("SELECT 1").await
    }

    async fn set_database(&mut self, db: &str) -> DriverResult<()> {
        let mut cfg = self.cfg.clone();
        cfg.database = db.to_owned();
        *self = PgConn::open(cfg).await?;
        Ok(())
    }

    async fn create_database(&mut self, req: &NewDatabase) -> DriverResult<()> {
        let d = dialect_for(DriverId::Postgres);
        let lit = |s: &str| d.literal(&Value::Text(s.to_owned()));
        let mut sql = format!("CREATE DATABASE {}", d.quote_ident(&req.name));
        if req.encoding.is_some() || req.collation.is_some() {
            sql.push_str(" TEMPLATE template0");
        }
        if let Some(e) = &req.encoding {
            sql.push_str(&format!(" ENCODING {}", lit(e)));
        }
        if let Some(c) = &req.collation {
            sql.push_str(&format!(" LC_COLLATE {} LC_CTYPE {}", lit(c), lit(c)));
        }
        self.simple(&sql).await
    }

    async fn rename_database(&mut self, from: &str, to: &str) -> DriverResult<()> {
        let d = dialect_for(DriverId::Postgres);
        self.simple(&format!("ALTER DATABASE {} RENAME TO {}", d.quote_ident(from), d.quote_ident(to))).await
    }

    async fn drop_database(&mut self, name: &str) -> DriverResult<()> {
        let d = dialect_for(DriverId::Postgres);
        self.simple(&format!("DROP DATABASE {}", d.quote_ident(name))).await
    }
}

/// Cancels through a second connection (`pg_cancel_backend` equivalent at protocol level).
struct PgCancel {
    token: PgCancelToken,
    tls: MakeRustlsConnect,
}

#[async_trait]
impl CancelToken for PgCancel {
    async fn cancel(&self) -> DriverResult<()> {
        self.token.cancel_query(self.tls.clone()).await.map_err(|e| DriverError::Other(format!("cancel failed: {}", pg_message(&e))))
    }
}
