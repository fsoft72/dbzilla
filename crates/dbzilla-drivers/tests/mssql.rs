//! SQL Server driver integration tests against a throwaway Docker container.
//! Switched on with `DBZILLA_TEST_MSSQL=1` (the image is big and slow to start).
#![cfg(feature = "mssql")]

mod support;

use chrono::{NaiveDate, NaiveTime};
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_core::value::Value;
use dbzilla_core::dialect::dialect_for;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, NewDatabase, QueryOpts, SslMode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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

/// Recreates `dbo.<table>` in the `t7_batch` database and returns a connection working in it. Every test has its
/// own table, so they can run in parallel.
async fn batch_fixture(port: u16, table: &str) -> Box<dyn Conn> {
    raw_exec(port, &["IF DB_ID('t7_batch') IS NULL CREATE DATABASE t7_batch"]).await;
    raw_exec(
        port,
        &[
            &format!("USE t7_batch; IF OBJECT_ID('dbo.{table}') IS NOT NULL DROP TABLE dbo.{table}"),
            &format!("USE t7_batch; CREATE TABLE dbo.{table} (a int NOT NULL, s nvarchar(100) NULL)"),
        ],
    )
    .await;
    connect(port, "t7_batch").await
}

async fn count_rows(conn: &mut Box<dyn Conn>, table: &str) -> Value {
    let (batches, _) = collect(conn, &format!("SELECT COUNT(*) FROM dbo.{table}"), opts()).await;
    rows_of(&batches).remove(0).remove(0)
}

#[tokio::test]
async fn batch_returns_affected_counts() {
    let Some(c) = container().await else { return };
    let mut conn = batch_fixture(c.port, "t7_counts").await;
    let stmts = ["INSERT INTO dbo.t7_counts (a) VALUES (1), (2)", "UPDATE dbo.t7_counts SET a = a + 1", "CREATE INDEX ix7 ON dbo.t7_counts (a)", "DELETE FROM dbo.t7_counts"].map(String::from);
    assert_eq!(conn.execute_batch(&stmts, false).await.unwrap(), vec![2, 2, 0, 2]);
}

#[tokio::test]
async fn atomic_batch_rolls_back_on_error_and_plain_batch_keeps_earlier_statements() {
    let Some(c) = container().await else { return };
    let mut conn = batch_fixture(c.port, "t7_atomic").await;
    let bad = ["INSERT INTO dbo.t7_atomic (a) VALUES (1)", "INSERT INTO dbo.t7_atomic (a) VALUES (NULL)"].map(String::from);
    let err = conn.execute_batch(&bad, true).await.unwrap_err();
    assert!(err.to_string().contains("statement 2"), "{err}");
    assert_eq!(count_rows(&mut conn, "t7_atomic").await, Value::I64(0));
    assert!(conn.execute_batch(&bad, false).await.is_err());
    assert_eq!(count_rows(&mut conn, "t7_atomic").await, Value::I64(1));
    let good = ["INSERT INTO dbo.t7_atomic (a) VALUES (5)".to_owned()];
    assert_eq!(conn.execute_batch(&good, true).await.unwrap(), vec![1]);
    assert_eq!(count_rows(&mut conn, "t7_atomic").await, Value::I64(2));
}

#[tokio::test]
async fn unicode_roundtrip_through_the_dialect_literal() {
    let Some(c) = container().await else { return };
    let mut conn = batch_fixture(c.port, "t7_unicode").await;
    let text = "ciao àèì 日本 it's [x]";
    let insert = format!("INSERT INTO dbo.t7_unicode (a, s) VALUES (1, {})", dialect_for(DriverId::SqlServer).literal(&Value::Text(text.into())));
    conn.execute_batch(&[insert], true).await.unwrap();
    let (batches, _) = collect(&mut conn, "SELECT s FROM dbo.t7_unicode", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::Text(text.into())]]);
}

