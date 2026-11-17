//! SQL Server driver integration tests against a throwaway Docker container.
//! Switched on with `DBZILLA_TEST_MSSQL=1` (the image is big and slow to start).
#![cfg(feature = "mssql")]

mod support;

use chrono::{NaiveDate, NaiveTime};
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, QueryOpts, SslMode};
use std::time::{Duration, Instant};
use support::{collect, opts, rows_of, Container};
use uuid::Uuid;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio_util::compat::TokioAsyncWriteCompatExt;

const LABEL_MSSQL: &str = "dbzilla-test-mssql";
const IMAGE: &str = "mcr.microsoft.com/mssql/server:2022-latest";
const SA_PASSWORD: &str = "Dbzilla_Pw1!";
static MSSQL: OnceCell<Option<Container>> = OnceCell::const_new();

#[dtor::dtor(unsafe)]
fn cleanup_containers() {
    support::cleanup(LABEL_MSSQL);
}

/// Opens a raw tiberius client with required TLS and the self-signed certificate trusted.
async fn raw_client(port: u16) -> Result<Client<tokio_util::compat::Compat<TcpStream>>, tiberius::error::Error> {
    let mut cfg = Config::new();
    cfg.host("127.0.0.1");
    cfg.port(port);
    cfg.authentication(AuthMethod::sql_server("sa", SA_PASSWORD));
    cfg.encryption(EncryptionLevel::Required);
    cfg.trust_cert();
    let tcp = TcpStream::connect(cfg.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Client::connect(cfg, tcp.compat_write()).await
}

/// Starts the shared container and waits until a login works. None when the tests are off.
async fn container() -> Option<&'static Container> {
    if !support::mssql_enabled() {
        return None;
    }
    MSSQL
        .get_or_init(|| async {
            let c = support::start(LABEL_MSSQL, IMAGE, &[("ACCEPT_EULA", "Y"), ("MSSQL_SA_PASSWORD", SA_PASSWORD)], 1433, &[])?;
            for _ in 0..240 {
                if raw_client(c.port).await.is_ok() {
                    return Some(c);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            panic!("SQL Server did not become ready");
        })
        .await
        .as_ref()
}

#[tokio::test]
async fn tls_spike_connects() {
    let Some(c) = container().await else { return };
    let mut client = raw_client(c.port).await.expect("login over required TLS");
    let row = client.simple_query("SELECT 1").await.unwrap().into_row().await.unwrap().unwrap();
    assert_eq!(row.get::<i32, _>(0), Some(1));
}

/// Connection settings for the shared container.
fn cfg(port: u16, db: &str) -> ConnectConfig {
    ConnectConfig {
        driver: DriverId::SqlServer,
        host: "127.0.0.1".into(),
        port: Some(port),
        user: "sa".into(),
        password: Some(SA_PASSWORD.into()),
        database: db.into(),
        ssl: SslMode::Required,
        bootstrap: vec![],
    }
}

/// Opens a connection through the driver.
async fn connect(port: u16, db: &str) -> Box<dyn Conn> {
    driver_for(DriverId::SqlServer).unwrap().connect(&cfg(port, db)).await.expect("connect")
}

/// Runs fixture statements one by one on a raw client (`CREATE FUNCTION` and friends need their own batch).
async fn raw_exec(port: u16, statements: &[&str]) {
    let mut client = raw_client(port).await.expect("raw login");
    for sql in statements {
        client.simple_query(*sql).await.unwrap_or_else(|e| panic!("{sql}: {e}")).into_results().await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

#[tokio::test]
async fn connects_and_reports_version() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    assert!(conn.server_version().await.unwrap().contains("Microsoft SQL Server"));
}

#[tokio::test]
async fn wrong_password_is_a_connect_error() {
    let Some(c) = container().await else { return };
    let mut bad = cfg(c.port, "");
    bad.password = Some("wrong".into());
    let err = driver_for(DriverId::SqlServer).unwrap().connect(&bad).await.err().expect("login must fail");
    assert!(matches!(err, DriverError::Connect(ref m) if !m.is_empty()), "{err:?}");
}

#[tokio::test]
async fn unreachable_host_is_a_connect_error() {
    if !support::mssql_enabled() {
        return;
    }
    let mut bad = cfg(1, "");
    bad.host = "127.0.0.1".into();
    let err = driver_for(DriverId::SqlServer).unwrap().connect(&bad).await.err().expect("connect must fail");
    assert!(matches!(err, DriverError::Connect(_)), "{err:?}");
}

#[tokio::test]
async fn lists_databases_schemas_and_objects() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t4_browse') IS NULL CREATE DATABASE t4_browse"]).await;
    raw_exec(
        c.port,
        &[
            "USE t4_browse; IF SCHEMA_ID('app') IS NULL EXEC('CREATE SCHEMA app')",
            "USE t4_browse; IF OBJECT_ID('app.items') IS NULL CREATE TABLE app.items (id int PRIMARY KEY, name nvarchar(50))",
            "USE t4_browse; IF OBJECT_ID('app.items_v') IS NULL EXEC('CREATE VIEW app.items_v AS SELECT id FROM app.items')",
            "USE t4_browse; IF OBJECT_ID('app.one') IS NULL EXEC('CREATE FUNCTION app.one() RETURNS int AS BEGIN RETURN 1 END')",
            "USE t4_browse; IF OBJECT_ID('app.do_nothing') IS NULL EXEC('CREATE PROCEDURE app.do_nothing AS SELECT 1')",
        ],
    )
    .await;
    let mut conn = connect(c.port, "").await;
    let dbs: Vec<String> = conn.list_databases().await.unwrap().into_iter().map(|d| d.name).collect();
    assert!(dbs.contains(&"t4_browse".to_owned()) && dbs.contains(&"master".to_owned()), "{dbs:?}");
    let schemas = conn.list_schemas("t4_browse").await.unwrap();
    assert!(schemas.contains(&"app".to_owned()) && schemas.contains(&"dbo".to_owned()), "{schemas:?}");
    assert!(!schemas.contains(&"sys".to_owned()) && !schemas.contains(&"INFORMATION_SCHEMA".to_owned()), "{schemas:?}");
    let objects = conn.list_objects("t4_browse", Some("app")).await.unwrap();
    let find = |name: &str| objects.iter().find(|o| o.table.name == name).map(|o| (o.kind, o.table.clone()));
    let table = TableRef { database: Some("t4_browse".into()), schema: Some("app".into()), name: "items".into() };
    assert_eq!(find("items"), Some((ObjectKind::Table, table)));
    assert_eq!(find("items_v").map(|f| f.0), Some(ObjectKind::View));
    assert_eq!(find("one").map(|f| f.0), Some(ObjectKind::Function));
    assert_eq!(find("do_nothing").map(|f| f.0), Some(ObjectKind::Procedure));
    let dbo = conn.list_objects("t4_browse", Some("dbo")).await.unwrap();
    assert!(dbo.iter().all(|o| o.table.name != "items"), "objects of another schema leaked: {dbo:?}");
}

#[tokio::test]
async fn ping_and_set_database() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t4_ping') IS NULL CREATE DATABASE t4_ping"]).await;
    let mut conn = connect(c.port, "").await;
    conn.ping().await.unwrap();
    conn.set_database("t4_ping").await.unwrap();
    conn.ping().await.unwrap();
    let mut other = connect(c.port, "t4_ping").await;
    assert!(other.list_schemas("t4_ping").await.unwrap().contains(&"dbo".to_owned()));
}

static STRUCTURE: OnceCell<()> = OnceCell::const_new();

/// Creates the database and tables of the structure tests once, even when tests run in parallel.
async fn structure_fixture(port: u16) {
    STRUCTURE.get_or_init(|| create_structure_fixture(port)).await;
}

async fn create_structure_fixture(port: u16) {
    raw_exec(port, &["IF DB_ID('t5_struct') IS NULL CREATE DATABASE t5_struct"]).await;
    raw_exec(
        port,
        &[
            "USE t5_struct; IF OBJECT_ID('dbo.parent') IS NULL CREATE TABLE dbo.parent (id int PRIMARY KEY)",
            "USE t5_struct; IF OBJECT_ID('dbo.child') IS NULL CREATE TABLE dbo.child (
                id int IDENTITY(1,1) NOT NULL,
                parent_id int NOT NULL,
                name nvarchar(50) NULL CONSTRAINT df_child_name DEFAULT (N'x'),
                price decimal(10,2) NOT NULL,
                code varchar(20) NULL,
                CONSTRAINT pk_child PRIMARY KEY (id),
                CONSTRAINT fk_child_parent FOREIGN KEY (parent_id) REFERENCES dbo.parent (id) ON DELETE CASCADE,
                CONSTRAINT uq_child UNIQUE (parent_id, code))",
            "USE t5_struct; IF OBJECT_ID('dbo.trg_child') IS NULL EXEC('CREATE TRIGGER dbo.trg_child ON dbo.child AFTER INSERT, UPDATE AS SELECT 1')",
            "USE t5_struct; IF OBJECT_ID('dbo.child_v') IS NULL EXEC('CREATE VIEW dbo.child_v AS SELECT id, name FROM dbo.child')",
            "USE t5_struct; IF NOT EXISTS (SELECT 1 FROM dbo.parent) INSERT INTO dbo.parent VALUES (1),(2),(3),(4),(5)",
        ],
    )
    .await;
}

