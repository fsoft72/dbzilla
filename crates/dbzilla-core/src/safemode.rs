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
        return StatementKind::Write;
    }
    let text = stmts[0].text.as_str();
    match first_keyword(text).as_str() {
        "SHOW" | "DESCRIBE" | "DESC" => StatementKind::ReadOnly,
        "PRAGMA" => {
            if text.contains('=') { StatementKind::Write } else { StatementKind::ReadOnly }
        }
        "EXPLAIN" => {
            let upper = text.to_ascii_uppercase();
            if upper.contains("ANALYZE") || upper.contains("ANALYSE") { StatementKind::Write } else { StatementKind::ReadOnly }
        }
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" | "GRANT" | "REVOKE" => StatementKind::Ddl,
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