#[tokio::test]
async fn creates_renames_and_drops_databases() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    for name in ["t7_made", "t7_renamed"] {
        let _ = conn.drop_database(name).await;
    }
    conn.create_database(&NewDatabase { name: "t7_made".into(), encoding: None, collation: Some("Latin1_General_100_CI_AS".into()) }).await.unwrap();
    let has = |dbs: Vec<dbzilla_drivers::DatabaseInfo>, n: &str| dbs.iter().any(|d| d.name == n);
    assert!(has(conn.list_databases().await.unwrap(), "t7_made"));
    let (batches, _) = collect(&mut conn, "SELECT CAST(DATABASEPROPERTYEX('t7_made', 'Collation') AS nvarchar(100))", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::Text("Latin1_General_100_CI_AS".into())]]);
    conn.rename_database("t7_made", "t7_renamed").await.unwrap();
    let dbs = conn.list_databases().await.unwrap();
    assert!(has(dbs.clone(), "t7_renamed") && !has(dbs, "t7_made"));
    conn.drop_database("t7_renamed").await.unwrap();
    assert!(!has(conn.list_databases().await.unwrap(), "t7_renamed"));
    let mut inside = connect(c.port, "").await;
    inside.create_database(&NewDatabase { name: "t7_made".into(), encoding: None, collation: None }).await.unwrap();
    inside.set_database("t7_made").await.unwrap();
    inside.drop_database("t7_made").await.unwrap();
    assert!(!has(inside.list_databases().await.unwrap(), "t7_made"));
}

#[tokio::test]
async fn a_collation_that_is_not_a_plain_name_is_refused() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let req = NewDatabase { name: "t7_bad".into(), encoding: None, collation: Some("x; DROP DATABASE master".into()) };
    let err = conn.create_database(&req).await.unwrap_err();
    assert!(matches!(&err, DriverError::Query(m) if m.contains("not a valid collation")), "{err:?}");
    assert!(!conn.list_databases().await.unwrap().iter().any(|d| d.name == "t7_bad"));
}

#[tokio::test]
async fn cancel_stops_waitfor_and_the_connection_recovers() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t8_cancel') IS NULL CREATE DATABASE t8_cancel"]).await;
    let mut conn = connect(c.port, "").await;
    conn.set_database("t8_cancel").await.unwrap();
    let token = conn.cancel_handle().await.expect("cancel token");
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel().await
    });
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let started = Instant::now();
    let err = conn.query_stream("WAITFOR DELAY '00:00:30'", opts(), tx).await.expect_err("the query must be cancelled");
    assert!(matches!(err, DriverError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "cancel took {:?}", started.elapsed());
    canceller.await.unwrap().expect("KILL succeeded");
    let (batches, _) = collect(&mut conn, "SELECT DB_NAME()", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::Text("t8_cancel".into())]]);
}

#[tokio::test]
async fn a_cancel_token_taken_after_a_cancel_targets_the_new_session() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let first = conn.cancel_handle().await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let slow = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        first.cancel().await
    });
    assert!(matches!(conn.query_stream("WAITFOR DELAY '00:00:30'", opts(), tx).await, Err(DriverError::Cancelled)));
    slow.await.unwrap().unwrap();
    // The session was killed: a new token must reconnect first and point at the new session id.
    let second = conn.cancel_handle().await.unwrap();
    let again = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        second.cancel().await
    });
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    assert!(matches!(conn.query_stream("WAITFOR DELAY '00:00:30'", opts(), tx).await, Err(DriverError::Cancelled)));
    again.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_transaction_opened_in_one_call_spans_the_next_calls() {
    let Some(c) = container().await else { return };
    let mut conn = batch_fixture(c.port, "t8_tx").await;
    let step = |sql: &str| vec![sql.to_owned()];
    conn.execute_batch(&step("BEGIN TRANSACTION"), false).await.unwrap();
    conn.execute_batch(&step("INSERT INTO dbo.t8_tx (a) VALUES (1)"), false).await.unwrap();
    conn.execute_batch(&step("ROLLBACK"), false).await.unwrap();
    assert_eq!(count_rows(&mut conn, "t8_tx").await, Value::I64(0));
    conn.execute_batch(&step("BEGIN TRANSACTION"), false).await.unwrap();
    conn.execute_batch(&step("INSERT INTO dbo.t8_tx (a) VALUES (2)"), false).await.unwrap();
    conn.execute_batch(&step("COMMIT"), false).await.unwrap();
    assert_eq!(count_rows(&mut conn, "t8_tx").await, Value::I64(1));
}

static APP_QUERIES: OnceCell<()> = OnceCell::const_new();

