//! Dialect objects: quoting, literals, paging and type/keyword lists per driver.

use crate::model::TableRef;
use crate::value::Value;
use serde::{Deserialize, Serialize};

/// Supported driver identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DriverId {
    Sqlite,
    Postgres,
    MySql,
    SqlServer,
}

/// SQL dialect behaviour. UI code must go through this trait, never match on `DriverId`.
pub trait Dialect: Send + Sync {
    /// Driver this dialect belongs to.
    fn id(&self) -> DriverId;
    /// Quotes one identifier, doubling embedded quote characters.
    fn quote_ident(&self, name: &str) -> String;
    /// Renders a value as a SQL literal.
    fn literal(&self, v: &Value) -> String;
    /// `LIMIT n OFFSET m` clause.
    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        format!("LIMIT {limit} OFFSET {offset}")
    }
    /// Paging clause of the data tab. `has_order_by` is true when the statement already sorts, for the
    /// dialects (SQL Server) where paging is a syntax error without an `ORDER BY`.
    fn page_clause(&self, limit: u64, offset: u64, _has_order_by: bool) -> String {
        self.limit_offset(limit, offset)
    }
    /// Clause that caps the rows of a typed `SELECT`, appended after the statement.
    fn row_limit_clause(&self, n: u64, _has_order_by: bool) -> String {
        format!("LIMIT {n}")
    }
    /// Words that, at the top level of a `SELECT`, mean it already limits itself (besides `LIMIT`, `TOP` and `FETCH`).
    fn extra_limit_blockers(&self) -> &'static [&'static str] {
        &[]
    }
    /// The "first n rows" script of Copy Script As.
    fn select_top(&self, quoted_table: &str, n: u64) -> String {
        format!("SELECT * FROM {quoted_table} LIMIT {n};")
    }
    /// Data types offered in the structure editor.
    fn types(&self) -> &'static [&'static str];
    /// Keywords for highlight, completion and formatting.
    fn keywords(&self) -> &'static [&'static str];

    /// Expression for the current timestamp.
    fn now_expr(&self) -> &'static str {
        "NOW()"
    }
    /// Casts an SQL expression to text.
    fn cast_text(&self, expr: &str) -> String;
    /// Operand used by text matching operators (PostgreSQL needs an explicit cast).
    fn text_operand(&self, expr: &str) -> String {
        expr.to_owned()
    }
    /// Statement inserting a row made only of column defaults.
    fn insert_defaults(&self, quoted_table: &str) -> String {
        format!("INSERT INTO {quoted_table} DEFAULT VALUES")
    }
    /// True when `SET col = DEFAULT` is valid.
    fn supports_default_keyword(&self) -> bool {
        true
    }

    /// Quotes a table reference, skipping missing parts.
    fn quote_table(&self, t: &TableRef) -> String {
        let mut parts = Vec::new();
        if let Some(d) = &t.database {
            parts.push(self.quote_ident(d));
        }
        if let Some(s) = &t.schema {
            parts.push(self.quote_ident(s));
        }
        parts.push(self.quote_ident(&t.name));
        parts.join(".")
    }
}

/// Returns the dialect object for a driver.
pub fn dialect_for(id: DriverId) -> Box<dyn Dialect> {
    match id {
        DriverId::Sqlite => Box::new(SqliteDialect),
        DriverId::Postgres => Box::new(PostgresDialect),
        DriverId::MySql => Box::new(MySqlDialect),
        DriverId::SqlServer => Box::new(MsSqlDialect),
    }
}

/// Keywords shared by all dialects.
const COMMON_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "INSERT", "INTO",
    "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "ALTER", "DROP", "INDEX", "VIEW", "JOIN",
    "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON", "AS", "AND", "OR", "NOT", "NULL", "IS",
    "IN", "BETWEEN", "LIKE", "EXISTS", "UNION", "ALL", "DISTINCT", "CASE", "WHEN", "THEN", "ELSE",
    "END", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "DEFAULT", "UNIQUE", "CHECK", "CONSTRAINT",
    "ASC", "DESC", "WITH", "BEGIN", "COMMIT", "ROLLBACK", "TRUNCATE", "EXPLAIN", "TRIGGER",
];

