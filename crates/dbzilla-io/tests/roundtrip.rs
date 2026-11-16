//! Export and import round trips on real SQLite files (SPEC 12.1, 12.2).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::TableRef;
use dbzilla_core::value::Value;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, QueryOpts, SslMode};
use dbzilla_io::export::{run_export, CsvExportOptions, ExportFormat, ExportOptions, ExportSource, LineBreak, SqlExportOptions};
use dbzilla_io::import::*;
use dbzilla_io::naming::NameContext;
use dbzilla_io::IoError;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

async fn open(path: &Path) -> Box<dyn Conn> {
    let cfg = ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: path.to_string_lossy().into_owned(), ssl: SslMode::Disabled, bootstrap: vec![] };
    driver_for(DriverId::Sqlite).unwrap().connect(&cfg).await.unwrap()
}

fn new_db(dir: &Path, name: &str, setup: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    rusqlite::Connection::open(&path).unwrap().execute_batch(setup).unwrap();
    path
}

async fn rows(conn: &mut Box<dyn Conn>, sql: &str) -> Vec<Vec<Value>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<dbzilla_drivers::RowBatch>(8);
    let reader = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(b) = rx.recv().await {
            out.extend(b.rows);
        }
        out
    });
    conn.query_stream(sql, QueryOpts::default(), tx).await.unwrap();
    reader.await.unwrap()
}

const SETUP: &str = "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, score REAL, note TEXT);
    INSERT INTO people VALUES (1, 'Ann', 1.5, 'plain'), (2, 'Bob, Jr.', 2.25, 'say \"hi\"'), (3, 'Cy', NULL, ''), (4, NULL, 4.0, 'two
lines');";

fn ctx() -> NameContext {
    NameContext { connection: "conn".into(), database: "db".into(), tag: "dev".into(), date: "20260101".into(), time: "101010".into() }
}

fn opts(format: ExportFormat, dir: &Path) -> ExportOptions {
    ExportOptions { format, csv: CsvExportOptions::default(), sql: SqlExportOptions::default(), gzip: false, dir: dir.to_path_buf(), name_pattern: "{table}".into() }
}

fn table(name: &str) -> ExportSource {
    ExportSource::Table { table: TableRef::simple(name), columns: None, where_sql: None, order_sql: Some("id".into()), page: None }
}

async fn export(path: &Path, out: &Path, o: &ExportOptions, sources: &[ExportSource]) -> Vec<std::path::PathBuf> {
    let mut c = open(path).await;
    let cancel = AtomicBool::new(false);
    run_export(c.as_mut(), DriverId::Sqlite, sources, o, &ctx(), &|_| {}, &cancel).await.unwrap_or_else(|e| panic!("export failed: {e} ({out:?})"))
}

#[tokio::test]
async fn csv_export_follows_the_options() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    let mut o = opts(ExportFormat::Csv, dir.path());
    let files = export(&db, dir.path(), &o, &[table("people")]).await;
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("people.csv"));
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text, "id,name,score,note\n1,Ann,1.5,plain\n2,\"Bob, Jr.\",2.25,\"say \"\"hi\"\"\"\n3,Cy,NULL,\n4,NULL,4,\"two\nlines\"\n");
    o.csv = CsvExportOptions { delimiter: ';', decimal: ',', null_as_empty: true, line_break: LineBreak::CrLf, header: false, newline_to_space: true, ..CsvExportOptions::default() };
    let files = export(&db, dir.path(), &o, &[table("people")]).await;
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text, "1;Ann;1,5;plain\r\n2;Bob, Jr.;2,25;\"say \"\"hi\"\"\"\r\n3;Cy;;\r\n4;;4;two lines\r\n");
}

#[tokio::test]
async fn json_export_is_an_array_of_objects() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    let files = export(&db, dir.path(), &opts(ExportFormat::Json, dir.path()), &[table("people")]).await;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 4);
    assert_eq!(v[0]["name"], "Ann");
    assert!(v[2]["score"].is_null());
    assert_eq!(v[2]["note"], "", "an empty string stays distinct from NULL");
    assert!(v[3]["name"].is_null());
}