/// A database with a keyed table and a heap (no key), 25 rows each, as the data tab would meet them.
async fn app_queries_fixture(port: u16) {
    APP_QUERIES
        .get_or_init(|| async move {
            raw_exec(port, &["IF DB_ID('t10_app') IS NULL CREATE DATABASE t10_app"]).await;
            raw_exec(
                port,
                &[
                    "USE t10_app; CREATE TABLE dbo.keyed (id int PRIMARY KEY, name nvarchar(20))",
                    "USE t10_app; CREATE TABLE dbo.heap (id int, name nvarchar(20))",
                    "USE t10_app; INSERT INTO dbo.keyed SELECT TOP 25 ROW_NUMBER() OVER (ORDER BY (SELECT NULL)), N'n' FROM sys.all_objects",
                    "USE t10_app; INSERT INTO dbo.heap SELECT TOP 25 ROW_NUMBER() OVER (ORDER BY (SELECT NULL)), N'n' FROM sys.all_objects",
                ],
            )
            .await;
        })
        .await;
}

fn app_table(name: &str) -> TableRef {
    TableRef { database: Some("t10_app".into()), schema: Some("dbo".into()), name: name.into() }
}

#[tokio::test]
async fn the_data_tab_queries_run_on_sql_server() {
    use dbzilla_core::paging::{build_count, build_select, SortKey};
    let Some(c) = container().await else { return };
    app_queries_fixture(c.port).await;
    let mut conn = connect(c.port, "").await;
    let d = dialect_for(DriverId::SqlServer);
    let sorted = [SortKey { column: "id".into(), desc: true }];
    let (page, _) = collect(&mut conn, &build_select(d.as_ref(), &app_table("keyed"), &[], &[], false, &sorted, 10, 10), opts()).await;
    let ids: Vec<Value> = rows_of(&page).into_iter().map(|r| r[0].clone()).collect();
    assert_eq!(ids, (6..=15).rev().map(Value::I64).collect::<Vec<_>>());
    let (last, _) = collect(&mut conn, &build_select(d.as_ref(), &app_table("heap"), &[], &[], false, &[], 10, 20), opts()).await;
    assert_eq!(rows_of(&last).len(), 5, "a table without key and without sort still pages");
    let (count, _) = collect(&mut conn, &build_count(d.as_ref(), &app_table("keyed"), &[], &[], false), opts()).await;
    assert_eq!(rows_of(&count), vec![vec![Value::I64(25)]]);
}

#[tokio::test]
async fn the_editor_row_limit_and_top_script_run_on_sql_server() {
    use dbzilla_core::rowlimit::apply_row_limit_for;
    let Some(c) = container().await else { return };
    app_queries_fixture(c.port).await;
    let mut conn = connect(c.port, "t10_app").await;
    let d = dialect_for(DriverId::SqlServer);
    for sql in ["SELECT id FROM dbo.keyed ORDER BY id DESC;", "SELECT id FROM dbo.heap", "WITH x AS (SELECT id FROM dbo.keyed) SELECT id FROM x"] {
        let limited = apply_row_limit_for(d.as_ref(), sql, Some(5));
        let (batches, _) = collect(&mut conn, &limited, opts()).await;
        assert_eq!(rows_of(&batches).len(), 5, "{limited}");
    }
    let (top, _) = collect(&mut conn, &d.select_top(&d.quote_table(&app_table("keyed")), 3), opts()).await;
    assert_eq!(rows_of(&top).len(), 3);
}

const PLAIN_PW: &str = "Plain_Pw_1234!";

/// Creates a login that has no server permissions (and an optional default language).
async fn make_login(port: u16, name: &str, language: Option<&str>) {
    let lang = language.map(|l| format!(", DEFAULT_LANGUAGE = [{l}]")).unwrap_or_default();
    raw_exec(port, &[&format!("IF SUSER_ID('{name}') IS NULL CREATE LOGIN [{name}] WITH PASSWORD = N'{PLAIN_PW}', CHECK_POLICY = OFF{lang}")]).await;
}

