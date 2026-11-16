//! Pending row changes and their SQL generation (SPEC 9.1). Pure, no I/O.

use crate::dialect::Dialect;
use crate::model::TableRef;
use crate::value::Value;
use thiserror::Error;

/// A cell value being written.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Value(Value),
    Null,
    /// The column default.
    Default,
    /// A raw SQL expression such as `NOW()`.
    Expr(String),
}

impl From<Value> for CellValue {
    /// `Value::Null` maps to `CellValue::Null`, everything else to `CellValue::Value`.
    fn from(v: Value) -> Self {
        if v.is_null() { CellValue::Null } else { CellValue::Value(v) }
    }
}

/// One pending change.
#[derive(Debug, Clone, PartialEq)]
pub enum RowChange {
    Insert { values: Vec<(String, CellValue)> },
    Update { key: Vec<(String, Value)>, set: Vec<(String, CellValue)> },
    Delete { key: Vec<(String, Value)> },
}

/// Errors raised while generating SQL.
#[derive(Debug, Error, PartialEq)]
pub enum ChangeError {
    #[error("cannot update or delete rows of {table}: the table has no primary or unique key selected")]
    NoKey { table: String },
    #[error("setting column {column} to DEFAULT is not supported by this database")]
    DefaultNotSupported { column: String },
}

/// Undoable list of pending changes for one table tab.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    pub table: TableRef,
    pub rows: Vec<RowChange>,
    redo: Vec<RowChange>,
}

impl ChangeSet {
    /// An empty change set for `table`.
    pub fn new(table: TableRef) -> Self {
        Self { table, rows: Vec::new(), redo: Vec::new() }
    }

    /// Records a change and clears the redo stack.
    pub fn push(&mut self, change: RowChange) {
        self.rows.push(change);
        self.redo.clear();
    }

    /// Undoes the last change; returns it.
    pub fn undo(&mut self) -> Option<&RowChange> {
        let change = self.rows.pop()?;
        self.redo.push(change);
        self.redo.last()
    }

    /// Re-applies the last undone change; returns it.
    pub fn redo(&mut self) -> Option<&RowChange> {
        let change = self.redo.pop()?;
        self.rows.push(change);
        self.rows.last()
    }

    /// True when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Drops everything, including the undo history.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.redo.clear();
    }
}

/// Renders a cell for use in SQL; `Default` must be handled by the caller.
fn cell_sql(d: &dyn Dialect, c: &CellValue) -> String {
    match c {
        CellValue::Value(v) => d.literal(v),
        CellValue::Null => "NULL".to_owned(),
        CellValue::Default => "DEFAULT".to_owned(),
        CellValue::Expr(e) => e.clone(),
    }
}

/// `WHERE` clause body for a key.
fn where_key(d: &dyn Dialect, key: &[(String, Value)]) -> String {
    key.iter()
        .map(|(c, v)| {
            let col = d.quote_ident(c);
            if v.is_null() { format!("{col} IS NULL") } else { format!("{col} = {}", d.literal(v)) }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Merges consecutive updates of the same key (later values win).
fn merge_updates(rows: &[RowChange]) -> Vec<RowChange> {
    let mut out: Vec<RowChange> = Vec::new();
    for r in rows {
        if let (RowChange::Update { key: k2, set: s2 }, Some(RowChange::Update { key: k1, set: s1 })) = (r, out.last_mut()) {
            if k1 == k2 {
                for (col, val) in s2 {
                    match s1.iter_mut().find(|(c, _)| c == col) {
                        Some(slot) => slot.1 = val.clone(),
                        None => s1.push((col.clone(), val.clone())),
                    }
                }
                continue;
            }
        }
        out.push(r.clone());
    }
    out
}

/// Generates the statements for a change set, in order.
pub fn generate_sql(cs: &ChangeSet, d: &dyn Dialect) -> Result<Vec<String>, ChangeError> {
    let table = d.quote_table(&cs.table);
    let mut out = Vec::new();
    for change in merge_updates(&cs.rows) {
        match change {
            RowChange::Insert { values } => {
                let given: Vec<&(String, CellValue)> = values.iter().filter(|(_, v)| *v != CellValue::Default).collect();
                if given.is_empty() {
                    out.push(d.insert_defaults(&table));
                    continue;
                }
                let cols: Vec<String> = given.iter().map(|(c, _)| d.quote_ident(c)).collect();
                let vals: Vec<String> = given.iter().map(|(_, v)| cell_sql(d, v)).collect();
                out.push(format!("INSERT INTO {table} ({}) VALUES ({})", cols.join(", "), vals.join(", ")));
            }
            RowChange::Update { key, set } => {
                if key.is_empty() {
                    return Err(ChangeError::NoKey { table });
                }
                if set.is_empty() {
                    continue;
                }
                let mut assigns = Vec::new();
                for (c, v) in &set {
                    if *v == CellValue::Default && !d.supports_default_keyword() {
                        return Err(ChangeError::DefaultNotSupported { column: c.clone() });
                    }
                    assigns.push(format!("{} = {}", d.quote_ident(c), cell_sql(d, v)));
                }
                out.push(format!("UPDATE {table} SET {} WHERE {}", assigns.join(", "), where_key(d, &key)));
            }
            RowChange::Delete { key } => {
                if key.is_empty() {
                    return Err(ChangeError::NoKey { table });
                }
                out.push(format!("DELETE FROM {table} WHERE {}", where_key(d, &key)));
            }
        }
    }
    Ok(out)
}
