//! Parsing and importing CSV, JSON and SQL files (SPEC 12.1).

use crate::convert::to_value;
use crate::{IoError, Progress, ProgressFn, BATCH_ROWS};
use dbzilla_core::ddl::{generate_ddl, ColumnDef, StructuralOp};
use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::model::TableRef;
use dbzilla_core::splitter::split_statements;
use dbzilla_core::value::Value;
use dbzilla_drivers::Conn;
use std::sync::atomic::{AtomicBool, Ordering};

/// Rows shown in the wizard preview.
pub const PREVIEW_ROWS: usize = 100;
/// Characters shown when previewing an SQL dump.
pub const SQL_PREVIEW_CHARS: usize = 100_000;

/// A parsed file: header and rows (`None` is NULL).
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// CSV reading options.
#[derive(Debug, Clone, PartialEq)]
pub struct CsvImportOptions {
    pub delimiter: char,
    pub quote: char,
    pub header: bool,
    /// Encoding label such as `utf-8` or `windows-1252`.
    pub encoding: String,
    /// Cells equal to this text become NULL.
    pub null_text: Option<String>,
}

impl Default for CsvImportOptions {
    fn default() -> Self {
        Self { delimiter: ',', quote: '"', header: true, encoding: "utf-8".into(), null_text: Some("NULL".into()) }
    }
}

/// Decodes bytes with an encoding label; invalid input is an error, not replacement characters.
pub fn decode_text(bytes: &[u8], encoding: &str) -> Result<String, IoError> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let label = encoding.trim().to_lowercase();
    if label.is_empty() || label == "utf-8" || label == "utf8" {
        return String::from_utf8(bytes.to_vec()).map_err(|_| IoError::Format("the file is not valid UTF-8; pick another encoding".into()));
    }
    let enc = encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| IoError::Format(format!("unknown encoding '{encoding}'")))?;
    enc.decode_without_bom_handling_and_without_replacement(bytes).map(|c| c.into_owned()).ok_or_else(|| IoError::Format(format!("the file is not valid {encoding}")))
}

/// Splits CSV text into records of `(text, was_quoted)`; a quoted field is never a NULL marker.
fn tokenize_csv(text: &str, delimiter: char, quote: char) -> Result<Vec<Vec<(String, bool)>>, IoError> {
    let mut records: Vec<Vec<(String, bool)>> = Vec::new();
    let mut record: Vec<(String, bool)> = Vec::new();
    let (mut field, mut quoted, mut in_quotes, mut started) = (String::new(), false, false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == quote {
                if chars.peek() == Some(&quote) {
                    field.push(quote);
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            c if c == quote && field.is_empty() && !quoted => {
                in_quotes = true;
                quoted = true;
                started = true;
            }
            c if c == delimiter => {
                record.push((std::mem::take(&mut field), quoted));
                quoted = false;
                started = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if started || !field.is_empty() || quoted {
                    record.push((std::mem::take(&mut field), quoted));
                    records.push(std::mem::take(&mut record));
                }
                quoted = false;
                started = false;
            }
            c => {
                field.push(c);
                started = true;
            }
        }
    }
    if in_quotes {
        return Err(IoError::Format("the file ends inside a quoted value".into()));
    }
    if started || !field.is_empty() || quoted {
        record.push((field, quoted));
        records.push(record);
    }
    Ok(records)
}

/// Parses CSV text.
pub fn parse_csv(bytes: &[u8], o: &CsvImportOptions) -> Result<Parsed, IoError> {
    let text = decode_text(bytes, &o.encoding)?;
    let records = tokenize_csv(&text, o.delimiter, o.quote)?;
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for (n, rec) in records.into_iter().enumerate() {
        if n == 0 {
            if o.header {
                headers = rec.into_iter().map(|(t, _)| t).collect();
                continue;
            }
            headers = (1..=rec.len()).map(|i| format!("column{i}")).collect();
        }
        if rec.len() > headers.len() {
            return Err(IoError::Format(format!("row {} has {} cells but there are only {} columns", n + 1, rec.len(), headers.len())));
        }
        let mut cells: Vec<Option<String>> = rec.into_iter().map(|(t, quoted)| if !quoted && o.null_text.as_deref() == Some(t.as_str()) { None } else { Some(t) }).collect();
        cells.resize(headers.len(), None);
        rows.push(cells);
    }
    Ok(Parsed { headers, rows })
}

/// Parses a JSON array of objects.
pub fn parse_json(bytes: &[u8]) -> Result<Parsed, IoError> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| IoError::Format(format!("invalid JSON: {e}")))?;
    let items = v.as_array().ok_or_else(|| IoError::Format("the JSON file must contain an array of objects".into()))?;
    let mut headers: Vec<String> = Vec::new();
    for item in items {
        let obj = item.as_object().ok_or_else(|| IoError::Format("every element of the JSON array must be an object".into()))?;
        for k in obj.keys() {
            if !headers.contains(k) {
                headers.push(k.clone());
            }
        }
    }
    let rows = items
        .iter()
        .map(|item| {
            headers
                .iter()
                .map(|h| match item.get(h) {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(other) => Some(other.to_string()),
                })
                .collect()
        })
        .collect();
    Ok(Parsed { headers, rows })
}