async fn connect_as(port: u16, user: &str) -> Box<dyn Conn> {
    let mut config = cfg(port, "");
    config.user = user.into();
    config.password = Some(PLAIN_PW.into());
    driver_for(DriverId::SqlServer).unwrap().connect(&config).await.unwrap_or_else(|e| panic!("login {user}: {e}"))
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

#[tokio::test]
async fn a_dmy_language_login_reads_dates_the_way_they_are_written() {
    let Some(c) = container().await else { return };
    make_login(c.port, "dmy_user", Some("Italian")).await;
    let mut conn = connect_as(c.port, "dmy_user").await;
    let expected = vec![vec![Value::DateTime(day(2026, 11, 5).and_hms_opt(10, 0, 0).unwrap())]];
    let literal = dialect_for(DriverId::SqlServer).literal(&Value::DateTime(day(2026, 11, 5).and_hms_opt(10, 0, 0).unwrap()));
    let (b, _) = collect(&mut conn, &format!("SELECT CAST({literal} AS datetime)"), opts()).await;
    assert_eq!(rows_of(&b), expected, "the literal of the dialect must not depend on the login language");
    let (b, _) = collect(&mut conn, "SELECT CAST('2026-11-05 10:00:00' AS datetime)", opts()).await;
    assert_eq!(rows_of(&b), expected, "typed ISO text must be read as year-month-day");
}

#[tokio::test]
async fn legacy_datetime_keeps_milliseconds_and_its_literal_is_accepted() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let (b, _) = collect(&mut conn, "SELECT CAST('2020-01-02T03:04:05.997' AS datetime), CAST('2020-01-02T03:04:05.003' AS datetime)", opts()).await;
    let row = rows_of(&b).remove(0);
    assert_eq!(row[0], Value::DateTime(day(2020, 1, 2).and_hms_milli_opt(3, 4, 5, 997).unwrap()));
    assert_eq!(row[1], Value::DateTime(day(2020, 1, 2).and_hms_milli_opt(3, 4, 5, 3).unwrap()));
    let d = dialect_for(DriverId::SqlServer);
    for v in [&row[0], &row[1]] {
        let (back, _) = collect(&mut conn, &format!("SELECT CAST({} AS datetime)", d.literal(v)), opts()).await;
        assert_eq!(&rows_of(&back)[0][0], v, "{}", d.literal(v));
    }
}

static DML: OnceCell<()> = OnceCell::const_new();

/// Table with an audit trigger that does not silence its row counts.
async fn dml_fixture(port: u16) {
    DML.get_or_init(|| async move {
        raw_exec(port, &["IF DB_ID('t12_dml') IS NULL CREATE DATABASE t12_dml"]).await;
        raw_exec(
            port,
            &[
                "USE t12_dml; CREATE TABLE dbo.items (id int PRIMARY KEY, note nvarchar(50), [Output] int NULL)",
                "USE t12_dml; CREATE TABLE dbo.audit (id int IDENTITY, msg nvarchar(50))",
                "USE t12_dml; EXEC('CREATE TRIGGER dbo.trg_items ON dbo.items AFTER UPDATE AS INSERT INTO dbo.audit (msg) SELECT N''u'' FROM inserted')",
                "USE t12_dml; INSERT INTO dbo.items (id, note) VALUES (1, N'a'), (2, N'b')",
                "USE t12_dml; CREATE TABLE dbo.plain (id int PRIMARY KEY, note nvarchar(50))",
                "USE t12_dml; INSERT INTO dbo.plain (id, note) VALUES (7, N'p')",
            ],
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn affected_rows_ignore_what_triggers_do_and_words_inside_text() {
    let Some(c) = container().await else { return };
    dml_fixture(c.port).await;
    let mut conn = connect(c.port, "t12_dml").await;
    for sql in ["UPDATE dbo.items SET note = N'x' WHERE id = 1", "UPDATE dbo.items SET note = N'see output' WHERE id = 1", "UPDATE dbo.items SET [Output] = 3 WHERE id = 1"] {
        let (_, summary) = collect(&mut conn, sql, opts()).await;
        assert_eq!(summary.affected, Some(1), "{sql}");
        assert_eq!(conn.execute_batch(&[sql.to_owned()], false).await.unwrap(), vec![1], "{sql}");
    }
    let (batches, summary) = collect(&mut conn, "UPDATE dbo.plain SET note = N'y' OUTPUT inserted.id WHERE id = 7", opts()).await;
    assert_eq!(rows_of(&batches), vec![vec![Value::I64(7)]]);
    assert_eq!(summary.affected, Some(1), "a statement with OUTPUT returns its rows and still reports the count");
}

#[tokio::test]
async fn a_plain_user_can_cancel_and_a_second_cancel_is_harmless() {
    let Some(c) = container().await else { return };
    make_login(c.port, "plain_user", None).await;
    let mut conn = connect_as(c.port, "plain_user").await;
    let token = conn.cancel_handle().await.unwrap();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let first = token.cancel().await;
        let second = token.cancel().await;
        (first, second)
    });
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let started = Instant::now();
    let err = conn.query_stream("WAITFOR DELAY '00:00:30'", opts(), tx).await.expect_err("must be cancelled");
    assert!(matches!(err, DriverError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "cancel took {:?}", started.elapsed());
    let (first, second) = canceller.await.unwrap();
    assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}: a cancel never fails the caller");
    conn.ping().await.expect("the connection works after a cancel");
}

#[tokio::test]
async fn a_lost_connection_is_reported_not_silently_replaced() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let (b, _) = collect(&mut conn, "SELECT @@SPID", opts()).await;
    let spid = match &rows_of(&b)[0][0] {
        Value::I64(n) => *n,
        other => panic!("{other:?}"),
    };
    raw_exec(c.port, &[&format!("KILL {spid}")]).await;
    assert!(conn.ping().await.is_err(), "the first call after the loss fails");
    assert!(conn.ping().await.is_err(), "the next one must not open a new session behind the user's back");
}

#[tokio::test]
async fn the_default_timeout_does_not_cut_a_query_short() {
    let Some(c) = container().await else { return };
    let mut conn = connect(c.port, "").await;
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let started = Instant::now();
    conn.query_stream("WAITFOR DELAY '00:00:03'", QueryOpts { timeout: Duration::from_secs(1), ..opts() }, tx).await.expect("timing is the caller's business, as for the other drivers");
    assert!(started.elapsed() >= Duration::from_secs(3));
}

/// A TCP forwarder that counts the connections it accepts.
async fn counting_proxy(target: u16) -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(mut out) = TcpStream::connect(("127.0.0.1", target)).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                }
            });
        }
    });
    (port, count)
}

