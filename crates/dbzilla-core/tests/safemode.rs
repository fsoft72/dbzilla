//! Safe mode classifier and level matrix tests (SPEC 9.3, FR-S-001).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::safemode::{classify, SafeLevel, StatementKind};

fn k(sql: &str) -> StatementKind {
    classify(sql, DriverId::Postgres)
}

#[test]
fn read_only_statements() {
    assert_eq!(k("SELECT 1"), StatementKind::ReadOnly);
    assert_eq!(k("  -- c\n select * from t where a = 1"), StatementKind::ReadOnly);
    assert_eq!(k("EXPLAIN SELECT 1"), StatementKind::ReadOnly);
    assert_eq!(k("SHOW TABLES"), StatementKind::ReadOnly);
    assert_eq!(k("DESCRIBE t"), StatementKind::ReadOnly);
    assert_eq!(k("WITH x AS (SELECT 1) SELECT * FROM x"), StatementKind::ReadOnly);
    assert_eq!(classify("PRAGMA table_info(t)", DriverId::Sqlite), StatementKind::ReadOnly);
}

#[test]
fn pragma_assignment_is_write() {
    assert_eq!(classify("PRAGMA journal_mode = WAL", DriverId::Sqlite), StatementKind::Write);
}

#[test]
fn dml_is_write() {
    assert_eq!(k("INSERT INTO t VALUES (1)"), StatementKind::Write);
    assert_eq!(k("UPDATE t SET a = 1"), StatementKind::Write);
    assert_eq!(k("DELETE FROM t"), StatementKind::Write);
}

#[test]
fn dml_in_cte_is_write() {
    assert_eq!(k("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d"), StatementKind::Write);
}

#[test]
fn select_into_is_write() {
    assert_eq!(k("SELECT * INTO newt FROM t"), StatementKind::Write);
}

#[test]
fn explain_analyze_is_write() {
    assert_eq!(k("EXPLAIN ANALYZE DELETE FROM t"), StatementKind::Write);
}

#[test]
fn ddl_statements() {
    assert_eq!(k("CREATE TABLE t (a int)"), StatementKind::Ddl);
    assert_eq!(k("drop table t"), StatementKind::Ddl);
    assert_eq!(k("ALTER TABLE t ADD COLUMN b int"), StatementKind::Ddl);
    assert_eq!(k("TRUNCATE t"), StatementKind::Ddl);
}

#[test]
fn multi_statement_and_garbage_are_write() {
    assert_eq!(k("SELECT 1; DELETE FROM t"), StatementKind::Write);
    assert_eq!(k("SELEC nonsense ((("), StatementKind::Write);
}

#[test]
fn level_matrix() {
    use StatementKind::*;
    assert!(!SafeLevel::Silent.requires_confirm(Write));
    assert!(SafeLevel::Alert1.requires_confirm(ReadOnly));
    assert!(!SafeLevel::Alert2.requires_confirm(ReadOnly));
    assert!(SafeLevel::Alert2.requires_confirm(Write));
    assert!(!SafeLevel::Alert1.requires_password(Write));
    assert!(SafeLevel::Safe1.requires_password(ReadOnly));
    assert!(!SafeLevel::Safe2.requires_password(ReadOnly));
    assert!(SafeLevel::Safe2.requires_password(Ddl));
    assert!(SafeLevel::Safe2.requires_confirm(Ddl));
}
