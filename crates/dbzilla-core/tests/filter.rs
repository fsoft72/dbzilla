//! Filter to WHERE tests (FR-F-001).

use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::filter::{to_where, Filter, FilterColumn, FilterOp};
use dbzilla_core::model::ColumnMeta;

fn cols() -> Vec<ColumnMeta> {
    let c = |n: &str, t: &str| ColumnMeta { name: n.into(), type_name: t.into(), nullable: true, ..Default::default() };
    vec![c("id", "integer"), c("name", "text"), c("data", "bytea")]
}

fn f(col: &str, op: FilterOp, value: &str) -> Filter {
    Filter { enabled: true, column: FilterColumn::Column(col.into()), op, value: value.into() }
}

fn pg(filters: &[Filter], or: bool) -> String {
    to_where(dialect_for(DriverId::Postgres).as_ref(), &cols(), filters, or)
}

#[test]
fn eq_numeric_is_unquoted_and_text_is_quoted() {
    assert_eq!(pg(&[f("id", FilterOp::Eq, "5")], false), "\"id\" = 5");
    assert_eq!(pg(&[f("name", FilterOp::Eq, "bob")], false), "\"name\" = 'bob'");
    assert_eq!(pg(&[f("name", FilterOp::Eq, "5")], false), "\"name\" = '5'");
    assert_eq!(pg(&[f("id", FilterOp::Eq, "5; DROP TABLE t")], false), "\"id\" = '5; DROP TABLE t'");
}

#[test]
fn comparison_operators() {
    assert_eq!(pg(&[f("id", FilterOp::Ne, "1")], false), "\"id\" <> 1");
    assert_eq!(pg(&[f("id", FilterOp::Le, "1")], false), "\"id\" <= 1");
    assert_eq!(pg(&[f("id", FilterOp::Gt, "1")], false), "\"id\" > 1");
}

#[test]
fn contains_escapes_wildcards_and_casts_on_postgres() {
    assert_eq!(
        pg(&[f("name", FilterOp::Contains, "50%_!")], false),
        "CAST(\"name\" AS text) LIKE '%50!%!_!!%' ESCAPE '!'"
    );
}

#[test]
fn text_family_on_other_dialects() {
    let my = to_where(dialect_for(DriverId::MySql).as_ref(), &cols(), &[f("name", FilterOp::HasPrefix, "bob")], false);
    assert_eq!(my, "`name` LIKE 'bob%' ESCAPE '!'");
    let sq = to_where(dialect_for(DriverId::Sqlite).as_ref(), &cols(), &[f("name", FilterOp::HasSuffix, "bob")], false);
    assert_eq!(sq, "\"name\" LIKE '%bob' ESCAPE '!'");
    let not = to_where(dialect_for(DriverId::Sqlite).as_ref(), &cols(), &[f("name", FilterOp::NotContains, "x")], false);
    assert_eq!(not, "\"name\" NOT LIKE '%x%' ESCAPE '!'");
}

#[test]
fn plain_like_passes_pattern_through() {
    assert_eq!(pg(&[f("name", FilterOp::Like, "a%b")], false), "CAST(\"name\" AS text) LIKE 'a%b'");
}

#[test]
fn ilike_family_on_postgres_and_fallback() {
    assert_eq!(pg(&[f("name", FilterOp::IContains, "x")], false), "CAST(\"name\" AS text) ILIKE '%x%' ESCAPE '!'");
    let sq = to_where(dialect_for(DriverId::Sqlite).as_ref(), &cols(), &[f("name", FilterOp::IContains, "x")], false);
    assert_eq!(sq, "LOWER(\"name\") LIKE LOWER('%x%') ESCAPE '!'");
}

#[test]
fn in_lists() {
    assert_eq!(pg(&[f("id", FilterOp::In, "1, 2, 3")], false), "\"id\" IN (1, 2, 3)");
    assert_eq!(pg(&[f("name", FilterOp::NotIn, "a, 'b,c'")], false), "\"name\" NOT IN ('a', 'b,c')");
    assert_eq!(pg(&[f("id", FilterOp::In, "")], false), "1 = 0");
    assert_eq!(pg(&[f("id", FilterOp::NotIn, "")], false), "1 = 1");
}

#[test]
fn null_checks_and_between() {
    assert_eq!(pg(&[f("name", FilterOp::IsNull, "")], false), "\"name\" IS NULL");
    assert_eq!(pg(&[f("name", FilterOp::IsNotNull, "ignored")], false), "\"name\" IS NOT NULL");
    assert_eq!(pg(&[f("id", FilterOp::Between, "1 and 10")], false), "\"id\" BETWEEN 1 AND 10");
    assert_eq!(pg(&[f("id", FilterOp::NotBetween, "1 AND 10")], false), "\"id\" NOT BETWEEN 1 AND 10");
    assert_eq!(pg(&[f("id", FilterOp::Between, "1")], false), "");
}

#[test]
fn raw_and_combination() {
    let raw = Filter { enabled: true, column: FilterColumn::Raw, op: FilterOp::Eq, value: "total > 100 AND status = 'paid'".into() };
    assert_eq!(pg(std::slice::from_ref(&raw), false), "total > 100 AND status = 'paid'");
    assert_eq!(pg(&[raw.clone(), f("id", FilterOp::Eq, "5")], false), "(total > 100 AND status = 'paid') AND (\"id\" = 5)");
    assert_eq!(pg(&[f("id", FilterOp::Eq, "5"), f("name", FilterOp::Eq, "x")], true), "(\"id\" = 5) OR (\"name\" = 'x')");
}

#[test]
fn disabled_and_empty() {
    let mut off = f("id", FilterOp::Eq, "1");
    off.enabled = false;
    assert_eq!(pg(&[off], false), "");
    assert_eq!(pg(&[], false), "");
}

#[test]
fn any_column_ors_over_text_castable_columns() {
    let any = Filter { enabled: true, column: FilterColumn::Any, op: FilterOp::Contains, value: "x".into() };
    assert_eq!(
        pg(&[any], false),
        "(CAST(\"id\" AS text) LIKE '%x%' ESCAPE '!' OR CAST(\"name\" AS text) LIKE '%x%' ESCAPE '!')"
    );
}

/// Review I3: a raw filter with a second statement never reaches the server.
#[test]
fn raw_filter_with_several_statements_matches_nothing() {
    use dbzilla_core::dialect::{dialect_for, DriverId};
    use dbzilla_core::filter::*;
    let f = Filter { enabled: true, column: FilterColumn::Raw, op: FilterOp::Eq, value: "1=1; DELETE FROM orders; -- ".into() };
    let sql = to_where(dialect_for(DriverId::MySql).as_ref(), &[], &[f], false);
    assert!(!sql.to_uppercase().contains("DELETE"), "{sql}");
}
