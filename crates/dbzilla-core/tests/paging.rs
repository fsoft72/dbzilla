//! Paged SELECT / COUNT builders (FR-D-001).

use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::filter::{Filter, FilterColumn, FilterOp};
use dbzilla_core::model::{ColumnMeta, TableRef};
use dbzilla_core::paging::{build_count, build_select, SortKey};

fn cols() -> Vec<ColumnMeta> {
    vec![ColumnMeta { name: "id".into(), type_name: "integer".into(), ..Default::default() }]
}

fn filter() -> Filter {
    Filter { enabled: true, column: FilterColumn::Column("id".into()), op: FilterOp::Eq, value: "5".into() }
}

#[test]
fn select_with_filter_sort_and_paging() {
    let d = dialect_for(DriverId::Postgres);
    let t = TableRef { database: None, schema: Some("public".into()), name: "orders".into() };
    let sort = vec![SortKey { column: "name".into(), desc: true }, SortKey { column: "id".into(), desc: false }];
    assert_eq!(
        build_select(d.as_ref(), &t, &cols(), &[filter()], false, &sort, 300, 600),
        "SELECT * FROM \"public\".\"orders\" WHERE \"id\" = 5 ORDER BY \"name\" DESC, \"id\" ASC LIMIT 300 OFFSET 600"
    );
}

#[test]
fn select_plain_and_mysql_quoting() {
    let d = dialect_for(DriverId::MySql);
    assert_eq!(build_select(d.as_ref(), &TableRef::simple("t"), &[], &[], false, &[], 300, 0), "SELECT * FROM `t` LIMIT 300 OFFSET 0");
}

#[test]
fn count_ignores_sort_and_paging() {
    let d = dialect_for(DriverId::Sqlite);
    assert_eq!(build_count(d.as_ref(), &TableRef::simple("t"), &cols(), &[filter()], false), "SELECT COUNT(*) FROM \"t\" WHERE \"id\" = 5");
    assert_eq!(build_count(d.as_ref(), &TableRef::simple("t"), &cols(), &[], false), "SELECT COUNT(*) FROM \"t\"");
}

#[test]
fn sqlserver_pages_with_offset_fetch_and_a_fallback_order() {
    let d = dialect_for(DriverId::SqlServer);
    assert_eq!(
        build_select(d.as_ref(), &TableRef::simple("t"), &[], &[], false, &[], 300, 600),
        "SELECT * FROM [t] ORDER BY (SELECT NULL) OFFSET 600 ROWS FETCH NEXT 300 ROWS ONLY"
    );
    let sort = vec![SortKey { column: "id".into(), desc: true }];
    let sql = build_select(d.as_ref(), &TableRef::simple("t"), &[], &[], false, &sort, 300, 0);
    assert_eq!(sql, "SELECT * FROM [t] ORDER BY [id] DESC OFFSET 0 ROWS FETCH NEXT 300 ROWS ONLY");
    assert_eq!(sql.matches("ORDER BY").count(), 1);
}
