//! Quoting and literal escaping tests for the three dialects.

use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::TableRef;
use dbzilla_core::value::Value;
use proptest::prelude::*;
use sqlparser::ast::{Expr, SelectItem, SetExpr, Statement};
use sqlparser::dialect::{Dialect as SqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

fn all() -> [Box<dyn Dialect>; 3] {
    [dialect_for(DriverId::Sqlite), dialect_for(DriverId::Postgres), dialect_for(DriverId::MySql)]
}

#[test]
fn quote_ident_doubles_embedded_quote_pg_sqlite() {
    assert_eq!(dialect_for(DriverId::Postgres).quote_ident("a\"b"), "\"a\"\"b\"");
    assert_eq!(dialect_for(DriverId::Sqlite).quote_ident("a\"b"), "\"a\"\"b\"");
}

#[test]
fn quote_ident_doubles_embedded_backtick_mysql() {
    assert_eq!(dialect_for(DriverId::MySql).quote_ident("a`b"), "`a``b`");
}

#[test]
fn quote_table_joins_parts() {
    let t = TableRef { database: None, schema: Some("public".into()), name: "orders".into() };
    assert_eq!(dialect_for(DriverId::Postgres).quote_table(&t), "\"public\".\"orders\"");
    let t = TableRef { database: Some("shop".into()), schema: None, name: "orders".into() };
    assert_eq!(dialect_for(DriverId::MySql).quote_table(&t), "`shop`.`orders`");
}

#[test]
fn literal_text_doubles_single_quote() {
    for d in all() {
        assert_eq!(d.literal(&Value::Text("O'Brien".into())), "'O''Brien'");
    }
}

#[test]
fn literal_mysql_escapes_backslash() {
    assert_eq!(dialect_for(DriverId::MySql).literal(&Value::Text("a\\b".into())), "'a\\\\b'");
}

#[test]
fn literal_null_empty_and_bytes_are_distinct() {
    for d in all() {
        assert_eq!(d.literal(&Value::Null), "NULL");
        assert_eq!(d.literal(&Value::Text(String::new())), "''");
    }
    assert_eq!(dialect_for(DriverId::Sqlite).literal(&Value::Bytes(vec![0xDE, 0xAD])), "X'DEAD'");
    assert_eq!(dialect_for(DriverId::MySql).literal(&Value::Bytes(vec![0xDE, 0xAD])), "X'DEAD'");
    assert_eq!(dialect_for(DriverId::Postgres).literal(&Value::Bytes(vec![0xDE, 0xAD])), "'\\xDEAD'");
}

#[test]
fn literal_bool_per_dialect() {
    assert_eq!(dialect_for(DriverId::Sqlite).literal(&Value::Bool(true)), "1");
    assert_eq!(dialect_for(DriverId::Postgres).literal(&Value::Bool(false)), "FALSE");
}

#[test]
fn limit_offset_syntax() {
    for d in all() {
        assert_eq!(d.limit_offset(300, 600), "LIMIT 300 OFFSET 600");
    }
}

fn parse_dialect(id: DriverId) -> Box<dyn SqlDialect> {
    match id {
        DriverId::Sqlite => Box::new(SQLiteDialect {}),
        DriverId::Postgres => Box::new(PostgreSqlDialect {}),
        DriverId::MySql => Box::new(MySqlDialect {}),
    }
}

proptest! {
    /// A quoted identifier always re-parses as exactly one identifier with the original text.
    #[test]
    fn quoted_ident_round_trips(name in "[^\\x00]{1,24}") {
        for d in all() {
            let sql = format!("SELECT {} FROM t", d.quote_ident(&name));
            let stmts = Parser::parse_sql(parse_dialect(d.id()).as_ref(), &sql).expect("parse");
            prop_assert_eq!(stmts.len(), 1);
            let Statement::Query(q) = &stmts[0] else { panic!("not a query") };
            let SetExpr::Select(sel) = q.body.as_ref() else { panic!("not a select") };
            prop_assert_eq!(sel.projection.len(), 1);
            let SelectItem::UnnamedExpr(Expr::Identifier(id)) = &sel.projection[0] else { panic!("not an identifier") };
            prop_assert_eq!(&id.value, &name);
        }
    }

    /// A text literal always re-parses as exactly one string constant with the original text.
    #[test]
    fn text_literal_round_trips(text in "[^\\x00]{0,24}") {
        for d in all() {
            let sql = format!("SELECT {}", d.literal(&Value::Text(text.clone())));
            let stmts = Parser::parse_sql(parse_dialect(d.id()).as_ref(), &sql).expect("parse");
            prop_assert_eq!(stmts.len(), 1);
            let Statement::Query(q) = &stmts[0] else { panic!("not a query") };
            let SetExpr::Select(sel) = q.body.as_ref() else { panic!("not a select") };
            prop_assert_eq!(sel.projection.len(), 1);
            let SelectItem::UnnamedExpr(Expr::Value(v)) = &sel.projection[0] else { panic!("not a value") };
            let got = match &v.value {
                sqlparser::ast::Value::SingleQuotedString(s) => s.clone(),
                other => panic!("unexpected {other:?}"),
            };
            prop_assert_eq!(got, text.clone());
        }
    }
}
