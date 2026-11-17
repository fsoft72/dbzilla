//! Row limit injection for the SQL editor (SPEC 10.2).

use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::rowlimit::{apply_row_limit, apply_row_limit_for, ROW_LIMITS};

#[test]
fn appends_limit_to_plain_select() {
    assert_eq!(apply_row_limit("SELECT * FROM t", Some(100)), "SELECT * FROM t\nLIMIT 100");
    assert_eq!(apply_row_limit("select a from t where x = 1;", Some(5)), "select a from t where x = 1\nLIMIT 5");
    assert_eq!(apply_row_limit("WITH c AS (SELECT 1) SELECT * FROM c", Some(10)), "WITH c AS (SELECT 1) SELECT * FROM c\nLIMIT 10");
}

#[test]
fn keeps_statements_that_already_limit() {
    for sql in ["SELECT * FROM t LIMIT 5", "select * from t limit 5 offset 2", "SELECT * FROM t FETCH FIRST 5 ROWS ONLY", "SELECT TOP 5 * FROM t"] {
        assert_eq!(apply_row_limit(sql, Some(100)), sql);
    }
}

#[test]
fn a_limit_inside_a_subquery_does_not_count() {
    assert_eq!(apply_row_limit("SELECT * FROM (SELECT * FROM t LIMIT 3) s", Some(10)), "SELECT * FROM (SELECT * FROM t LIMIT 3) s\nLIMIT 10");
}

#[test]
fn words_in_strings_and_comments_are_ignored() {
    assert_eq!(apply_row_limit("SELECT 'limit 5' FROM t", Some(7)), "SELECT 'limit 5' FROM t\nLIMIT 7");
    assert_eq!(apply_row_limit("SELECT 1 -- limit here", Some(7)), "SELECT 1 -- limit here\nLIMIT 7");
}

#[test]
fn other_statements_are_untouched() {
    for sql in ["INSERT INTO t VALUES (1)", "UPDATE t SET a = 1", "SHOW TABLES", "EXPLAIN SELECT 1", "PRAGMA table_info(t)", "SELECT * INTO copy FROM t", "CREATE TABLE x (a int)"] {
        assert_eq!(apply_row_limit(sql, Some(10)), sql);
    }
}

#[test]
fn no_limit_means_unchanged() {
    assert_eq!(apply_row_limit("SELECT * FROM t", None), "SELECT * FROM t");
}

#[test]
fn menu_values() {
    assert_eq!(ROW_LIMITS[0], None);
    assert_eq!(ROW_LIMITS[1], Some(100));
    assert_eq!(*ROW_LIMITS.last().unwrap(), Some(500_000));
}

/// Review M13: data-changing statements and FOR UPDATE never get a LIMIT appended.
#[test]
fn data_changing_statements_are_left_alone() {
    for sql in ["WITH x AS (SELECT 1 AS id) DELETE FROM t USING x WHERE t.id = x.id RETURNING *", "WITH x AS (SELECT 1 AS id) UPDATE t SET a = 1 FROM x WHERE t.id = x.id RETURNING t.a", "SELECT * FROM t FOR UPDATE", "INSERT INTO t SELECT 1"] {
        assert_eq!(apply_row_limit(sql, Some(10)), sql, "{sql}");
    }
    assert_eq!(apply_row_limit("SELECT 'update' AS w FROM t", Some(10)), "SELECT 'update' AS w FROM t\nLIMIT 10");
}

#[test]
fn sqlserver_limits_with_offset_fetch() {
    let d = dialect_for(DriverId::SqlServer);
    assert_eq!(apply_row_limit_for(d.as_ref(), "SELECT * FROM t", Some(50)), "SELECT * FROM t\nORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY");
    let sql = apply_row_limit_for(d.as_ref(), "SELECT * FROM t ORDER BY a;", Some(50));
    assert_eq!(sql, "SELECT * FROM t ORDER BY a\nOFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY");
    assert_eq!(sql.matches("ORDER BY").count(), 1);
}

#[test]
fn sqlserver_keeps_statements_that_already_limit() {
    let d = dialect_for(DriverId::SqlServer);
    for sql in ["SELECT TOP 5 * FROM t", "SELECT * FROM t ORDER BY a OFFSET 5 ROWS FETCH NEXT 5 ROWS ONLY", "SELECT * INTO c FROM t"] {
        assert_eq!(apply_row_limit_for(d.as_ref(), sql, Some(100)), sql);
    }
}

#[test]
fn other_dialects_keep_the_limit_suffix() {
    for id in [DriverId::Sqlite, DriverId::Postgres, DriverId::MySql] {
        assert_eq!(apply_row_limit_for(dialect_for(id).as_ref(), "SELECT * FROM t", Some(100)), "SELECT * FROM t\nLIMIT 100");
    }
}
