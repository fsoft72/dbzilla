//! SQL Server driver: `tiberius` (TDS over TCP, TLS through rustls).

use crate::traits::*;
use async_trait::async_trait;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::ddl::create_from_structure;
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, ObjectInfo, ObjectKind, TableRef, TableStructure, TriggerInfo};
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

    /// Database (quoted and plain), schema and the `OBJECT_ID(..)` expression of a table reference. The
    /// schema defaults to `dbo` and the database to the current one.
    fn locate(&self, t: &TableRef) -> (String, String, String, String) {
        let db = t.database.clone().unwrap_or_else(|| self.current_db.clone());
        let schema = t.schema.clone().unwrap_or_else(|| "dbo".to_owned());
        let oid = format!("OBJECT_ID({})", lit(&format!("{}.{}.{}", q(&db), q(&schema), q(&t.name))));
        (q(&db), db, schema, oid)
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

    async fn query_stream(&mut self, _sql: &str, _opts: QueryOpts, _tx: RowSink) -> DriverResult<QuerySummary> {
        unsupported("running queries")
    }

    async fn execute_batch(&mut self, _stmts: &[String], _atomic: bool) -> DriverResult<Vec<u64>> {
        unsupported("batch execution")
    }

    async fn estimate_count(&mut self, t: &TableRef) -> DriverResult<Option<u64>> {
        let (qdb, _, _, oid) = self.locate(t);
        let rows = self.rows(&format!("SELECT CAST(SUM(p.rows) AS bigint) FROM {qdb}.sys.partitions p WHERE p.object_id = {oid} AND p.index_id IN (0, 1)")).await?;
        Ok(rows.first().and_then(|r| r.get::<i64, _>(0)).and_then(|n| u64::try_from(n).ok()))
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
    fn default_loses_the_parentheses_sql_server_adds() {
        assert_eq!(strip_default_parens("(N'x')"), "N'x'");
        assert_eq!(strip_default_parens("((0))"), "0");
        assert_eq!(strip_default_parens("(getdate())"), "getdate()");
        assert_eq!(strip_default_parens("(1)+(2)"), "(1)+(2)");
        assert_eq!(strip_default_parens("N'x'"), "N'x'");
    }
}