#[tokio::test]
async fn table_structure_reads_columns_keys_indexes_triggers() {
    let Some(c) = container().await else { return };
    structure_fixture(c.port).await;
    let mut conn = connect(c.port, "").await;
    let st = conn.table_structure(&TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "child".into() }).await.unwrap();
    assert_eq!(st.table, TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "child".into() });
    let col = |n: &str| st.columns.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no column {n}"));
    assert_eq!(st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "parent_id", "name", "price", "code"]);
    assert_eq!((col("id").type_name.as_str(), col("id").nullable, col("id").is_pk), ("int", false, true));
    assert_eq!(col("id").extra.get("identity").map(String::as_str), Some("true"));
    assert_eq!((col("name").type_name.as_str(), col("name").nullable, col("name").default.as_deref()), ("nvarchar(50)", true, Some("N'x'")));
    assert_eq!(col("price").type_name, "decimal(10,2)");
    assert_eq!(col("code").type_name, "varchar(20)");
    assert_eq!(st.primary_key, ["id"]);
    let fk = col("parent_id").fk.clone().expect("foreign key");
    assert_eq!(fk.table, TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "parent".into() });
    assert_eq!((fk.columns.as_slice(), fk.on_delete.as_str(), fk.on_update.as_str(), fk.name.as_deref()), (["id".to_owned()].as_slice(), "CASCADE", "NO ACTION", Some("fk_child_parent")));
    let uq = st.indexes.iter().find(|i| i.name == "uq_child").expect("unique index");
    assert!(uq.unique);
    assert_eq!(uq.columns, ["parent_id", "code"]);
    assert!(st.indexes.iter().any(|i| i.name == "pk_child" && i.unique));
    let tr = st.triggers.iter().find(|t| t.name == "trg_child").expect("trigger");
    assert_eq!((tr.timing.as_str(), tr.event.as_str()), ("AFTER", "INSERT, UPDATE"));
    assert!(tr.statement.contains("CREATE TRIGGER"));
    assert!(st.ddl.starts_with("CREATE TABLE [t5_struct].[dbo].[child]"), "{}", st.ddl);
}

