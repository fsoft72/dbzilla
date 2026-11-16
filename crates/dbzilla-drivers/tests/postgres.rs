//! PostgreSQL driver integration tests against a throwaway Docker container.

mod support;

use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, NewDatabase, SslMode};
use std::time::Duration;
use support::{collect, opts, rows_of, Container};
use tokio::sync::OnceCell;

const LABEL: &str = "dbzilla-test-pg";
static SERVER: OnceCell<Option<Container>> = OnceCell::const_new();

#[dtor::dtor(unsafe)]
fn cleanup_container() {
    support::cleanup(LABEL);
}

fn cfg(port: u16, db: &str) -> ConnectConfig {
    ConnectConfig {
        driver: DriverId::Postgres,
        host: "127.0.0.1".into(),
        port: Some(port),
        user: "postgres".into(),
        password: Some("pw".into()),
        database: db.into(),
        ssl: SslMode::Disabled,
        bootstrap: vec![],
    }
}

/// Starts (once) and waits for the server; None when Docker is unavailable.
async fn server() -> Option<&'static Container> {
    SERVER
        .get_or_init(|| async {
            let c = support::start(LABEL, "postgres:16-alpine", &[("POSTGRES_PASSWORD", "pw")], 5432, &[])?;
            let driver = driver_for(DriverId::Postgres).unwrap();
            for _ in 0..120 {
                if let Ok(mut conn) = driver.connect(&cfg(c.port, "postgres")).await {
                    if conn.ping().await.is_ok() {
                        return Some(c);
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            panic!("postgres container did not become ready");
        })
        .await
        .as_ref()
}

macro_rules! require_server {
    () => {
        match server().await {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED: docker is not available");
                return;
            }
        }
    };
}

/// Creates a fresh database and returns an open connection to it.
async fn fresh(srv: &Container, name: &str) -> Box<dyn Conn> {
    let driver = driver_for(DriverId::Postgres).unwrap();
    let mut admin = driver.connect(&cfg(srv.port, "postgres")).await.unwrap();
    admin.execute_batch(&[format!("DROP DATABASE IF EXISTS \"{name}\"")], false).await.unwrap();
    admin.execute_batch(&[format!("CREATE DATABASE \"{name}\"")], false).await.unwrap();
    driver.connect(&cfg(srv.port, name)).await.unwrap()
}

#[tokio::test]
async fn connect_version_bootstrap_and_errors() {
    let srv = require_server!();
    let driver = driver_for(DriverId::Postgres).unwrap();
    let mut config = cfg(srv.port, "postgres");
    config.bootstrap = vec!["SET application_name = 'dbz'".into()];
    let mut c = driver.connect(&config).await.unwrap();
    assert!(c.server_version().await.unwrap().starts_with("PostgreSQL 16"));
    let (b, _) = collect(&mut c, "SHOW application_name", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::Text("dbz".into()));
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let err = c.query_stream("SELEC 1", opts(), tx).await.unwrap_err();
    assert!(err.to_string().contains("syntax error"), "{err}");
    config.password = Some("wrong".into());
    let err = driver.connect(&config).await.err().expect("bad password must fail");
    assert!(err.to_string().to_lowercase().contains("password"), "{err}");
    config.password = Some("pw".into());
    config.ssl = SslMode::Required;
    assert!(driver.connect(&config).await.is_err(), "server without TLS must refuse a REQUIRED connection");
    config.ssl = SslMode::Preferred;
    assert!(driver.connect(&config).await.is_ok(), "PREFERRED falls back to plain");
}

#[tokio::test]
async fn schemas_objects_and_structure() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_structure").await;
    let ddl: Vec<String> = [
        "CREATE SCHEMA shop",
        "CREATE TABLE shop.customers (id serial PRIMARY KEY, name text NOT NULL DEFAULT 'x')",
        "CREATE TABLE shop.orders (id serial PRIMARY KEY, customer_id int REFERENCES shop.customers(id) ON DELETE CASCADE ON UPDATE NO ACTION, total numeric(10,2))",
        "CREATE INDEX ix_orders_customer ON shop.orders (customer_id, total)",
        "CREATE VIEW shop.v_orders AS SELECT * FROM shop.orders",
        "CREATE MATERIALIZED VIEW shop.mv_orders AS SELECT * FROM shop.orders",
        "CREATE SEQUENCE shop.seq1",
        "CREATE FUNCTION shop.f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        "CREATE FUNCTION shop.trg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE TRIGGER trg_orders BEFORE INSERT ON shop.orders FOR EACH ROW EXECUTE FUNCTION shop.trg()",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    c.execute_batch(&ddl, true).await.unwrap();
    let schemas = c.list_schemas("t_structure").await.unwrap();
    assert!(schemas.contains(&"public".to_owned()) && schemas.contains(&"shop".to_owned()));
    assert!(!schemas.iter().any(|s| s == "pg_catalog" || s == "information_schema"));
    let objs = c.list_objects("t_structure", Some("shop")).await.unwrap();
    let has = |k: ObjectKind, n: &str| objs.iter().any(|o| o.kind == k && o.table.name == n);
    assert!(has(ObjectKind::Table, "orders") && has(ObjectKind::Table, "customers"));
    assert!(has(ObjectKind::View, "v_orders"));
    assert!(has(ObjectKind::MaterializedView, "mv_orders"));
    assert!(has(ObjectKind::Sequence, "seq1"));
    assert!(has(ObjectKind::Function, "f"));
    let t = TableRef { database: None, schema: Some("shop".into()), name: "orders".into() };
    let st = c.table_structure(&t).await.unwrap();
    let names: Vec<&str> = st.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "customer_id", "total"]);
    assert_eq!(st.columns[2].type_name, "numeric(10,2)");
    assert_eq!(st.primary_key, vec!["id"]);
    assert!(st.columns[0].default.as_deref().unwrap_or("").starts_with("nextval"));
    let fk = st.columns[1].fk.as_ref().expect("fk");
    assert_eq!((fk.table.schema.as_deref(), fk.table.name.as_str()), (Some("shop"), "customers"));
    assert_eq!((fk.on_delete.as_str(), fk.on_update.as_str()), ("CASCADE", "NO ACTION"));
    assert_eq!(st.indexes.len(), 1);
    assert_eq!((st.indexes[0].name.as_str(), st.indexes[0].columns.clone()), ("ix_orders_customer", vec!["customer_id".to_owned(), "total".to_owned()]));
    assert_eq!((st.triggers[0].name.as_str(), st.triggers[0].timing.as_str(), st.triggers[0].event.as_str()), ("trg_orders", "BEFORE", "INSERT"));
    assert!(st.ddl.contains("CREATE TABLE \"shop\".\"orders\""), "{}", st.ddl);
    let v = TableRef { database: None, schema: Some("shop".into()), name: "v_orders".into() };
    assert!(c.table_structure(&v).await.unwrap().ddl.starts_with("CREATE OR REPLACE VIEW"));
}