/// Renders bytes as upper-case hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Quotes text with single quotes, doubling them; optionally escaping backslashes.
fn quote_text(s: &str, escape_backslash: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' if escape_backslash => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// True when the decimal text is a plain number and can be emitted unquoted.
fn is_plain_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
}

/// Shared literal rendering; `bool_fn`, `bytes_fn` and backslash handling differ per dialect.
fn literal_common(
    v: &Value,
    escape_backslash: bool,
    bool_fn: fn(bool) -> &'static str,
    bytes_fn: fn(&[u8]) -> String,
) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => bool_fn(*b).into(),
        Value::I64(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F64(f) if f.is_finite() => f.to_string(),
        Value::F64(_) => "NULL".into(),
        Value::Decimal(s) if is_plain_number(s) => s.clone(),
        Value::Decimal(s) => quote_text(s, escape_backslash),
        Value::Text(s) | Value::Geometry(s) | Value::Other(s) => quote_text(s, escape_backslash),
        Value::Bytes(b) => bytes_fn(b),
        Value::Json(j) => quote_text(&j.to_string(), escape_backslash),
        Value::Date(d) => quote_text(&d.to_string(), false),
        Value::Time(t) => quote_text(&t.to_string(), false),
        Value::DateTime(d) => quote_text(&d.format("%Y-%m-%d %H:%M:%S%.f").to_string(), false),
        Value::DateTimeTz(d) => quote_text(&d.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string(), false),
        Value::Uuid(u) => quote_text(&u.to_string(), false),
        Value::Array(items) => {
            let json: Vec<serde_json::Value> = items.iter().map(array_item_json).collect();
            quote_text(&serde_json::Value::Array(json).to_string(), escape_backslash)
        }
    }
}

/// PostgreSQL array text such as `{1,"a b",NULL}`.
fn pg_array_text(items: &[Value]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|v| match v {
            Value::Null => "NULL".to_owned(),
            Value::Array(inner) => pg_array_text(inner),
            Value::Bool(b) => if *b { "t" } else { "f" }.to_owned(),
            Value::I64(_) | Value::U64(_) | Value::F64(_) | Value::Decimal(_) => v.to_display(),
            other => format!("\"{}\"", other.to_display().replace('\\', "\\\\").replace('"', "\\\"")),
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// Converts an array element to JSON for dialects without native array literals.
fn array_item_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::I64(n) => (*n).into(),
        Value::U64(n) => (*n).into(),
        Value::F64(f) => serde_json::Number::from_f64(*f).map_or(serde_json::Value::Null, Into::into),
        Value::Json(j) => j.clone(),
        other => other.to_display().into(),
    }
}

/// SQLite dialect.
pub struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn id(&self) -> DriverId {
        DriverId::Sqlite
    }
    fn quote_ident(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
    fn literal(&self, v: &Value) -> String {
        literal_common(v, false, |b| if b { "1" } else { "0" }, |b| format!("X'{}'", hex(b)))
    }
    fn cast_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS TEXT)")
    }
    fn now_expr(&self) -> &'static str {
        "CURRENT_TIMESTAMP"
    }
    fn supports_default_keyword(&self) -> bool {
        false
    }
    fn types(&self) -> &'static [&'static str] {
        &["INTEGER", "TEXT", "REAL", "BLOB", "NUMERIC", "BOOLEAN", "DATE", "DATETIME", "VARCHAR", "JSON"]
    }
    fn keywords(&self) -> &'static [&'static str] {
        COMMON_KEYWORDS
    }
}

/// PostgreSQL dialect.
pub struct PostgresDialect;

impl Dialect for PostgresDialect {
    fn id(&self) -> DriverId {
        DriverId::Postgres
    }
    fn quote_ident(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
    fn literal(&self, v: &Value) -> String {
        if let Value::Array(items) = v {
            return quote_text(&pg_array_text(items), false);
        }
        literal_common(v, false, |b| if b { "TRUE" } else { "FALSE" }, |b| format!("'\\x{}'", hex(b)))
    }
    fn cast_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS text)")
    }
    fn text_operand(&self, expr: &str) -> String {
        self.cast_text(expr)
    }
    fn types(&self) -> &'static [&'static str] {
        &[
            "integer", "bigint", "smallint", "serial", "bigserial", "numeric", "real", "double precision",
            "boolean", "text", "varchar", "char", "bytea", "date", "time", "timestamp", "timestamptz",
            "uuid", "json", "jsonb", "inet",
        ]
    }
    fn keywords(&self) -> &'static [&'static str] {
        COMMON_KEYWORDS
    }
}

