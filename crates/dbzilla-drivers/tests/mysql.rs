//! MySQL / MariaDB driver integration tests against throwaway Docker containers.

mod support;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, NewDatabase, SslMode};
use std::time::Duration;
use support::{collect, opts, rows_of, Container};
use tokio::sync::OnceCell;

const LABEL_MYSQL: &str = "dbzilla-test-mysql";
const LABEL_MARIA: &str = "dbzilla-test-mariadb";
const LABEL_NOTLS: &str = "dbzilla-test-mariadb-notls";
static MYSQL: OnceCell<Option<Container>> = OnceCell::const_new();
static MARIA: OnceCell<Option<Container>> = OnceCell::const_new();
static NOTLS: OnceCell<Option<Container>> = OnceCell::const_new();

#[dtor::dtor(unsafe)]
fn cleanup_containers() {
    support::cleanup(LABEL_MYSQL);
    support::cleanup(LABEL_MARIA);
    support::cleanup(LABEL_NOTLS);
}

fn cfg(port: u16, db: &str) -> ConnectConfig {
    ConnectConfig {
        driver: DriverId::MySql,
        host: "127.0.0.1".into(),
        port: Some(port),
        user: "root".into(),
        password: Some("pw".into()),
        database: db.into(),
        ssl: SslMode::Disabled,
        bootstrap: vec![],
    }
}