/// The first rows of a parsed file.
pub fn preview(p: &Parsed) -> Parsed {
    Parsed { headers: p.headers.clone(), rows: p.rows.iter().take(PREVIEW_ROWS).cloned().collect() }
}

/// How source columns are matched to destination columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStrategy {
    NameAndOrder,
    Name,
    Order,
    NameCaseInsensitive,
}

/// For each source column, the index of the matching destination column.
pub fn match_columns(strategy: MatchStrategy, source: &[String], dest: &[String]) -> Vec<Option<usize>> {
    let by_name = |s: &str, ci: bool| dest.iter().position(|d| if ci { d.eq_ignore_ascii_case(s) } else { d == s });
    match strategy {
        MatchStrategy::Name => source.iter().map(|s| by_name(s, false)).collect(),
        MatchStrategy::NameCaseInsensitive => source.iter().map(|s| by_name(s, true)).collect(),
        MatchStrategy::Order => (0..source.len()).map(|i| (i < dest.len()).then_some(i)).collect(),
        MatchStrategy::NameAndOrder => {
            let mut out: Vec<Option<usize>> = source.iter().map(|s| by_name(s, false)).collect();
            let mut free: Vec<usize> = (0..dest.len()).filter(|d| !out.contains(&Some(*d))).collect();
            free.reverse();
            for slot in out.iter_mut().filter(|o| o.is_none()) {
                *slot = free.pop();
            }
            out
        }
    }
}

/// Column type guessed from the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedType {
    Integer,
    Real,
    Text,
}

impl DetectedType {
    /// SQL type for a driver.
    pub fn sql_type(self, driver: DriverId) -> &'static str {
        match (self, driver) {
            (DetectedType::Integer, DriverId::Sqlite) => "INTEGER",
            (DetectedType::Integer, _) => "bigint",
            (DetectedType::Real, DriverId::Sqlite) => "REAL",
            (DetectedType::Real, DriverId::Postgres) => "double precision",
            (DetectedType::Real, DriverId::MySql) => "double",
            (DetectedType::Text, DriverId::Sqlite) => "TEXT",
            (DetectedType::Text, _) => "text",
        }
    }
}

/// True for numbers whose text would change when stored as a number (leading zeros).
fn keeps_text(s: &str) -> bool {
    s.len() > 1 && s.starts_with('0') && !s.starts_with("0.") || s.starts_with("+")
}

/// Guesses a type per column: integer, real or text.
pub fn detect_types(p: &Parsed) -> Vec<DetectedType> {
    (0..p.headers.len())
        .map(|c| {
            let cells: Vec<&str> = p.rows.iter().filter_map(|r| r.get(c).and_then(|v| v.as_deref())).map(str::trim).filter(|s| !s.is_empty()).collect();
            if cells.is_empty() || cells.iter().any(|s| keeps_text(s)) {
                DetectedType::Text
            } else if cells.iter().all(|s| s.parse::<i64>().is_ok()) {
                DetectedType::Integer
            } else if cells.iter().all(|s| s.parse::<f64>().is_ok_and(f64::is_finite)) {
                DetectedType::Real
            } else {
                DetectedType::Text
            }
        })
        .collect()
}

/// What to do with rows whose key already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMode {
    StopOnError,
    UpdateOnDuplicate,
    Replace,
}

/// A complete import definition.
#[derive(Debug, Clone)]
pub struct ImportPlan {
    pub table: TableRef,
    /// Columns to create first `(name, type)`.
    pub create: Option<Vec<(String, String)>>,
    /// For each source column: the destination `(column, type)`, or `None` to skip it.
    pub mapping: Vec<Option<(String, String)>>,
    pub mode: InsertMode,
    /// Key columns used by "update on duplicate" (PostgreSQL and SQLite).
    pub key_columns: Vec<String>,
}

/// Outcome of an import.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportSummary {
    pub rows: u64,
}

/// Statement starting a transaction.
fn begin_sql(driver: DriverId) -> &'static str {
    if driver == DriverId::MySql { "START TRANSACTION" } else { "BEGIN" }
}