#[tokio::test]
async fn column_selection_filter_and_page() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    let src = ExportSource::Table { table: TableRef::simple("people"), columns: Some(vec!["id".into(), "name".into()]), where_sql: Some("id >= 2".into()), order_sql: Some("id".into()), page: Some((1, 1)) };
    let files = export(&db, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[src]).await;
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "id,name\n3,Cy\n");
}

#[tokio::test]
async fn query_results_can_be_exported() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    let src = ExportSource::Query { name: "report".into(), sql: "SELECT count(*) AS n FROM people".into() };
    let files = export(&db, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[src]).await;
    assert!(files[0].ends_with("report.csv"));
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "n\n4\n");
}

#[tokio::test]
async fn csv_round_trip_keeps_values_nulls_and_empty_strings() {
    let dir = tempfile::tempdir().unwrap();
    let src = new_db(dir.path(), "src.sqlite3", SETUP);
    let files = export(&src, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[table("people")]).await;
    let dst = new_db(dir.path(), "dst.sqlite3", "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, score REAL, note TEXT);");
    let bytes = std::fs::read(&files[0]).unwrap();
    let parsed = parse_csv(&bytes, &CsvImportOptions::default()).unwrap();
    let mut c = open(&dst).await;
    let plan = ImportPlan { table: TableRef::simple("people"), create: None, mapping: mapping_for(&parsed, &["INTEGER", "TEXT", "REAL", "TEXT"]), mode: InsertMode::StopOnError, key_columns: vec![] };
    let summary = run_import(c.as_mut(), DriverId::Sqlite, &plan, &parsed, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    assert_eq!(summary.rows, 4);
    let got = rows(&mut c, "SELECT id, name, score, note FROM people ORDER BY id").await;
    let mut s = open(&src).await;
    assert_eq!(got, rows(&mut s, "SELECT id, name, score, note FROM people ORDER BY id").await);
}

fn mapping_for(p: &Parsed, types: &[&str]) -> Vec<Option<(String, String)>> {
    p.headers.iter().zip(types).map(|(h, t)| Some((h.clone(), (*t).to_owned()))).collect()
}

#[tokio::test]
async fn json_round_trip_into_a_new_table_with_detected_types() {
    let dir = tempfile::tempdir().unwrap();
    let src = new_db(dir.path(), "src.sqlite3", SETUP);
    let files = export(&src, dir.path(), &opts(ExportFormat::Json, dir.path()), &[table("people")]).await;
    let parsed = parse_json(&std::fs::read(&files[0]).unwrap()).unwrap();
    assert_eq!(parsed.headers, vec!["id", "name", "score", "note"]);
    let types = detect_types(&parsed);
    assert_eq!(types, vec![DetectedType::Integer, DetectedType::Text, DetectedType::Real, DetectedType::Text]);
    let dst = new_db(dir.path(), "dst.sqlite3", "SELECT 1;");
    let mut c = open(&dst).await;
    let cols: Vec<(String, String)> = parsed.headers.iter().zip(&types).map(|(h, t)| (h.clone(), t.sql_type(DriverId::Sqlite).to_owned())).collect();
    let plan = ImportPlan { table: TableRef::simple("imported"), create: Some(cols.clone()), mapping: cols.iter().map(|(n, t)| Some((n.clone(), t.clone()))).collect(), mode: InsertMode::StopOnError, key_columns: vec![] };
    run_import(c.as_mut(), DriverId::Sqlite, &plan, &parsed, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    let got = rows(&mut c, "SELECT id, name, score, note FROM imported ORDER BY id").await;
    let mut s = open(&src).await;
    assert_eq!(got, rows(&mut s, "SELECT id, name, score, note FROM people ORDER BY id").await);
}

#[tokio::test]
async fn sql_dump_gzip_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let src = new_db(dir.path(), "src.sqlite3", SETUP);
    let mut o = opts(ExportFormat::Sql, dir.path());
    o.gzip = true;
    o.sql = SqlExportOptions { structure: true, drop: true, data: true };
    let files = export(&src, dir.path(), &o, &[table("people")]).await;
    assert!(files[0].to_string_lossy().ends_with(".sql.gz"));
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(std::fs::File::open(&files[0]).unwrap()), &mut text).unwrap();
    assert!(text.contains("DROP TABLE IF EXISTS \"people\";") && text.contains("CREATE TABLE") && text.contains("INSERT INTO \"people\""), "{text}");
    let dst = new_db(dir.path(), "dst.sqlite3", "SELECT 1;");
    let mut c = open(&dst).await;
    let n = run_sql_dump(c.as_mut(), DriverId::Sqlite, &text, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    assert!(n >= 3, "drop, create and at least one insert ran");
    let got = rows(&mut c, "SELECT id, name, score, note FROM people ORDER BY id").await;
    let mut s = open(&src).await;
    assert_eq!(got, rows(&mut s, "SELECT id, name, score, note FROM people ORDER BY id").await);
}

#[tokio::test]
async fn xlsx_export_writes_a_workbook() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    let files = export(&db, dir.path(), &opts(ExportFormat::Xlsx, dir.path()), &[table("people")]).await;
    let bytes = std::fs::read(&files[0]).unwrap();
    assert_eq!(&bytes[..2], b"PK", "an xlsx file is a zip archive");
    assert!(bytes.len() > 1000);
}

