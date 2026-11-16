//! Pending change to SQL tests (SPEC 9.1).

use dbzilla_core::changes::{generate_sql, CellValue, ChangeError, ChangeSet, RowChange};
use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::model::TableRef;
use dbzilla_core::value::Value;

fn cs(rows: Vec<RowChange>) -> ChangeSet {
    let mut c = ChangeSet::new(TableRef::simple("t"));
    for r in rows {
        c.push(r);
    }
    c
}

fn sql(id: DriverId, rows: Vec<RowChange>) -> Result<Vec<String>, ChangeError> {
    generate_sql(&cs(rows), dialect_for(id).as_ref())
}

fn ins(values: Vec<(&str, CellValue)>) -> RowChange {
    RowChange::Insert { values: values.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() }
}

#[test]
fn insert_keeps_null_empty_and_expr_distinct_and_omits_default() {
    let r = ins(vec![
        ("a", CellValue::Null),
        ("b", CellValue::Value(Value::Text(String::new()))),
        ("skip", CellValue::Default),
        ("c", CellValue::Expr("NOW()".into())),
    ]);
    assert_eq!(sql(DriverId::Postgres, vec![r]).unwrap(), vec!["INSERT INTO \"t\" (\"a\", \"b\", \"c\") VALUES (NULL, '', NOW())"]);
}

#[test]
fn insert_with_only_defaults() {
    assert_eq!(sql(DriverId::Sqlite, vec![ins(vec![])]).unwrap(), vec!["INSERT INTO \"t\" DEFAULT VALUES"]);
    assert_eq!(sql(DriverId::MySql, vec![ins(vec![("x", CellValue::Default)])]).unwrap(), vec!["INSERT INTO `t` () VALUES ()"]);
}

#[test]
fn update_sets_only_changed_columns_with_composite_and_null_key() {
    let r = RowChange::Update {
        key: vec![("a".into(), Value::I64(1)), ("b".into(), Value::Null)],
        set: vec![("c".into(), CellValue::Value(Value::Text("x'y".into())))],
    };
    assert_eq!(sql(DriverId::Postgres, vec![r]).unwrap(), vec!["UPDATE \"t\" SET \"c\" = 'x''y' WHERE \"a\" = 1 AND \"b\" IS NULL"]);
}

#[test]
fn consecutive_updates_of_same_key_merge_later_wins() {
    let key = vec![("id".to_owned(), Value::I64(7))];
    let a = RowChange::Update { key: key.clone(), set: vec![("n".into(), CellValue::Value(Value::I64(1)))] };
    let b = RowChange::Update { key, set: vec![("n".into(), CellValue::Value(Value::I64(2))), ("m".into(), CellValue::Null)] };
    assert_eq!(sql(DriverId::Sqlite, vec![a, b]).unwrap(), vec!["UPDATE \"t\" SET \"n\" = 2, \"m\" = NULL WHERE \"id\" = 7"]);
}

#[test]
fn delete_by_composite_key() {
    let r = RowChange::Delete { key: vec![("a".into(), Value::I64(1)), ("b".into(), Value::Text("z".into()))] };
    assert_eq!(sql(DriverId::MySql, vec![r]).unwrap(), vec!["DELETE FROM `t` WHERE `a` = 1 AND `b` = 'z'"]);
}

#[test]
fn update_and_delete_without_key_are_refused() {
    let u = RowChange::Update { key: vec![], set: vec![("c".into(), CellValue::Null)] };
    assert!(matches!(sql(DriverId::Sqlite, vec![u]), Err(ChangeError::NoKey { .. })));
    let d = RowChange::Delete { key: vec![] };
    assert!(matches!(sql(DriverId::Sqlite, vec![d]), Err(ChangeError::NoKey { .. })));
}

#[test]
fn default_in_update_depends_on_dialect() {
    let u = || RowChange::Update { key: vec![("id".into(), Value::I64(1))], set: vec![("c".into(), CellValue::Default)] };
    assert_eq!(sql(DriverId::Postgres, vec![u()]).unwrap(), vec!["UPDATE \"t\" SET \"c\" = DEFAULT WHERE \"id\" = 1"]);
    assert!(matches!(sql(DriverId::Sqlite, vec![u()]), Err(ChangeError::DefaultNotSupported { .. })));
}

#[test]
fn update_with_empty_set_produces_nothing() {
    let u = RowChange::Update { key: vec![("id".into(), Value::I64(1))], set: vec![] };
    assert!(sql(DriverId::Sqlite, vec![u]).unwrap().is_empty());
}

#[test]
fn undo_redo() {
    let mut c = ChangeSet::new(TableRef::simple("t"));
    c.push(RowChange::Delete { key: vec![("id".into(), Value::I64(1))] });
    c.push(RowChange::Delete { key: vec![("id".into(), Value::I64(2))] });
    assert_eq!(c.rows.len(), 2);
    assert!(c.undo().is_some());
    assert_eq!(c.rows.len(), 1);
    assert!(c.redo().is_some());
    assert_eq!(c.rows.len(), 2);
    c.undo();
    c.push(RowChange::Delete { key: vec![("id".into(), Value::I64(3))] });
    assert!(c.redo().is_none(), "a new change clears the redo stack");
    assert!(!c.is_empty());
}
