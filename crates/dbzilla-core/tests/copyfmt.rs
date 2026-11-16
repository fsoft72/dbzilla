//! Copy format and copy-script tests (SPEC 8.5, 8.10).

use dbzilla_core::copyfmt::{copy_script, format_rows, CopyContext, CopyFormat, CsvOptions, ScriptKind};
use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::model::{ColumnMeta, Row, TableRef, TableStructure};
use dbzilla_core::value::Value;

fn cols() -> Vec<ColumnMeta> {
    ["id", "name", "score"].iter().map(|n| ColumnMeta { name: (*n).into(), ..Default::default() }).collect()
}

fn rows() -> Vec<Row> {
    vec![
        vec![Value::I64(1), Value::Text("a,b".into()), Value::F64(1.5)],
        vec![Value::I64(2), Value::Null, Value::Text("say \"hi\"".into())],
    ]
}

fn fmt(f: CopyFormat, csv: CsvOptions) -> String {
    let d = dialect_for(DriverId::Postgres);
    let table = TableRef::simple("t");
    format_rows(f, &cols(), &rows(), &CopyContext { dialect: d.as_ref(), table: Some(&table), csv: &csv })
}

#[test]
fn plain_text_is_tab_separated_with_null_marker() {
    assert_eq!(fmt(CopyFormat::PlainText, CsvOptions::default()), "1\ta,b\t1.5\n2\tNULL\tsay \"hi\"");
}

#[test]
fn csv_quotes_only_when_needed() {
    assert_eq!(fmt(CopyFormat::Csv, CsvOptions::default()), "1,\"a,b\",1.5\n2,NULL,\"say \"\"hi\"\"\"");
}

#[test]
fn csv_with_header_and_options() {
    let opts = CsvOptions { delimiter: ';', decimal: ',', null_as_empty: true, line_break: "\r\n".into(), ..CsvOptions::default() };
    assert_eq!(fmt(CopyFormat::CsvWithHeader, opts), "id;name;score\r\n1;a,b;1,5\r\n2;;\"say \"\"hi\"\"\"");
}

#[test]
fn json_is_typed() {
    let out = fmt(CopyFormat::Json, CsvOptions::default());
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["id"], 1);
    assert_eq!(v[0]["score"], 1.5);
    assert!(v[1]["name"].is_null());
    assert_eq!(v[1]["score"], "say \"hi\"");
}

#[test]
fn markdown_escapes_pipes() {
    let d = dialect_for(DriverId::Sqlite);
    let out = format_rows(
        CopyFormat::Markdown,
        &cols()[..2],
        &[vec![Value::I64(1), Value::Text("a|b\nc".into())]],
        &CopyContext { dialect: d.as_ref(), table: None, csv: &CsvOptions::default() },
    );
    assert_eq!(out, "| id | name |\n| --- | --- |\n| 1 | a\\|b<br>c |");
}

#[test]
fn html_escapes() {
    let d = dialect_for(DriverId::Sqlite);
    let out = format_rows(
        CopyFormat::Html,
        &cols()[..2],
        &[vec![Value::I64(1), Value::Text("<b>&".into())]],
        &CopyContext { dialect: d.as_ref(), table: None, csv: &CsvOptions::default() },
    );
    assert_eq!(out, "<table><thead><tr><th>id</th><th>name</th></tr></thead><tbody><tr><td>1</td><td>&lt;b&gt;&amp;</td></tr></tbody></table>");
}

#[test]
fn insert_statements() {
    assert_eq!(
        fmt(CopyFormat::InsertStatement, CsvOptions::default()),
        "INSERT INTO \"t\" (\"id\", \"name\", \"score\") VALUES (1, 'a,b', 1.5);\nINSERT INTO \"t\" (\"id\", \"name\", \"score\") VALUES (2, NULL, 'say \"hi\"');"
    );
}

#[test]
fn copy_scripts() {
    let d = dialect_for(DriverId::Postgres);
    let st = TableStructure {
        table: TableRef { database: None, schema: Some("public".into()), name: "t".into() },
        columns: vec![ColumnMeta { name: "id".into(), type_name: "integer".into(), nullable: false, is_pk: true, ..Default::default() }],
        primary_key: vec!["id".into()],
        ddl: String::new(),
        ..Default::default()
    };
    assert_eq!(copy_script(ScriptKind::Drop, &st, d.as_ref()), "DROP TABLE \"public\".\"t\";");
    assert_eq!(copy_script(ScriptKind::Truncate, &st, d.as_ref()), "TRUNCATE TABLE \"public\".\"t\";");
    assert_eq!(copy_script(ScriptKind::SelectTop100, &st, d.as_ref()), "SELECT * FROM \"public\".\"t\" LIMIT 100;");
    assert_eq!(
        copy_script(ScriptKind::Creation, &st, d.as_ref()),
        "CREATE TABLE \"public\".\"t\" (\n  \"id\" integer NOT NULL,\n  PRIMARY KEY (\"id\")\n);"
    );
}