#[tokio::test]
async fn table_structure_of_a_view_carries_its_definition() {
    let Some(c) = container().await else { return };
    structure_fixture(c.port).await;
    let mut conn = connect(c.port, "").await;
    let st = conn.table_structure(&TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "child_v".into() }).await.unwrap();
    assert_eq!(st.columns.len(), 2);
    assert!(st.ddl.contains("CREATE VIEW"), "{}", st.ddl);
}

#[tokio::test]
async fn estimate_count_uses_partition_rows() {
    let Some(c) = container().await else { return };
    structure_fixture(c.port).await;
    let mut conn = connect(c.port, "").await;
    let parent = TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "parent".into() };
    assert_eq!(conn.estimate_count(&parent).await.unwrap(), Some(5));
    let missing = TableRef { database: Some("t5_struct".into()), schema: Some("dbo".into()), name: "nope".into() };
    assert_eq!(conn.estimate_count(&missing).await.unwrap(), None);
}

#[tokio::test]
async fn maps_types() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let sql = "SELECT CAST(NULL AS int), CAST(1 AS bit), CAST(12.3456789012 AS decimal(38,10)), CAST('2020-01-02T03:04:05.123' AS datetime2), \
               CAST('2020-01-02T03:04:05.1234567+02:00' AS datetimeoffset), CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier), 0xDEADBEEF, \
               N'ciao àèì 日本', CAST(1.5 AS float), CAST('<a/>' AS xml), CAST(12.5 AS money), CAST('2020-01-02' AS date), CAST('03:04:05' AS time), \
               CAST(7 AS bigint), CAST(7 AS tinyint), CAST(1.5 AS real), CAST(7 AS smallint)";
    let (batches, summary) = collect(&mut conn, sql, opts()).await;
    let row = rows_of(&batches).remove(0);
    let day = NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
    let expected = [
        Value::Null,
        Value::Bool(true),
        Value::Decimal("12.3456789012".into()),
        Value::DateTime(day.and_hms_milli_opt(3, 4, 5, 123).unwrap()),
        Value::DateTimeTz(chrono::DateTime::parse_from_rfc3339("2020-01-02T03:04:05.1234567+02:00").unwrap()),
        Value::Uuid(Uuid::parse_str("6F9619FF-8B86-D011-B42D-00C04FC964FF").unwrap()),
        Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        Value::Text("ciao àèì 日本".into()),
        Value::F64(1.5),
        Value::Text("<a/>".into()),
        Value::Decimal("12.5000".into()),
        Value::Date(day),
        Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap()),
        Value::I64(7),
        Value::I64(7),
        Value::F64(1.5),
        Value::I64(7),
    ];
    assert_eq!(row, expected);
    assert!(batches[0].columns.as_ref().is_some_and(|c| c.len() == expected.len()));
    assert_eq!(summary.affected, None);
}