#[tokio::test]
async fn cancel_removes_the_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", "CREATE TABLE big (n INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 200000) INSERT INTO big SELECT x FROM c;");
    let mut c = open(&db).await;
    let cancel = AtomicBool::new(true);
    let res = run_export(c.as_mut(), DriverId::Sqlite, &[ExportSource::Table { table: TableRef::simple("big"), columns: None, where_sql: None, order_sql: None, page: None }], &opts(ExportFormat::Csv, dir.path()), &ctx(), &|_| {}, &cancel).await;
    assert!(matches!(res, Err(IoError::Cancelled)), "{res:?}");
    assert!(!dir.path().join("big.csv").exists());
    cancel.store(false, Ordering::SeqCst);
}

#[tokio::test]
async fn import_modes_and_rollback_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'old');");
    let mut c = open(&db).await;
    let parsed = parse_csv(b"id,name\n2,new\n1,dup\n", &CsvImportOptions::default()).unwrap();
    let map = || mapping_for(&parsed, &["INTEGER", "TEXT"]);
    let plan = |mode| ImportPlan { table: TableRef::simple("t"), create: None, mapping: map(), mode, key_columns: vec!["id".into()] };
    let err = run_import(c.as_mut(), DriverId::Sqlite, &plan(InsertMode::StopOnError), &parsed, &|_| {}, &AtomicBool::new(false)).await;
    assert!(err.is_err());
    assert_eq!(rows(&mut c, "SELECT count(*) FROM t").await[0][0], Value::I64(1), "the failed import was rolled back");
    run_import(c.as_mut(), DriverId::Sqlite, &plan(InsertMode::UpdateOnDuplicate), &parsed, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    assert_eq!(rows(&mut c, "SELECT id, name FROM t ORDER BY id").await, vec![vec![Value::I64(1), Value::Text("dup".into())], vec![Value::I64(2), Value::Text("new".into())]]);
    let parsed2 = parse_csv(b"id,name\n2,replaced\n", &CsvImportOptions::default()).unwrap();
    let plan2 = ImportPlan { table: TableRef::simple("t"), create: None, mapping: mapping_for(&parsed2, &["INTEGER", "TEXT"]), mode: InsertMode::Replace, key_columns: vec![] };
    run_import(c.as_mut(), DriverId::Sqlite, &plan2, &parsed2, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    assert_eq!(rows(&mut c, "SELECT name FROM t WHERE id = 2").await[0][0], Value::Text("replaced".into()));
}

#[tokio::test]
async fn bad_values_are_reported_with_their_position() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", "CREATE TABLE t (id INTEGER PRIMARY KEY, qty INTEGER);");
    let mut c = open(&db).await;
    let parsed = parse_csv(b"id,qty\n1,5\n2,abc\n", &CsvImportOptions::default()).unwrap();
    let plan = ImportPlan { table: TableRef::simple("t"), create: None, mapping: mapping_for(&parsed, &["INTEGER", "INTEGER"]), mode: InsertMode::StopOnError, key_columns: vec![] };
    let err = run_import(c.as_mut(), DriverId::Sqlite, &plan, &parsed, &|_| {}, &AtomicBool::new(false)).await.unwrap_err();
    let text = err.to_string();
    assert!(text.contains("row 2") && text.contains("qty") && text.contains("abc"), "{text}");
    assert_eq!(rows(&mut c, "SELECT count(*) FROM t").await[0][0], Value::I64(0));
}

