//! Statement splitter tests (SPEC 10.2, 10.8).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::splitter::split_statements;

fn texts(sql: &str, d: DriverId) -> Vec<String> {
    split_statements(sql, d).into_iter().map(|s| s.text).collect()
}

#[test]
fn splits_two_statements() {
    assert_eq!(texts("SELECT 1; SELECT 2;", DriverId::Sqlite), vec!["SELECT 1", "SELECT 2"]);
}

#[test]
fn last_statement_without_semicolon() {
    assert_eq!(texts("SELECT 1; SELECT 2", DriverId::Sqlite), vec!["SELECT 1", "SELECT 2"]);
}

#[test]
fn semicolon_inside_string_is_kept() {
    assert_eq!(texts("SELECT 'a;b'; SELECT 2", DriverId::Postgres), vec!["SELECT 'a;b'", "SELECT 2"]);
}

#[test]
fn doubled_quote_does_not_end_string() {
    assert_eq!(texts("SELECT 'it''s; ok'; SELECT 2", DriverId::Sqlite), vec!["SELECT 'it''s; ok'", "SELECT 2"]);
}

#[test]
fn mysql_backslash_escape_in_string() {
    assert_eq!(texts("SELECT 'it\\'s; ok'; SELECT 2", DriverId::MySql), vec!["SELECT 'it\\'s; ok'", "SELECT 2"]);
}

#[test]
fn semicolon_inside_comments_is_kept() {
    let sql = "SELECT 1 -- a;b\n; /* x; y */ SELECT 2";
    let got = texts(sql, DriverId::Sqlite);
    assert_eq!(got.len(), 2);
    assert!(got[1].ends_with("SELECT 2"));
}

#[test]
fn semicolon_inside_quoted_identifier_is_kept() {
    assert_eq!(texts("SELECT \"a;b\" FROM t; SELECT `x;y`", DriverId::MySql).len(), 2);
}

#[test]
fn postgres_dollar_quoted_body_is_one_statement() {
    let sql = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql; SELECT 2";
    let got = texts(sql, DriverId::Postgres);
    assert_eq!(got.len(), 2);
    assert!(got[0].ends_with("plpgsql"));
}

#[test]
fn postgres_tagged_dollar_quote() {
    let sql = "DO $body$ BEGIN PERFORM 1; END $body$; SELECT 2";
    assert_eq!(texts(sql, DriverId::Postgres).len(), 2);
}

#[test]
fn positional_parameter_is_not_a_dollar_quote() {
    assert_eq!(texts("SELECT $1; SELECT $2", DriverId::Postgres), vec!["SELECT $1", "SELECT $2"]);
}

#[test]
fn mysql_delimiter_block() {
    let sql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\nSELECT 3;";
    let got = texts(sql, DriverId::MySql);
    assert_eq!(got.len(), 2);
    assert!(got[0].starts_with("CREATE PROCEDURE"));
    assert!(got[0].contains("SELECT 1; SELECT 2;"));
    assert_eq!(got[1], "SELECT 3");
}

#[test]
fn label_from_comment_above() {
    let got = split_statements("-- name: Recent orders\nSELECT 1;\n-- tab: Second\nSELECT 2;\nSELECT 3;", DriverId::Sqlite);
    assert_eq!(got[0].label.as_deref(), Some("Recent orders"));
    assert_eq!(got[1].label.as_deref(), Some("Second"));
    assert_eq!(got[2].label, None);
}

#[test]
fn label_keyword_variants() {
    let got = split_statements("-- label: L\nSELECT 1", DriverId::Sqlite);
    assert_eq!(got[0].label.as_deref(), Some("L"));
}

#[test]
fn comment_only_and_empty_chunks_are_skipped() {
    assert_eq!(texts(";; -- nothing\n; SELECT 1", DriverId::Sqlite).len(), 1);
    assert!(texts("   ", DriverId::Sqlite).is_empty());
}

#[test]
fn offsets_point_into_the_source() {
    let sql = "  SELECT 1;  SELECT 2";
    let got = split_statements(sql, DriverId::Sqlite);
    assert_eq!(&sql[got[0].start..got[0].end], "SELECT 1");
    assert_eq!(&sql[got[1].start..got[1].end], "SELECT 2");
}

#[test]
fn unicode_and_unterminated_input_do_not_panic() {
    assert_eq!(texts("SELECT 'è;à'; SELECT 'open", DriverId::Postgres).len(), 2);
    assert_eq!(texts("SELECT /* open", DriverId::Postgres).len(), 1);
}

/// Review I5: MySQL executable comments are code.
#[test]
fn mysql_version_comments_are_kept() {
    use dbzilla_core::dialect::DriverId;
    let dump = "/*!40101 SET NAMES utf8mb4 */;\n/*!50003 CREATE TRIGGER x BEFORE INSERT ON t FOR EACH ROW SET NEW.a = 1 */;\nINSERT INTO t VALUES (1);\n/* plain comment */;";
    let n = dbzilla_core::splitter::split_statements(dump, DriverId::MySql).len();
    assert_eq!(n, 3, "the two executable comments and the INSERT survive, the plain comment does not");
}
