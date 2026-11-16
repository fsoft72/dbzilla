//! Safe mode: statement classification and the level matrix (SPEC 9.3).

use crate::dialect::DriverId;
use crate::splitter::split_statements;
use serde::{Deserialize, Serialize};
use sqlparser::ast::{Query, SetExpr, Statement};
use sqlparser::dialect::{Dialect as SqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

/// Kind of a SQL statement for safe-mode purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    ReadOnly,
    Write,
    Ddl,
}

/// Per-connection safe mode level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SafeLevel {
    #[default]
    Silent,
    Alert1,
    Alert2,
    Safe1,
    Safe2,
}

impl SafeLevel {
    /// True when the statements must be shown and confirmed before sending.
    pub fn requires_confirm(self, kind: StatementKind) -> bool {
        match self {
            SafeLevel::Silent => false,
            SafeLevel::Alert1 | SafeLevel::Safe1 => true,
            SafeLevel::Alert2 | SafeLevel::Safe2 => kind != StatementKind::ReadOnly,
        }
    }

    /// True when the database password is required before sending.
    pub fn requires_password(self, kind: StatementKind) -> bool {
        match self {
            SafeLevel::Safe1 => true,
            SafeLevel::Safe2 => kind != StatementKind::ReadOnly,
            _ => false,
        }
    }
}

/// First keyword of a statement, upper-cased, skipping comments and parentheses.
fn first_keyword(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix('(') {
            rest = r.trim_start();
        } else {
            break;
        }
    }
    rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase()
}

/// Maps a driver to its `sqlparser` dialect.
fn parser_dialect(d: DriverId) -> Box<dyn SqlDialect> {
    match d {
        DriverId::Sqlite => Box::new(SQLiteDialect {}),
        DriverId::Postgres => Box::new(PostgreSqlDialect {}),
        DriverId::MySql => Box::new(MySqlDialect {}),
    }
}

/// True when a query (including CTEs) cannot modify data or create objects.
fn query_is_read_only(q: &Query) -> bool {
    let ctes_ok = q.with.as_ref().is_none_or(|w| w.cte_tables.iter().all(|c| query_is_read_only(&c.query)));
    ctes_ok && set_expr_is_read_only(&q.body)
}

/// True when a set expression is a pure read.
fn set_expr_is_read_only(e: &SetExpr) -> bool {
    match e {
        SetExpr::Select(s) => s.into.is_none(),
        SetExpr::Query(q) => query_is_read_only(q),
        SetExpr::SetOperation { left, right, .. } => set_expr_is_read_only(left) && set_expr_is_read_only(right),
        SetExpr::Values(_) | SetExpr::Table(_) => true,
        _ => false,
    }
}

/// Classifies SQL for safe mode. Multi-statement or unparsable input is `Write`.
pub fn classify(sql: &str, dialect: DriverId) -> StatementKind {
    let stmts = split_statements(sql, dialect);
    if stmts.is_empty() {
        return StatementKind::ReadOnly;
    }
    if stmts.len() > 1 {
        let strictest = stmts.iter().map(|s| classify_one(&s.text, dialect)).max_by_key(|k| severity(*k)).unwrap_or(StatementKind::Write);
        return if strictest == StatementKind::ReadOnly { StatementKind::Write } else { strictest };
    }
    classify_one(stmts[0].text.as_str(), dialect)
}

/// Introspection pragmas that take an argument but never change anything.
const READ_PRAGMAS: [&str; 10] = ["table_info", "table_xinfo", "table_list", "index_list", "index_info", "index_xinfo", "foreign_key_list", "database_list", "collation_list", "function_list"];

/// True for `PRAGMA name` without arguments and for the introspection pragmas.
fn pragma_is_read_only(text: &str) -> bool {
    if text.contains('=') {
        return false;
    }
    let rest = text.trim_start()[6..].trim_start();
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.').collect();
    let name = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    !rest.contains('(') || READ_PRAGMAS.contains(&name.as_str())
}

/// Order of kinds, most dangerous last.
fn severity(k: StatementKind) -> u8 {
    match k {
        StatementKind::ReadOnly => 0,
        StatementKind::Write => 1,
        StatementKind::Ddl => 2,
    }
}

/// Classifies one statement.
fn classify_one(text: &str, dialect: DriverId) -> StatementKind {
    match first_keyword(text).as_str() {
        "SHOW" | "DESCRIBE" | "DESC" => StatementKind::ReadOnly,
        "PRAGMA" => {
            if pragma_is_read_only(text) { StatementKind::ReadOnly } else { StatementKind::Write }
        }
        "EXPLAIN" => {
            let upper = text.to_ascii_uppercase();
            if upper.contains("ANALYZE") || upper.contains("ANALYSE") { StatementKind::Write } else { StatementKind::ReadOnly }
        }
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" | "GRANT" | "REVOKE" | "DO" | "CALL" | "COPY" | "ATTACH" | "DETACH" => StatementKind::Ddl,
        "SELECT" | "WITH" | "VALUES" | "TABLE" => classify_query(text, dialect),
        _ => StatementKind::Write,
    }
}

/// Parses a query-like statement and decides whether it is read-only.
fn classify_query(text: &str, dialect: DriverId) -> StatementKind {
    let Ok(parsed) = Parser::parse_sql(parser_dialect(dialect).as_ref(), text) else {
        return StatementKind::Write;
    };
    match parsed.as_slice() {
        [Statement::Query(q)] if query_is_read_only(q) => StatementKind::ReadOnly,
        _ => StatementKind::Write,
    }
}