/// Review M9: an export never overwrites an existing file, it picks a free name.
#[tokio::test]
async fn export_never_overwrites_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", SETUP);
    std::fs::write(dir.path().join("people.csv"), "precious").unwrap();
    let files = export(&db, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[table("people")]).await;
    assert_eq!(std::fs::read_to_string(dir.path().join("people.csv")).unwrap(), "precious");
    assert!(files[0].ends_with("people-1.csv"), "{files:?}");
    let again = export(&db, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[table("people")]).await;
    assert!(again[0].ends_with("people-2.csv"), "{again:?}");
    let mut o = opts(ExportFormat::Sql, dir.path());
    o.gzip = true;
    std::fs::write(dir.path().join("people.sql.gz"), "x").unwrap();
    let sql = export(&db, dir.path(), &o, &[table("people")]).await;
    assert!(sql[0].ends_with("people-1.sql.gz"), "{sql:?}");
}

/// Review M12: a failed import into a new table leaves no empty table behind.
#[tokio::test]
async fn failed_import_into_a_new_table_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = new_db(dir.path(), "a.sqlite3", "SELECT 1;");
    let mut c = open(&db).await;
    let parsed = parse_csv(b"id\n1\nnot a number\n", &CsvImportOptions::default()).unwrap();
    let cols = vec![("id".to_owned(), "INTEGER".to_owned())];
    let plan = ImportPlan { table: TableRef::simple("fresh"), create: Some(cols.clone()), mapping: vec![Some(cols[0].clone())], mode: InsertMode::StopOnError, key_columns: vec![] };
    assert!(run_import(c.as_mut(), DriverId::Sqlite, &plan, &parsed, &|_| {}, &AtomicBool::new(false)).await.is_err());
    let tables = rows(&mut c, "SELECT count(*) FROM sqlite_master WHERE name = 'fresh'").await;
    assert_eq!(tables[0][0], Value::I64(0));
}

/// A text value "NULL" survives a CSV round trip next to a real NULL: quoted means text.
#[tokio::test]
async fn csv_round_trip_tells_the_text_null_from_a_real_null() {
    let dir = tempfile::tempdir().unwrap();
    let src = new_db(dir.path(), "src.sqlite3", "CREATE TABLE w (id INTEGER PRIMARY KEY, v TEXT); INSERT INTO w VALUES (1, 'NULL'), (2, NULL), (3, ''), (4, 'a');");
    let files = export(&src, dir.path(), &opts(ExportFormat::Csv, dir.path()), &[table("w")]).await;
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(text, "id,v\n1,\"NULL\"\n2,NULL\n3,\n4,a\n");
    let dst = new_db(dir.path(), "dst.sqlite3", "CREATE TABLE w (id INTEGER PRIMARY KEY, v TEXT);");
    let parsed = parse_csv(text.as_bytes(), &CsvImportOptions::default()).unwrap();
    let mut c = open(&dst).await;
    let plan = ImportPlan { table: TableRef::simple("w"), create: None, mapping: mapping_for(&parsed, &["INTEGER", "TEXT"]), mode: InsertMode::StopOnError, key_columns: vec![] };
    run_import(c.as_mut(), DriverId::Sqlite, &plan, &parsed, &|_| {}, &AtomicBool::new(false)).await.unwrap();
    let mut s = open(&src).await;
    assert_eq!(rows(&mut c, "SELECT id, v FROM w ORDER BY id").await, rows(&mut s, "SELECT id, v FROM w ORDER BY id").await);
}