#[tokio::test]
async fn a_refused_login_is_not_retried_without_encryption() {
    let Some(c) = container().await else { return };
    let (port, count) = counting_proxy(c.port).await;
    let mut bad = cfg(port, "");
    bad.ssl = SslMode::Preferred;
    bad.password = Some("wrong".into());
    let err = driver_for(DriverId::SqlServer).unwrap().connect(&bad).await.err().expect("login must fail");
    assert!(matches!(err, DriverError::Connect(_)), "{err:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1, "the password must not be sent a second time without TLS");
}

#[tokio::test]
async fn renaming_a_database_in_use_fails_and_changes_nothing() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t8_busy') IS NULL CREATE DATABASE t8_busy"]).await;
    let _holder = connect(c.port, "t8_busy").await;
    let mut conn = connect(c.port, "").await;
    let started = Instant::now();
    assert!(conn.rename_database("t8_busy", "t8_busy_new").await.is_err());
    assert!(started.elapsed() < Duration::from_secs(30), "it must fail, not wait");
    let names: Vec<String> = conn.list_databases().await.unwrap().into_iter().map(|d| d.name).collect();
    assert!(names.contains(&"t8_busy".to_owned()) && !names.contains(&"t8_busy_new".to_owned()), "{names:?}");
}

#[tokio::test]
async fn a_use_typed_in_the_editor_survives_a_reconnect() {
    let Some(c) = container().await else { return };
    raw_exec(c.port, &["IF DB_ID('t12_use') IS NULL CREATE DATABASE t12_use"]).await;
    let mut conn = connect(c.port, "").await;
    collect(&mut conn, "USE t12_use", opts()).await;
    let token = conn.cancel_handle().await.unwrap();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        token.cancel().await
    });
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    assert!(matches!(conn.query_stream("WAITFOR DELAY '00:00:30'", opts(), tx).await, Err(DriverError::Cancelled)));
    canceller.await.unwrap().unwrap();
    let (b, _) = collect(&mut conn, "SELECT DB_NAME()", opts()).await;
    assert_eq!(rows_of(&b), vec![vec![Value::Text("t12_use".into())]], "the session came back in another database");
}