/// Multi-row INSERT for a batch.
fn insert_sql(driver: DriverId, plan: &ImportPlan, cols: &[String], rows: &[Vec<Value>]) -> Result<String, IoError> {
    let d = dialect_for(driver);
    let head = match (plan.mode, driver) {
        (InsertMode::Replace, DriverId::MySql) => "REPLACE INTO",
        (InsertMode::Replace, DriverId::Sqlite) => "INSERT OR REPLACE INTO",
        (InsertMode::Replace, DriverId::Postgres) => return Err(IoError::Unsupported("REPLACE is not available on PostgreSQL; use update on duplicate".into())),
        _ => "INSERT INTO",
    };
    let names = cols.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
    let values = rows.iter().map(|r| format!("({})", r.iter().map(|v| d.literal(v)).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join(", ");
    let mut sql = format!("{head} {} ({names}) VALUES {values}", d.quote_table(&plan.table));
    if plan.mode == InsertMode::UpdateOnDuplicate {
        let updates: Vec<&String> = cols.iter().filter(|c| !plan.key_columns.contains(c)).collect();
        match driver {
            DriverId::MySql => {
                let set = if updates.is_empty() { format!("{0} = {0}", d.quote_ident(&cols[0])) } else { updates.iter().map(|c| format!("{0} = VALUES({0})", d.quote_ident(c))).collect::<Vec<_>>().join(", ") };
                sql.push_str(&format!(" ON DUPLICATE KEY UPDATE {set}"));
            }
            _ => {
                if plan.key_columns.is_empty() {
                    return Err(IoError::Format("choose the key columns used to detect duplicates".into()));
                }
                let keys = plan.key_columns.iter().map(|k| d.quote_ident(k)).collect::<Vec<_>>().join(", ");
                if updates.is_empty() {
                    sql.push_str(&format!(" ON CONFLICT ({keys}) DO NOTHING"));
                } else {
                    let set = updates.iter().map(|c| format!("{0} = EXCLUDED.{0}", d.quote_ident(c))).collect::<Vec<_>>().join(", ");
                    sql.push_str(&format!(" ON CONFLICT ({keys}) DO UPDATE SET {set}"));
                }
            }
        }
    }
    Ok(sql)
}

/// Converts rows `[start, end)` using the mapping; errors name the row and column.
fn convert_batch(plan: &ImportPlan, parsed: &Parsed, start: usize, end: usize) -> Result<Vec<Vec<Value>>, IoError> {
    let mut out = Vec::with_capacity(end - start);
    for (n, row) in parsed.rows[start..end].iter().enumerate() {
        let mut values = Vec::new();
        for (src, map) in plan.mapping.iter().enumerate() {
            let Some((col, ty)) = map else { continue };
            let cell = row.get(src).and_then(|c| c.as_deref());
            values.push(to_value(ty, cell).map_err(|e| IoError::Format(format!("row {} (column {col}): {e}", start + n + 1)))?);
        }
        out.push(values);
    }
    Ok(out)
}

/// Creates the table when needed and inserts the rows in batches inside one transaction.
pub async fn run_import(conn: &mut dyn Conn, driver: DriverId, plan: &ImportPlan, parsed: &Parsed, progress: ProgressFn<'_>, cancel: &AtomicBool) -> Result<ImportSummary, IoError> {
    let d = dialect_for(driver);
    let dest: Vec<String> = plan.mapping.iter().flatten().map(|(c, _)| c.clone()).collect();
    if dest.is_empty() {
        return Err(IoError::Format("no column is mapped to import".into()));
    }
    conn.execute_batch(&[begin_sql(driver).to_owned()], false).await?;
    let mut created = false;
    let total = parsed.rows.len();
    let mut done = 0usize;
    let result: Result<(), IoError> = async {
        if let Some(cols) = &plan.create {
            let columns = cols.iter().map(|(n, t)| ColumnDef { name: n.clone(), type_name: t.clone(), nullable: true, default: None, extra: None }).collect();
            let ops = StructuralOp::CreateTable { table: plan.table.clone(), columns, primary_key: vec![], foreign_keys: vec![] };
            let stmts = generate_ddl(&ops, d.as_ref()).map_err(|e| IoError::Format(e.to_string()))?;
            conn.execute_batch(&stmts, false).await?;
            created = true;
        }
        while done < total {
            if cancel.load(Ordering::SeqCst) {
                return Err(IoError::Cancelled);
            }
            let end = (done + BATCH_ROWS).min(total);
            let rows = convert_batch(plan, parsed, done, end)?;
            let sql = insert_sql(driver, plan, &dest, &rows)?;
            conn.execute_batch(&[sql], false).await?;
            done = end;
            progress(Progress { item: plan.table.name.clone(), rows: done as u64, done_items: 0, total_items: 1 });
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            conn.execute_batch(&["COMMIT".to_owned()], false).await?;
            Ok(ImportSummary { rows: total as u64 })
        }
        Err(e) => {
            let _ = conn.execute_batch(&["ROLLBACK".to_owned()], false).await;
            if created {
                // MySQL commits DDL implicitly, so the new table may have survived the rollback
                let _ = conn.execute_batch(&[format!("DROP TABLE IF EXISTS {}", d.quote_table(&plan.table))], false).await;
            }
            Err(e)
        }
    }
}

/// Runs the statements of an SQL dump one after another; returns how many ran.
pub async fn run_sql_dump(conn: &mut dyn Conn, driver: DriverId, text: &str, progress: ProgressFn<'_>, cancel: &AtomicBool) -> Result<u64, IoError> {
    let statements = split_statements(text, driver);
    let total = statements.len();
    for (i, s) in statements.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(IoError::Cancelled);
        }
        conn.execute_batch(std::slice::from_ref(&s.text), false).await.map_err(|e| IoError::Format(format!("statement {} failed: {e}", i + 1)))?;
        progress(Progress { item: "SQL dump".into(), rows: (i + 1) as u64, done_items: i + 1, total_items: total });
    }
    Ok(total as u64)
}
