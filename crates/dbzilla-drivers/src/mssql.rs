//! SQL Server driver: `tiberius` (TDS over TCP, TLS through rustls).

use crate::traits::*;
use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef, TableStructure};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Row};
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

/// Builds the tiberius settings. Encryption follows the TLS mode: PREFERRED and REQUIRED encrypt without
/// checking the certificate (servers usually have a self-signed one), the VERIFY modes check it, and
/// DISABLED encrypts only the login, as the server insists.
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

/// Opens the socket and logs in.
async fn connect_client(cfg: &ConnectConfig, database: &str, ssl: SslMode) -> DriverResult<MsClient> {
    let config = build_config(cfg, database, ssl);
    let addr = config.get_addr();
    let attempt = async {
        let tcp = TcpStream::connect(&addr).await.map_err(|e| format!("cannot reach {addr}: {e}"))?;
        tcp.set_nodelay(true).map_err(|e| e.to_string())?;
        Client::connect(config, tcp.compat_write()).await.map_err(|e| error_text(&e))
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(message)) => Err(DriverError::Connect(message)),
        Err(_) => Err(DriverError::Connect(format!("timed out connecting to {addr}"))),
    }
}

/// Opens a client; PREFERRED falls back to no encryption, as the other drivers do.
async fn open_client(cfg: &ConnectConfig, database: &str) -> DriverResult<(MsClient, SslMode)> {
    match connect_client(cfg, database, cfg.ssl).await {
        Ok(c) => Ok((c, cfg.ssl)),
        Err(_) if cfg.ssl == SslMode::Preferred => Ok((connect_client(cfg, database, SslMode::Disabled).await?, SslMode::Disabled)),
        Err(e) => Err(e),
    }
}

/// Session setup: reliable row counts, the bootstrap commands, then the session id and current database.
async fn init_session(client: &mut MsClient, cfg: &ConnectConfig) -> DriverResult<(i32, String)> {
    let setup = std::iter::once("SET NOCOUNT OFF").chain(cfg.bootstrap.iter().map(String::as_str));
    for stmt in setup {
        let outcome = match client.simple_query(stmt).await {
            Ok(stream) => stream.into_results().await.map(|_| ()),
            Err(e) => Err(e),
        };
        outcome.map_err(|e| DriverError::Connect(format!("bootstrap command failed: {}", error_text(&e))))?;
    }
    let row = client.simple_query("SELECT CAST(@@SPID AS int), DB_NAME()").await.map_err(map_err)?.into_row().await.map_err(map_err)?;
    let row = row.ok_or_else(|| DriverError::Other("the server did not return its session id".into()))?;
    Ok((row.get::<i32, _>(0).unwrap_or_default(), text(&row, 1)))
}

/// An open connection. `client` is `None` after the socket died or the session was killed; the next call reconnects.
struct MsConn {
    client: Option<MsClient>,
    cfg: ConnectConfig,
    /// TLS mode that actually worked.
    ssl: SslMode,
    /// Database the session works in; restored after a reconnect.
    current_db: String,
    /// Server session id, used to cancel from a second connection.
    spid: i32,
}

impl MsConn {
    /// Connects and prepares the session.
    async fn open(cfg: ConnectConfig) -> DriverResult<Self> {
        let (mut client, ssl) = open_client(&cfg, &cfg.database).await?;
        let (spid, current_db) = init_session(&mut client, &cfg).await?;
        Ok(Self { client: Some(client), cfg, ssl, current_db, spid })
    }

    /// The live client, reconnecting (and returning to `current_db`) when it is gone.
    async fn ensure_client(&mut self) -> DriverResult<&mut MsClient> {
        if self.client.is_none() {
            let mut client = connect_client(&self.cfg, &self.current_db, self.ssl).await?;
            let (spid, _) = init_session(&mut client, &self.cfg).await?;
            self.spid = spid;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("client was just set"))
    }

    /// Turns a tiberius error into a driver error, dropping the client when the socket is gone.
    fn fail(&mut self, e: tiberius::error::Error) -> DriverError {
        if is_broken(&e) {
            self.client = None;
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

fn unsupported<T>(what: &str) -> DriverResult<T> {
    Err(DriverError::Unsupported(format!("{what} is not implemented for SQL Server yet")))
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

    async fn table_structure(&mut self, _t: &TableRef) -> DriverResult<TableStructure> {
        unsupported("table structure")
    }

    async fn query_stream(&mut self, _sql: &str, _opts: QueryOpts, _tx: RowSink) -> DriverResult<QuerySummary> {
        unsupported("running queries")
    }

    async fn execute_batch(&mut self, _stmts: &[String], _atomic: bool) -> DriverResult<Vec<u64>> {
        unsupported("batch execution")
    }

    async fn estimate_count(&mut self, _t: &TableRef) -> DriverResult<Option<u64>> {
        unsupported("row estimates")
    }

    async fn cancel_handle(&mut self) -> DriverResult<Box<dyn CancelToken>> {
        unsupported("cancelling")
    }

    async fn ping(&mut self) -> DriverResult<()> {
        self.run("SELECT 1").await
    }

    async fn set_database(&mut self, db: &str) -> DriverResult<()> {
        self.run(&format!("USE {}", q(db))).await?;
        self.current_db = db.to_owned();
        Ok(())
    }

    async fn create_database(&mut self, _req: &NewDatabase) -> DriverResult<()> {
        unsupported("creating databases")
    }

    async fn rename_database(&mut self, _from: &str, _to: &str) -> DriverResult<()> {
        unsupported("renaming databases")
    }

    async fn drop_database(&mut self, _name: &str) -> DriverResult<()> {
        unsupported("dropping databases")
    }
}
