//! SQL Server driver integration tests against a throwaway Docker container.
//! Switched on with `DBZILLA_TEST_MSSQL=1` (the image is big and slow to start).
#![cfg(feature = "mssql")]

mod support;

use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, SslMode};
use std::time::Duration;
use support::Container;
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