/// MySQL / MariaDB dialect.
pub struct MySqlDialect;

impl Dialect for MySqlDialect {
    fn id(&self) -> DriverId {
        DriverId::MySql
    }
    fn quote_ident(&self, name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }
    fn literal(&self, v: &Value) -> String {
        literal_common(v, true, |b| if b { "TRUE" } else { "FALSE" }, |b| format!("X'{}'", hex(b)))
    }
    fn cast_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS CHAR)")
    }
    fn insert_defaults(&self, quoted_table: &str) -> String {
        format!("INSERT INTO {quoted_table} () VALUES ()")
    }
    fn types(&self) -> &'static [&'static str] {
        &[
            "int", "bigint", "smallint", "tinyint", "decimal", "float", "double", "varchar", "char", "text",
            "longtext", "blob", "longblob", "date", "time", "datetime", "timestamp", "json", "enum", "bit",
        ]
    }
    fn keywords(&self) -> &'static [&'static str] {
        COMMON_KEYWORDS
    }
}

/// Microsoft SQL Server (T-SQL) dialect.
pub struct MsSqlDialect;

impl Dialect for MsSqlDialect {
    fn id(&self) -> DriverId {
        DriverId::SqlServer
    }
    fn quote_ident(&self, name: &str) -> String {
        format!("[{}]", name.replace(']', "]]"))
    }
    fn literal(&self, v: &Value) -> String {
        match v {
            Value::Text(s) | Value::Geometry(s) | Value::Other(s) => format!("N{}", quote_text(s, false)),
            Value::Json(j) => format!("N{}", quote_text(&j.to_string(), false)),
            other => literal_common(other, false, |b| if b { "1" } else { "0" }, |b| format!("0x{}", hex(b))),
        }
    }
    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        format!("OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    }
    fn page_clause(&self, limit: u64, offset: u64, has_order_by: bool) -> String {
        let order = if has_order_by { "" } else { "ORDER BY (SELECT NULL) " };
        format!("{order}{}", self.limit_offset(limit, offset))
    }
    fn row_limit_clause(&self, n: u64, has_order_by: bool) -> String {
        self.page_clause(n, 0, has_order_by)
    }
    fn extra_limit_blockers(&self) -> &'static [&'static str] {
        &["OFFSET"]
    }
    fn select_top(&self, quoted_table: &str, n: u64) -> String {
        format!("SELECT TOP {n} * FROM {quoted_table};")
    }
    fn now_expr(&self) -> &'static str {
        "GETDATE()"
    }
    fn cast_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS NVARCHAR(MAX))")
    }
    fn types(&self) -> &'static [&'static str] {
        &[
            "int", "bigint", "smallint", "tinyint", "bit", "decimal", "numeric", "money", "float", "real",
            "nvarchar", "varchar", "nchar", "char", "text", "ntext", "varbinary", "image", "date", "time",
            "datetime", "datetime2", "datetimeoffset", "uniqueidentifier", "xml",
        ]
    }
    fn keywords(&self) -> &'static [&'static str] {
        MSSQL_KEYWORDS
    }
}

/// Keywords of T-SQL: the common ones plus the SQL Server specific ones.
const MSSQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "ORDER", "HAVING", "TOP", "OFFSET", "FETCH", "INSERT", "INTO",
    "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "ALTER", "DROP", "INDEX", "VIEW", "JOIN",
    "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON", "AS", "AND", "OR", "NOT", "NULL", "IS",
    "IN", "BETWEEN", "LIKE", "EXISTS", "UNION", "ALL", "DISTINCT", "CASE", "WHEN", "THEN", "ELSE",
    "END", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "DEFAULT", "UNIQUE", "CHECK", "CONSTRAINT",
    "ASC", "DESC", "WITH", "BEGIN", "COMMIT", "ROLLBACK", "TRUNCATE", "TRIGGER", "EXEC", "GO",
    "IDENTITY", "NOLOCK", "OUTPUT",
];