async fn start(cell: &'static OnceCell<Option<Container>>, label: &str, image: &str, env: (&str, &str), args: &[&str]) -> Option<&'static Container> {
    cell.get_or_init(|| async {
        let c = support::start(label, image, &[env], 3306, args)?;
        let driver = driver_for(DriverId::MySql).unwrap();
        for _ in 0..240 {
            if let Ok(mut conn) = driver.connect(&cfg(c.port, "")).await {
                if conn.ping().await.is_ok() {
                    return Some(c);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("{image} did not become ready");
    })
    .await
    .as_ref()
}

async fn mysql8() -> Option<&'static Container> {
    start(&MYSQL, LABEL_MYSQL, "mysql:8.4", ("MYSQL_ROOT_PASSWORD", "pw"), &[]).await
}

async fn mariadb() -> Option<&'static Container> {
    start(&MARIA, LABEL_MARIA, "mariadb:11.8", ("MARIADB_ROOT_PASSWORD", "pw"), &[]).await
}

/// A MariaDB server with TLS switched off.
async fn mariadb_no_tls() -> Option<&'static Container> {
    start(&NOTLS, LABEL_NOTLS, "mariadb:11.8", ("MARIADB_ROOT_PASSWORD", "pw"), &["--skip-ssl"]).await
}

macro_rules! require {
    ($e:expr) => {
        match $e.await {
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
    let driver = driver_for(DriverId::MySql).unwrap();
    let mut admin = driver.connect(&cfg(srv.port, "")).await.unwrap();
    admin.execute_batch(&[format!("DROP DATABASE IF EXISTS `{name}`")], false).await.unwrap();
    admin.execute_batch(&[format!("CREATE DATABASE `{name}`")], false).await.unwrap();
    driver.connect(&cfg(srv.port, name)).await.unwrap()
}

/// Shared assertions run against both MySQL and MariaDB.
async fn suite(srv: &Container, db: &str, prefix: &str) {
    let driver = driver_for(DriverId::MySql).unwrap();
    // connect, version, bootstrap, syntax errors
    let mut config = cfg(srv.port, "");
    config.bootstrap = vec!["SET @x = 5".into()];
    let mut c = driver.connect(&config).await.unwrap();
    assert!(c.server_version().await.unwrap().starts_with(prefix));
    let (b, _) = collect(&mut c, "SELECT @x", opts()).await;
    assert_eq!(rows_of(&b)[0][0].to_display(), "5");
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let err = c.query_stream("SELEC 1", opts(), tx).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("syntax"), "{err}");
    config.password = Some("wrong".into());
    let err = driver.connect(&config).await.err().expect("bad password must fail");
    assert!(err.to_string().to_lowercase().contains("access denied"), "{err}");

    // structure
    let mut c = fresh(srv, db).await;
    let ddl: Vec<String> = [
        "CREATE TABLE customers (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20) NOT NULL DEFAULT 'x') ENGINE=InnoDB",
        "CREATE TABLE orders (id INT AUTO_INCREMENT PRIMARY KEY, customer_id INT, total DECIMAL(10,2), CONSTRAINT fk_c FOREIGN KEY (customer_id) REFERENCES customers(id) ON DELETE CASCADE ON UPDATE NO ACTION) ENGINE=InnoDB",
        "CREATE INDEX ix_orders_customer ON orders (customer_id, total)",
        "CREATE VIEW v_orders AS SELECT * FROM orders",
        "CREATE TRIGGER trg_orders BEFORE INSERT ON orders FOR EACH ROW SET NEW.total = IFNULL(NEW.total, 0)",
        "CREATE FUNCTION f() RETURNS INT DETERMINISTIC RETURN 1",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    c.execute_batch(&ddl, false).await.unwrap();
    assert_eq!(c.list_schemas(db).await.unwrap(), vec![db.to_owned()]);
    let objs = c.list_objects(db, None).await.unwrap();
    let has = |k: ObjectKind, n: &str| objs.iter().any(|o| o.kind == k && o.table.name == n);
    assert!(has(ObjectKind::Table, "orders") && has(ObjectKind::Table, "customers"));
    assert!(has(ObjectKind::View, "v_orders"));
    assert!(has(ObjectKind::Function, "f"));
    let t = TableRef { database: Some(db.into()), schema: None, name: "orders".into() };
    let st = c.table_structure(&t).await.unwrap();
    assert_eq!(st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["id", "customer_id", "total"]);
    assert_eq!(st.columns[2].type_name, "decimal(10,2)");
    assert_eq!(st.primary_key, vec!["id"]);
    assert!(st.columns[0].extra.get("extra").is_some_and(|e| e.contains("auto_increment")));
    let fk = st.columns[1].fk.as_ref().expect("fk");
    assert_eq!(fk.table.name, "customers");
    assert_eq!(fk.columns, vec!["id"]);
    assert_eq!(fk.on_delete, "CASCADE");
    assert!(["NO ACTION", "RESTRICT"].contains(&fk.on_update.as_str()), "{}", fk.on_update);
    assert_eq!(st.indexes.iter().find(|i| i.name == "ix_orders_customer").unwrap().columns, vec!["customer_id", "total"]);
    assert_eq!((st.triggers[0].timing.as_str(), st.triggers[0].event.as_str()), ("BEFORE", "INSERT"));
    assert!(st.ddl.starts_with("CREATE TABLE"), "{}", st.ddl);
    let cust = c.table_structure(&TableRef { database: Some(db.into()), schema: None, name: "customers".into() }).await.unwrap();
    assert!(!cust.columns[1].nullable);
    assert_eq!(cust.columns[1].default.as_deref(), Some("'x'"));
    let view = c.table_structure(&TableRef { database: Some(db.into()), schema: None, name: "v_orders".into() }).await.unwrap();
    assert!(view.ddl.contains("VIEW"), "{}", view.ddl);

    // streaming 100000 rows
    c.execute_batch(&["CREATE TABLE d (x INT)".into(), "INSERT INTO d VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)".into()], false).await.unwrap();
    let sql = "SELECT a.x + 10*b.x + 100*c.x + 1000*d.x + 10000*e.x AS n FROM d a, d b, d c, d d, d e";
    let (b, s) = collect(&mut c, sql, opts()).await;
    assert!(b[0].columns.is_some() && b[0].rows.is_empty());
    assert!(b.iter().all(|x| x.rows.len() <= 200));
    assert_eq!(rows_of(&b).len(), 100_000);
    assert!(b.len() >= 500);
    assert_eq!(s.affected, None);
    let o = dbzilla_drivers::QueryOpts { limit: Some(25), ..opts() };
    let (b, _) = collect(&mut c, sql, o).await;
    assert_eq!(rows_of(&b).len(), 25);
    let (_, s) = collect(&mut c, "UPDATE d SET x = x + 1", opts()).await;
    assert_eq!(s.affected, Some(10));

    // atomic batches
    c.execute_batch(&["CREATE TABLE t (a INT NOT NULL) ENGINE=InnoDB".into()], false).await.unwrap();
    let bad = vec!["INSERT INTO t VALUES (1)".to_owned(), "INSERT INTO t VALUES (NULL)".to_owned()];
    let err = c.execute_batch(&bad, true).await.unwrap_err();
    assert!(err.to_string().contains("statement 2"), "{err}");
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(0));
    assert!(c.execute_batch(&bad, false).await.is_err());
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(1));
    assert_eq!(c.execute_batch(&["UPDATE t SET a = 2".to_owned()], true).await.unwrap(), vec![1]);

    // estimates
    c.execute_batch(&["ANALYZE TABLE d".into()], false).await.unwrap();
    let est = c.estimate_count(&TableRef { database: Some(db.into()), schema: None, name: "d".into() }).await.unwrap();
    assert!(est.is_some());

    // database operations
    let tmp = format!("{db}_tmp");
    c.execute_batch(&[format!("DROP DATABASE IF EXISTS `{tmp}`")], false).await.unwrap();
    c.create_database(&NewDatabase { name: tmp.clone(), encoding: Some("utf8mb4".into()), collation: None }).await.unwrap();
    let names: Vec<String> = c.list_databases().await.unwrap().into_iter().map(|d| d.name).collect();
    assert!(names.contains(&tmp) && names.contains(&db.to_owned()));
    assert!(matches!(c.rename_database(&tmp, "x").await, Err(DriverError::Unsupported(_))));
    c.set_database(&tmp).await.unwrap();
    let (b, _) = collect(&mut c, "SELECT DATABASE()", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::Text(tmp.clone()));
    c.set_database(db).await.unwrap();
    c.drop_database(&tmp).await.unwrap();
}

#[tokio::test]
async fn mysql8_suite() {
    let srv = require!(mysql8());
    suite(srv, "t_suite", "MySQL 8").await;
}

#[tokio::test]
async fn mariadb_suite() {
    let srv = require!(mariadb());
    suite(srv, "t_suite", "MariaDB 11").await;
}

#[tokio::test]
async fn mysql8_value_mapping() {
    let srv = require!(mysql8());
    let mut c = fresh(srv, "t_values").await;
    let sql = "SELECT 1, 1.5e0, 'txt', x'00FF', CAST('2024-01-02' AS DATE), CAST('2024-01-02 03:04:05' AS DATETIME), \
        CAST('{\"a\":1}' AS JSON), 12345.6700, NULL, CAST('03:04:05' AS TIME), 18446744073709551615, CAST('-838:59:59' AS TIME)";
    let (b, _) = collect(&mut c, sql, opts()).await;
    let r = &rows_of(&b)[0];
    let ts = NaiveDateTime::new(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(), NaiveTime::from_hms_opt(3, 4, 5).unwrap());
    let expected = vec![
        Value::I64(1),
        Value::F64(1.5),
        Value::Text("txt".into()),
        Value::Bytes(vec![0, 255]),
        Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        Value::DateTime(ts),
        Value::Json(serde_json::json!({"a": 1})),
        Value::Decimal("12345.6700".into()),
        Value::Null,
        Value::Time(NaiveTime::from_hms_opt(3, 4, 5).unwrap()),
        Value::U64(18_446_744_073_709_551_615),
        Value::Other("-838:59:59".into()),
    ];
    for (i, (got, want)) in r.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "column {i}");
    }
    assert_eq!(r.len(), expected.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql8_cancel_kills_the_running_query() {
    let srv = require!(mysql8());
    let mut c = fresh(srv, "t_cancel").await;
    c.execute_batch(&["CREATE TABLE d (x INT)".into(), "INSERT INTO d VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)".into()], false).await.unwrap();
    let token = c.cancel_handle().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let task = tokio::spawn(async move {
        let r = c.query_stream("SELECT MAX(SHA2(CONCAT(a.x, b.x, c.x, d.x, e.x, f.x, g.x, h.x), 256)) FROM d a, d b, d c, d d, d e, d f, d g, d h", opts(), tx).await;
        (r, c)
    });
    tokio::time::sleep(Duration::from_millis(700)).await;
    token.cancel().await.unwrap();
    let (res, mut c) = tokio::time::timeout(Duration::from_secs(10), task).await.expect("cancel must be prompt").unwrap();
    assert!(matches!(res, Err(DriverError::Cancelled)), "{res:?}");
    assert!(c.ping().await.is_ok(), "connection survives KILL QUERY");
}

#[tokio::test]
async fn tls_required_works_with_tls_and_fails_without() {
    let my = require!(mysql8());
    let driver = driver_for(DriverId::MySql).unwrap();
    let mut config = cfg(my.port, "");
    config.ssl = SslMode::Required;
    let mut c = driver.connect(&config).await.unwrap();
    let (b, _) = collect(&mut c, "SHOW STATUS LIKE 'Ssl_cipher'", opts()).await;
    assert!(!rows_of(&b)[0][1].to_display().is_empty(), "TLS must be active");
    config.ssl = SslMode::Preferred;
    assert!(driver.connect(&config).await.is_ok());
    let ma = require!(mariadb_no_tls());
    let mut config = cfg(ma.port, "");
    config.ssl = SslMode::Required;
    assert!(driver.connect(&config).await.is_err(), "server without TLS must refuse a REQUIRED connection");
    config.ssl = SslMode::Preferred;
    assert!(driver.connect(&config).await.is_ok(), "PREFERRED falls back to plain");
}

/// Review M1: a server running with NO_BACKSLASH_ESCAPES does not break backslash handling in our session.
#[tokio::test]
async fn no_backslash_escapes_is_switched_off_for_the_session() {
    let srv = require!(mysql8());
    let driver = driver_for(DriverId::MySql).unwrap();
    let mut admin = driver.connect(&cfg(srv.port, "")).await.unwrap();
    admin.execute_batch(&["SET GLOBAL sql_mode = CONCAT(@@GLOBAL.sql_mode, ',NO_BACKSLASH_ESCAPES')".into()], false).await.unwrap();
    let result = async {
        let mut c = driver.connect(&cfg(srv.port, "")).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<dbzilla_drivers::RowBatch>(4);
        let reader = tokio::spawn(async move {
            let mut rows = Vec::new();
            while let Some(b) = rx.recv().await {
                rows.extend(b.rows);
            }
            rows
        });
        c.query_stream("SELECT @@SESSION.sql_mode LIKE '%NO_BACKSLASH_ESCAPES%', CHAR_LENGTH('a\\\\b')", dbzilla_drivers::QueryOpts::default(), tx).await.unwrap();
        reader.await.unwrap()
    }
    .await;
    admin.execute_batch(&["SET GLOBAL sql_mode = REPLACE(@@GLOBAL.sql_mode, ',NO_BACKSLASH_ESCAPES', '')".into()], false).await.unwrap();
    assert_eq!(result[0][0], dbzilla_core::value::Value::I64(0), "the mode is off for our session");
    assert_eq!(result[0][1], dbzilla_core::value::Value::I64(3), "'a\\\\b' is a, one backslash, b");
}