#[tokio::test]
async fn streams_in_batches_with_limit() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let sql = "SELECT TOP 1000 ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n FROM sys.all_objects a CROSS JOIN sys.all_objects b";
    let (batches, _) = collect(&mut conn, sql, QueryOpts { limit: Some(450), ..opts() }).await;
    assert!(batches[0].columns.as_ref().is_some_and(|c| c[0].name == "n"));
    assert!(batches[0].rows.is_empty());
    assert!(batches.iter().all(|b| b.rows.len() <= 200));
    assert_eq!(rows_of(&batches).len(), 450);
    let (all, _) = collect(&mut conn, sql, opts()).await;
    assert_eq!(rows_of(&all).len(), 1000);
}

#[tokio::test]
async fn non_row_statement_reports_affected_rows() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t6_dml') IS NULL CREATE DATABASE t6_dml"]).await;
    raw_exec(c.port, &["USE t6_dml; IF OBJECT_ID('dbo.t') IS NOT NULL DROP TABLE dbo.t", "USE t6_dml; CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v int)"]).await;
    let mut conn = connect(c.port, "t6_dml").await;
    let (_, inserted) = collect(&mut conn, "INSERT INTO dbo.t (v) VALUES (1), (2), (3)", opts()).await;
    assert_eq!(inserted.affected, Some(3));
    let (batches, updated) = collect(&mut conn, "UPDATE dbo.t SET v = v + 1", opts()).await;
    assert_eq!((updated.affected, rows_of(&batches).len()), (Some(3), 0));
    let (_, deleted) = collect(&mut conn, "DELETE FROM dbo.t WHERE v > 2", opts()).await;
    assert_eq!(deleted.affected, Some(2));
    let (batches, _) = collect(&mut conn, "INSERT INTO dbo.t (v) OUTPUT inserted.v VALUES (9)", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::I64(9)]]);
}

#[tokio::test]
async fn a_failing_query_is_a_query_error_and_the_connection_survives() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let err = conn.query_stream("SELECT * FROM no_such_table_here", opts(), tx).await.expect_err("must fail");
    assert!(matches!(&err, DriverError::Query(m) if m.contains("Invalid object name")), "{err:?}");
    while rx.recv().await.is_some() {}
    let (batches, _) = collect(&mut conn, "SELECT 1", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::I64(1)]]);
}

#[tokio::test]
async fn timeout_fails_and_the_connection_recovers() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let started = Instant::now();
    let err = conn.query_stream("WAITFOR DELAY '00:00:20'", QueryOpts { timeout: Duration::from_secs(1), ..opts() }, tx).await.expect_err("must time out");
    assert!(matches!(&err, DriverError::Query(m) if m.contains("timed out")), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    conn.ping().await.unwrap();
}
