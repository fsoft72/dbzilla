//! SQLite driver integration tests on real files.

use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectKind, TableRef};
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, DriverError, NewDatabase, QueryOpts, RowBatch, SslMode};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

fn cfg(path: &Path) -> ConnectConfig {
    ConnectConfig {
        driver: DriverId::Sqlite,
        host: String::new(),
        port: None,
        user: String::new(),
        password: None,
        database: path.to_string_lossy().into_owned(),
        ssl: SslMode::Disabled,
        bootstrap: vec![],
    }
}

async fn open(path: &Path) -> Box<dyn Conn> {
    driver_for(DriverId::Sqlite).unwrap().connect(&cfg(path)).await.unwrap()
}

fn opts() -> QueryOpts {
    QueryOpts { limit: None, timeout: Duration::from_secs(30), batch_size: 200 }
}

async fn collect(conn: &mut Box<dyn Conn>, sql: &str, o: QueryOpts) -> (Vec<RowBatch>, dbzilla_drivers::QuerySummary) {
    let (tx, mut rx) = mpsc::channel(4);
    let consumer = tokio::spawn(async move {
        let mut v = Vec::new();
        while let Some(b) = rx.recv().await {
            v.push(b);
        }
        v
    });
    let summary = conn.query_stream(sql, o, tx).await.unwrap();
    (consumer.await.unwrap(), summary)
}

fn rows_of(batches: &[RowBatch]) -> Vec<Vec<Value>> {
    batches.iter().flat_map(|b| b.rows.clone()).collect()
}

#[tokio::test]
async fn missing_file_is_a_clear_error_and_spaces_work() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.sqlite");
    let err = driver_for(DriverId::Sqlite).unwrap().connect(&cfg(&missing)).await.err().expect("must fail");
    assert!(err.to_string().contains("nope.sqlite"), "{err}");
    assert!(!missing.exists(), "must not create the file");
    let spaced = dir.path().join("my db.sqlite");
    rusqlite::Connection::open(&spaced).unwrap();
    let mut c = open(&spaced).await;
    assert!(c.server_version().await.unwrap().starts_with("SQLite"));
}

#[tokio::test]
async fn structure_objects_fk_index_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    c.execute_batch(
        &[
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x')".into(),
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers(id) ON DELETE CASCADE, total REAL)".into(),
            "CREATE INDEX ix_orders_customer ON orders (customer_id, total)".into(),
            "CREATE VIEW v_orders AS SELECT * FROM orders".into(),
            "CREATE TRIGGER trg_orders AFTER INSERT ON orders BEGIN SELECT 1; END".into(),
        ],
        true,
    )
    .await
    .unwrap();
    let objs = c.list_objects("main", None).await.unwrap();
    let kinds: Vec<(ObjectKind, String)> = objs.iter().map(|o| (o.kind, o.table.name.clone())).collect();
    assert!(kinds.contains(&(ObjectKind::Table, "customers".into())));
    assert!(kinds.contains(&(ObjectKind::View, "v_orders".into())));
    let st = c.table_structure(&TableRef::simple("orders")).await.unwrap();
    assert_eq!(st.columns.len(), 3);
    assert_eq!(st.primary_key, vec!["id"]);
    let fk = st.columns[1].fk.as_ref().expect("fk");
    assert_eq!(fk.table.name, "customers");
    assert_eq!(fk.columns, vec!["id"]);
    assert_eq!(fk.on_delete, "CASCADE");
    assert_eq!(st.indexes.len(), 1);
    assert_eq!(st.indexes[0].name, "ix_orders_customer");
    assert_eq!(st.indexes[0].columns, vec!["customer_id", "total"]);
    assert_eq!(st.triggers.len(), 1);
    assert_eq!(st.triggers[0].timing, "AFTER");
    assert_eq!(st.triggers[0].event, "INSERT");
    assert!(st.ddl.starts_with("CREATE TABLE orders"));
    let cs = c.table_structure(&TableRef::simple("customers")).await.unwrap();
    assert!(!cs.columns[1].nullable);
    assert_eq!(cs.columns[1].default.as_deref(), Some("'x'"));
}

#[tokio::test]
async fn streams_in_batches_with_columns_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    let sql = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 10000) SELECT x, 'row' || x AS label FROM c";
    let (batches, summary) = collect(&mut c, sql, opts()).await;
    assert!(batches[0].columns.is_some() && batches[0].rows.is_empty());
    assert_eq!(batches[0].columns.as_ref().unwrap()[1].name, "label");
    assert!(batches.iter().all(|b| b.rows.len() <= 200));
    assert_eq!(rows_of(&batches).len(), 10_000);
    assert!(batches.len() >= 50);
    assert_eq!(rows_of(&batches)[9_999][0], Value::I64(10_000));
    assert_eq!(summary.affected, None);
}