#[tokio::test]
async fn value_mapping() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_values").await;
    c.execute_batch(&["CREATE TYPE mood AS ENUM ('sad','happy')".into()], false).await.unwrap();
    let sql = "SELECT true, 1::int2, 2::int4, 3::int8, 1.5::float4, 2.5::float8, 12345.6700::numeric, 'txt'::text, '\\x00ff'::bytea, \
        '2024-01-02'::date, '03:04:05'::time, '2024-01-02 03:04:05'::timestamp, '2024-01-02 03:04:05+00'::timestamptz, \
        '11111111-1111-1111-1111-111111111111'::uuid, '{\"a\":1}'::jsonb, ARRAY[1,2,3], 'happy'::mood, NULL::text, \
        interval '1 day 2 hours', '10.0.0.1'::inet, '10.0.0.0/8'::cidr, '10.0.0.1/24'::inet, (-0.0012)::numeric, 0::numeric, \
        100000000::numeric, 'NaN'::numeric, 'infinity'::timestamp, ARRAY['a','b']::text[], ARRAY[[1,2],[3,4]]";
    let (b, _) = collect(&mut c, sql, opts()).await;
    let r = &rows_of(&b)[0];
    let utc = FixedOffset::east_opt(0).unwrap();
    let ts = NaiveDateTime::new(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(), NaiveTime::from_hms_opt(3, 4, 5).unwrap());
    let expected = vec![
        Value::Bool(true),
        Value::I64(1),
        Value::I64(2),
        Value::I64(3),
        Value::F64(1.5),
        Value::F64(2.5),
        Value::Decimal("12345.6700".into()),
        Value::Text("txt".into()),
        Value::Bytes(vec![0, 255]),
        Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap()),
        Value::DateTime(ts),
        Value::DateTimeTz(utc.from_utc_datetime(&ts)),
        Value::Uuid(uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()),
        Value::Json(serde_json::json!({"a": 1})),
        Value::Array(vec![Value::I64(1), Value::I64(2), Value::I64(3)]),
        Value::Text("happy".into()),
        Value::Null,
        Value::Other("1 day 02:00:00".into()),
        Value::Other("10.0.0.1".into()),
        Value::Other("10.0.0.0/8".into()),
        Value::Other("10.0.0.1/24".into()),
        Value::Decimal("-0.0012".into()),
        Value::Decimal("0".into()),
        Value::Decimal("100000000".into()),
        Value::Decimal("NaN".into()),
        Value::Other("infinity".into()),
        Value::Array(vec![Value::Text("a".into()), Value::Text("b".into())]),
        Value::Array(vec![Value::Array(vec![Value::I64(1), Value::I64(2)]), Value::Array(vec![Value::I64(3), Value::I64(4)])]),
    ];
    for (i, (got, want)) in r.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "column {i}");
    }
    assert_eq!(r.len(), expected.len());
    assert_eq!(b[0].columns.as_ref().unwrap()[2].type_name, "int4");
}

