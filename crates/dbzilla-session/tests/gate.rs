//! Safe mode gate and console redaction tests (SPEC 9.3, 7.5, NFR-05).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::safemode::SafeLevel;
use dbzilla_session::console::{display_for_console, redact_sql};
use dbzilla_session::gate::{gate_check, verify_password, GateDecision};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

#[test]
fn silent_always_allows() {
    assert_eq!(gate_check(SafeLevel::Silent, DriverId::Postgres, &s(&["DELETE FROM t"])), GateDecision::Allow);
}

#[test]
fn alert_levels() {
    let sel = s(&["SELECT 1"]);
    let del = s(&["DELETE FROM t"]);
    assert_eq!(gate_check(SafeLevel::Alert1, DriverId::Postgres, &sel), GateDecision::Confirm(sel.clone()));
    assert_eq!(gate_check(SafeLevel::Alert2, DriverId::Postgres, &sel), GateDecision::Allow);
    assert_eq!(gate_check(SafeLevel::Alert2, DriverId::Postgres, &del), GateDecision::Confirm(del));
}

#[test]
fn safe_levels_need_the_password() {
    let sel = s(&["SELECT 1"]);
    let ddl = s(&["DROP TABLE t"]);
    assert_eq!(gate_check(SafeLevel::Safe1, DriverId::MySql, &sel), GateDecision::Password(sel.clone()));
    assert_eq!(gate_check(SafeLevel::Safe2, DriverId::MySql, &sel), GateDecision::Allow);
    assert_eq!(gate_check(SafeLevel::Safe2, DriverId::MySql, &ddl), GateDecision::Password(ddl));
}

#[test]
fn the_strictest_statement_decides() {
    let mixed = s(&["SELECT 1", "UPDATE t SET a = 1"]);
    assert_eq!(gate_check(SafeLevel::Safe2, DriverId::Sqlite, &mixed), GateDecision::Password(mixed.clone()));
    assert_eq!(gate_check(SafeLevel::Alert2, DriverId::Sqlite, &mixed), GateDecision::Confirm(mixed));
    assert_eq!(gate_check(SafeLevel::Safe1, DriverId::Sqlite, &[]), GateDecision::Allow);
}

#[test]
fn password_verification() {
    assert!(verify_password("s3cret", "s3cret"));
    assert!(!verify_password("s3cret", "s3creT"));
    assert!(!verify_password("s3cret", "s3cret "));
    assert!(!verify_password("s3cret", ""));
    assert!(!verify_password("", ""), "an empty stored password never unlocks");
}

#[test]
fn redaction_hides_password_literals() {
    assert_eq!(redact_sql("CREATE ROLE bob WITH LOGIN PASSWORD 's3cret'"), "CREATE ROLE bob WITH LOGIN PASSWORD '***'");
    assert_eq!(redact_sql("ALTER USER 'a'@'%' IDENTIFIED BY 'x''y'"), "ALTER USER 'a'@'%' IDENTIFIED BY '***'");
    assert_eq!(redact_sql("SET PASSWORD FOR 'u'@'h' = PASSWORD('abc')"), "SET PASSWORD FOR 'u'@'h' = PASSWORD('***')");
    assert_eq!(redact_sql("alter user x password = 'p\\'q'"), "alter user x password = '***'");
}

#[test]
fn redaction_leaves_other_statements_alone() {
    for sql in ["SELECT password FROM users", "SELECT 'my password is ''x'''", "SELECT 1"] {
        assert_eq!(redact_sql(sql), sql);
    }
}

#[test]
fn huge_statements_are_replaced_by_a_note() {
    let big = format!("INSERT INTO t VALUES ('{}')", "x".repeat(20_000));
    let shown = display_for_console(&big);
    assert!(shown.starts_with("-- statement of "), "{shown}");
    assert!(shown.len() < 100);
    assert_eq!(display_for_console("SELECT 1"), "SELECT 1");
}