#[tokio::test]
async fn value_mapping_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    let (b, _) = collect(&mut c, "SELECT 1, 1.5, 'x', x'00FF', NULL", opts()).await;
    assert_eq!(
        rows_of(&b)[0],
        vec![Value::I64(1), Value::F64(1.5), Value::Text("x".into()), Value::Bytes(vec![0, 255]), Value::Null]
    );
    let sql = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 1000) SELECT x FROM c";
    let (b, _) = collect(&mut c, sql, QueryOpts { limit: Some(25), ..opts() }).await;
    assert_eq!(rows_of(&b).len(), 25);
}

#[tokio::test]
async fn non_row_statement_reports_affected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    c.execute_batch(&["CREATE TABLE t (a INTEGER)".into(), "INSERT INTO t VALUES (1),(2),(3)".into()], true).await.unwrap();
    let (batches, summary) = collect(&mut c, "UPDATE t SET a = a + 1", opts()).await;
    assert!(rows_of(&batches).is_empty());
    assert_eq!(summary.affected, Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_interrupts_a_long_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    let token = c.cancel_handle().await.unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let task = tokio::spawn(async move {
        let sql = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c";
        c.query_stream(sql, opts(), tx).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    token.cancel().await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), task).await.expect("cancel must be prompt").unwrap();
    assert!(matches!(res, Err(DriverError::Cancelled)), "{res:?}");
}

#[tokio::test]
async fn atomic_batch_rolls_back_and_non_atomic_keeps_earlier_statements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut c = open(&path).await;
    c.execute_batch(&["CREATE TABLE t (a INTEGER NOT NULL)".into()], true).await.unwrap();
    let bad = vec!["INSERT INTO t VALUES (1)".to_owned(), "INSERT INTO t VALUES (NULL)".to_owned()];
    let err = c.execute_batch(&bad, true).await.unwrap_err();
    assert!(err.to_string().contains("statement 2"), "{err}");
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(0));
    assert!(c.execute_batch(&bad, false).await.is_err());
    let (b, _) = collect(&mut c, "SELECT count(*) FROM t", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(1));
    let counts = c.execute_batch(&["UPDATE t SET a = 5".to_owned()], true).await.unwrap();
    assert_eq!(counts, vec![1]);
}

#[tokio::test]
async fn bootstrap_commands_run_on_connect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.sqlite");
    rusqlite::Connection::open(&path).unwrap();
    let mut config = cfg(&path);
    config.bootstrap = vec!["PRAGMA user_version = 7".into()];
    let mut c = driver_for(DriverId::Sqlite).unwrap().connect(&config).await.unwrap();
    let (b, _) = collect(&mut c, "PRAGMA user_version", opts()).await;
    assert_eq!(rows_of(&b)[0][0], Value::I64(7));
}

#[tokio::test]
async fn database_files_list_create_rename_drop_switch() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.sqlite3");
    rusqlite::Connection::open(&a).unwrap();
    rusqlite::Connection::open(dir.path().join("b.db")).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    let mut c = open(&a).await;
    let names: Vec<String> = c.list_databases().await.unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec!["a.sqlite3", "b.db"]);
    c.create_database(&NewDatabase { name: "new".into(), encoding: None, collation: None }).await.unwrap();
    assert!(dir.path().join("new.sqlite3").exists());
    c.rename_database("new.sqlite3", "renamed.sqlite3").await.unwrap();
    assert!(dir.path().join("renamed.sqlite3").exists());
    c.drop_database("renamed.sqlite3").await.unwrap();
    assert!(!dir.path().join("renamed.sqlite3").exists());
    assert!(c.drop_database("a.sqlite3").await.is_err(), "cannot drop the open database");
    c.set_database("b.db").await.unwrap();
    c.execute_batch(&["CREATE TABLE only_in_b (x)".into()], true).await.unwrap();
    let objs = c.list_objects("b.db", None).await.unwrap();
    assert!(objs.iter().any(|o| o.table.name == "only_in_b"));
}

/// Review I9: renaming onto an existing file is refused and the sidecar files move along.
#[tokio::test]
async fn rename_database_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["a.sqlite3", "b.sqlite3", "open.sqlite3"] {
        rusqlite::Connection::open(dir.path().join(n)).unwrap();
    }
    std::fs::write(dir.path().join("a.sqlite3-wal"), b"w").unwrap();
    let cfg = dbzilla_drivers::ConnectConfig { driver: dbzilla_core::dialect::DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: dir.path().join("open.sqlite3").to_string_lossy().into_owned(), ssl: dbzilla_drivers::SslMode::Disabled, bootstrap: vec![] };
    let mut c = dbzilla_drivers::driver_for(dbzilla_core::dialect::DriverId::Sqlite).unwrap().connect(&cfg).await.unwrap();
    assert!(c.rename_database("a.sqlite3", "b.sqlite3").await.is_err());
    assert!(dir.path().join("a.sqlite3").exists());
    c.rename_database("a.sqlite3", "c.sqlite3").await.unwrap();
    assert!(dir.path().join("c.sqlite3").exists() && dir.path().join("c.sqlite3-wal").exists() && !dir.path().join("a.sqlite3-wal").exists());
}