#[tokio::test]
async fn streams_and_limits() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_stream").await;
    let (b, s) = collect(&mut c, "SELECT g, 'row' || g FROM generate_series(1, 20000) g", opts()).await;
    assert!(b[0].columns.is_some() && b[0].rows.is_empty());
    assert!(b.iter().all(|x| x.rows.len() <= 200));
    assert_eq!(rows_of(&b).len(), 20_000);
    assert!(b.len() >= 100);
    assert_eq!(s.affected, None);
    let o = dbzilla_drivers::QueryOpts { limit: Some(25), ..opts() };
    let (b, _) = collect(&mut c, "SELECT g FROM generate_series(1, 100000) g", o).await;
    assert_eq!(rows_of(&b).len(), 25);
    let (_, s) = collect(&mut c, "CREATE TABLE tt (a int)", opts()).await;
    assert_eq!(s.affected, Some(0));
    let (_, s) = collect(&mut c, "INSERT INTO tt SELECT generate_series(1, 7)", opts()).await;
    assert_eq!(s.affected, Some(7));
    // the connection stays usable after an early stop
    let (b, _) = collect(&mut c, "SELECT count(*) FROM tt", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_interrupts_a_long_query() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_cancel").await;
    let token = c.cancel_handle().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let task = tokio::spawn(async move {
        let r = c.query_stream("SELECT pg_sleep(30)", opts(), tx).await;
        (r, c)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    token.cancel().await.unwrap();
    let (res, mut c) = tokio::time::timeout(Duration::from_secs(10), task).await.expect("cancel must be prompt").unwrap();
    assert!(matches!(res, Err(DriverError::Cancelled)), "{res:?}");
    assert!(c.ping().await.is_ok(), "connection survives a cancel");
}

#[tokio::test]
async fn atomic_batches_notices_and_estimates() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_batch").await;
    c.execute_batch(&["CREATE TABLE t (a int NOT NULL)".into()], true).await.unwrap();
    let bad = vec!["INSERT INTO t VALUES (1)".to_owned(), "INSERT INTO t VALUES (NULL)".to_owned()];
    let err = c.execute_batch(&bad, true).await.unwrap_err();
    assert!(err.to_string().contains("statement 2"), "{err}");
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(0));
    assert!(c.execute_batch(&bad, false).await.is_err());
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(1));
    assert_eq!(c.execute_batch(&["UPDATE t SET a = 2".to_owned()], true).await.unwrap(), vec![1]);
    let (_, s) = collect(&mut c, "DO $$ BEGIN RAISE NOTICE 'hello %', 1; END $$", opts()).await;
    assert!(s.messages.iter().any(|m| m.contains("hello 1")), "{:?}", s.messages);
    c.execute_batch(&["INSERT INTO t SELECT generate_series(1, 1000)".into(), "ANALYZE t".into()], false).await.unwrap();
    let est = c.estimate_count(&TableRef::simple("t")).await.unwrap().expect("estimate");
    assert!((900..=1100).contains(&est), "{est}");
}

#[tokio::test]
async fn database_operations_and_switching() {
    let srv = require_server!();
    let mut c = fresh(srv, "t_dbops").await;
    for n in ["t_created", "t_renamed"] {
        c.execute_batch(&[format!("DROP DATABASE IF EXISTS {n}")], false).await.unwrap();
    }
    c.create_database(&NewDatabase { name: "t_created".into(), encoding: Some("UTF8".into()), collation: None }).await.unwrap();
    let names: Vec<String> = c.list_databases().await.unwrap().into_iter().map(|d| d.name).collect();
    assert!(names.contains(&"t_created".to_owned()) && !names.contains(&"template0".to_owned()));
    c.rename_database("t_created", "t_renamed").await.unwrap();
    c.set_database("t_renamed").await.unwrap();
    let (b, _) = collect(&mut c, "SELECT current_database()", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::Text("t_renamed".into()));
    assert!(c.drop_database("t_renamed").await.is_err(), "cannot drop the open database");
    c.set_database("t_dbops").await.unwrap();
    c.drop_database("t_renamed").await.unwrap();
}