/// A rename that fails half way puts the files back.
#[tokio::test]
async fn failed_rename_restores_the_files() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["a.sqlite3", "open.sqlite3"] {
        rusqlite::Connection::open(dir.path().join(n)).unwrap();
    }
    std::fs::write(dir.path().join("a.sqlite3-wal"), b"w").unwrap();
    std::fs::create_dir_all(dir.path().join("c.sqlite3-wal").join("blocker")).unwrap();
    let cfg = dbzilla_drivers::ConnectConfig { driver: dbzilla_core::dialect::DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: dir.path().join("open.sqlite3").to_string_lossy().into_owned(), ssl: dbzilla_drivers::SslMode::Disabled, bootstrap: vec![] };
    let mut c = dbzilla_drivers::driver_for(dbzilla_core::dialect::DriverId::Sqlite).unwrap().connect(&cfg).await.unwrap();
    assert!(c.rename_database("a.sqlite3", "c.sqlite3").await.is_err());
    assert!(dir.path().join("a.sqlite3").exists() && dir.path().join("a.sqlite3-wal").exists(), "the original files are back");
    assert!(!dir.path().join("c.sqlite3").exists());
}

/// Altering a column of a real table with constraints, a partial index and a trigger keeps all of them.
#[tokio::test]
async fn column_rebuild_keeps_constraints_indexes_and_triggers() {
    use dbzilla_core::ddl::{generate_ddl, ColumnDef, StructuralOp};
    use dbzilla_core::dialect::{dialect_for, DriverId};
    use dbzilla_core::model::TableRef;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("r.sqlite3");
    rusqlite::Connection::open(&path).unwrap().execute_batch(
        "CREATE TABLE a (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT UNIQUE COLLATE NOCASE, qty INTEGER NOT NULL DEFAULT 1 CHECK (qty > 0), note TEXT);
         CREATE INDEX ix_note ON a (note) WHERE note IS NOT NULL;
         CREATE INDEX ix_lower ON a (lower(note));
         CREATE TABLE log (msg TEXT);
         CREATE TRIGGER trg AFTER INSERT ON a BEGIN INSERT INTO log VALUES ('inserted'); END;
         INSERT INTO a (email, qty, note) VALUES ('x@y.z', 2, 'n');
         CREATE TABLE child (aid INTEGER REFERENCES a(id) ON DELETE CASCADE);",
    ).unwrap();
    let cfg = dbzilla_drivers::ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: path.to_string_lossy().into_owned(), ssl: dbzilla_drivers::SslMode::Disabled, bootstrap: vec![] };
    let mut c = dbzilla_drivers::driver_for(DriverId::Sqlite).unwrap().connect(&cfg).await.unwrap();
    let alter = |st: dbzilla_core::model::TableStructure| StructuralOp::AlterColumn {
        table: TableRef::simple("a"),
        old: ColumnDef { name: "note".into(), type_name: "TEXT".into(), nullable: true, default: None, extra: None },
        new: ColumnDef { name: "note".into(), type_name: "VARCHAR(40)".into(), nullable: true, default: None, extra: None },
        current: Some(Box::new(st)),
    };
    let st = c.table_structure(&TableRef::simple("a")).await.unwrap();
    assert_eq!(st.referenced_by, vec!["child".to_owned()]);
    assert!(generate_ddl(&alter(st), dialect_for(DriverId::Sqlite).as_ref()).is_err(), "a referenced table is refused");
    rusqlite::Connection::open(&path).unwrap().execute_batch("DROP TABLE child;").unwrap();
    let st = c.table_structure(&TableRef::simple("a")).await.unwrap();
    assert!(st.referenced_by.is_empty() && st.index_sql.len() == 2);
    let stmts = generate_ddl(&alter(st), dialect_for(DriverId::Sqlite).as_ref()).unwrap();
    c.execute_batch(&stmts, true).await.unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM a", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "the data is kept");
    assert!(db.execute("INSERT INTO a (email, qty) VALUES ('X@Y.Z', 1)", []).is_err(), "UNIQUE with NOCASE still holds");
    assert!(db.execute("INSERT INTO a (email, qty) VALUES ('q@y.z', 0)", []).is_err(), "CHECK still holds");
    db.execute("INSERT INTO a (email, qty) VALUES ('ok@y.z', 3)", []).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM log", [], |r| r.get::<_, i64>(0)).unwrap(), 2, "the trigger fires again (1 original + 1 new)");
    let objects: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE tbl_name = 'a' ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
    assert!(objects.contains(&"ix_note".to_owned()) && objects.contains(&"ix_lower".to_owned()) && objects.contains(&"trg".to_owned()), "{objects:?}");
    let ddl: String = db.query_row("SELECT sql FROM sqlite_master WHERE name = 'a'", [], |r| r.get(0)).unwrap();
    assert!(ddl.contains("VARCHAR(40)") && ddl.contains("AUTOINCREMENT"), "{ddl}");
}
