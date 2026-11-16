//! Pending row changes of a data tab: edits, inserts, deletes, undo and SQL generation (SPEC 9).

use dbzilla_core::changes::{CellValue, ChangeSet, RowChange};
use dbzilla_core::model::{ColumnMeta, Row, TableRef, TableStructure};
use dbzilla_core::value::Value;
use std::collections::BTreeMap;

/// Maximum number of undo steps kept.
const MAX_UNDO: usize = 100;
/// Text shown for a cell set to its default.
pub const DEFAULT_TEXT: &str = "DEFAULT";

/// State of a row relative to the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Clean,
    New,
    Modified,
    Deleted,
}

/// Change bookkeeping of one row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMeta {
    pub state: RowState,
    /// Values as loaded (kept for the WHERE clause of updates and deletes).
    pub original: Option<Row>,
    /// Columns written by the user, or all columns of a new row.
    pub edits: BTreeMap<usize, CellValue>,
}

impl RowMeta {
    /// A row that matches the database.
    fn clean() -> Self {
        Self { state: RowState::Clean, original: None, edits: BTreeMap::new() }
    }
}

/// Rows and metadata at one point in time.
type Snapshot = (Vec<Row>, Vec<RowMeta>);

/// Pending changes with an undo history.
#[derive(Debug, Default)]
pub struct Pending {
    pub meta: Vec<RowMeta>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

/// Why the changes cannot be turned into SQL.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingError {
    NoKey,
    Missing(String),
}

impl std::fmt::Display for PendingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PendingError::NoKey => write!(f, "This table has no primary key: choose the key columns to update or delete rows"),
            PendingError::Missing(c) => write!(f, "The key column {c} is not in the result"),
        }
    }
}

/// Text shown in the grid for a written value.
fn display_of(v: &CellValue) -> Value {
    match v {
        CellValue::Value(v) => v.clone(),
        CellValue::Null => Value::Null,
        CellValue::Default => Value::Other(DEFAULT_TEXT.into()),
        CellValue::Expr(e) => Value::Other(e.clone()),
    }
}

/// Converts typed text to a cell value following the column type (FR-D-020).
pub fn parse_edit(type_name: &str, text: &str) -> Result<CellValue, String> {
    use dbzilla_io::convert::to_value;
    let value = to_value(type_name, Some(text))?;
    // an emptied numeric or boolean cell means NULL, an emptied text cell stays an empty string
    Ok(if value == Value::Null { CellValue::Null } else { CellValue::Value(value) })
}

impl Pending {
    /// Makes the metadata as long as the rows (rows stream in after the first batch).
    pub fn sync_len(&mut self, rows: usize) {
        self.meta.resize_with(rows, RowMeta::clean);
        self.meta.truncate(rows);
    }

    /// Forgets everything (after a reload).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// True when something is waiting to be committed.
    pub fn has_changes(&self) -> bool {
        self.meta.iter().any(|m| m.state != RowState::Clean)
    }

    /// State of a row.
    pub fn state(&self, r: usize) -> RowState {
        self.meta.get(r).map_or(RowState::Clean, |m| m.state)
    }

    /// True when the cell was written by the user.
    pub fn is_edited(&self, r: usize, c: usize) -> bool {
        self.meta.get(r).is_some_and(|m| m.edits.contains_key(&c))
    }

    /// Saves the current state for undo.
    fn snapshot(&mut self, rows: &[Row]) {
        self.undo.push((rows.to_vec(), self.meta.clone()));
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Writes a value into a cell.
    pub fn edit_cell(&mut self, rows: &mut [Row], r: usize, c: usize, v: CellValue) {
        if r >= rows.len() || c >= rows[r].len() {
            return;
        }
        self.sync_len(rows.len());
        if self.meta[r].state == RowState::Deleted {
            return;
        }
        self.snapshot(rows);
        let original = self.meta[r].original.clone().unwrap_or_else(|| rows[r].clone());
        let meta = &mut self.meta[r];
        if meta.state == RowState::Clean {
            meta.original = Some(original.clone());
            meta.state = RowState::Modified;
        }
        let unchanged = meta.state == RowState::Modified && matches!(&v, CellValue::Value(x) if original.get(c) == Some(x)) || (meta.state == RowState::Modified && matches!(v, CellValue::Null) && original.get(c) == Some(&Value::Null));
        if unchanged {
            meta.edits.remove(&c);
            rows[r][c] = original[c].clone();
            if meta.edits.is_empty() {
                *meta = RowMeta::clean();
            }
            return;
        }
        rows[r][c] = display_of(&v);
        meta.edits.insert(c, v);
    }

    /// Appends a new row; columns with a default start as DEFAULT, the others as NULL.
    pub fn insert_row(&mut self, rows: &mut Vec<Row>, columns: &[ColumnMeta]) -> usize {
        self.sync_len(rows.len());
        self.snapshot(rows);
        let edits: BTreeMap<usize, CellValue> = columns.iter().enumerate().map(|(i, c)| (i, if c.default.is_some() || c.is_pk { CellValue::Default } else { CellValue::Null })).collect();
        rows.push(edits.values().map(display_of).collect());
        self.meta.push(RowMeta { state: RowState::New, original: None, edits });
        rows.len() - 1
    }

    /// Duplicates rows as new rows; key columns get their default so the database generates them.
    pub fn duplicate(&mut self, rows: &mut Vec<Row>, columns: &[ColumnMeta], selected: &[usize]) -> Vec<usize> {
        self.sync_len(rows.len());
        self.snapshot(rows);
        let mut added = Vec::new();
        for &r in selected {
            let Some(src) = rows.get(r).cloned() else { continue };
            let edits: BTreeMap<usize, CellValue> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let v = if c.is_pk { CellValue::Default } else { CellValue::from(src.get(i).cloned().unwrap_or(Value::Null)) };
                    (i, v)
                })
                .collect();
            rows.push(edits.values().map(display_of).collect());
            self.meta.push(RowMeta { state: RowState::New, original: None, edits });
            added.push(rows.len() - 1);
        }
        added
    }

    /// Deletes rows: new rows vanish, loaded rows are marked.
    pub fn delete(&mut self, rows: &mut Vec<Row>, selected: &[usize]) {
        self.sync_len(rows.len());
        if selected.is_empty() {
            return;
        }
        self.snapshot(rows);
        for &r in selected.iter().rev() {
            match self.meta.get(r).map(|m| m.state) {
                Some(RowState::New) => {
                    rows.remove(r);
                    self.meta.remove(r);
                }
                Some(_) => {
                    let m = &mut self.meta[r];
                    if m.original.is_none() {
                        m.original = Some(rows[r].clone());
                    }
                    m.state = RowState::Deleted;
                }
                None => {}
            }
        }
    }

    /// Undoes the last change; false when there is nothing to undo.
    pub fn undo(&mut self, rows: &mut Vec<Row>) -> bool {
        let Some((r, m)) = self.undo.pop() else { return false };
        self.redo.push((std::mem::replace(rows, r), std::mem::replace(&mut self.meta, m)));
        true
    }

    /// Redoes the last undone change.
    pub fn redo(&mut self, rows: &mut Vec<Row>) -> bool {
        let Some((r, m)) = self.redo.pop() else { return false };
        self.undo.push((std::mem::replace(rows, r), std::mem::replace(&mut self.meta, m)));
        true
    }

    /// Builds the change set. `key_cols` are the columns identifying a row.
    pub fn to_changeset(&self, table: &TableRef, columns: &[ColumnMeta], key_cols: &[String], rows: &[Row]) -> Result<ChangeSet, PendingError> {
        let mut cs = ChangeSet::new(table.clone());
        for (r, m) in self.meta.iter().enumerate() {
            match m.state {
                RowState::Clean => {}
                RowState::New => {
                    let values = columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (c.name.clone(), m.edits.get(&i).cloned().unwrap_or_else(|| CellValue::from(rows.get(r).and_then(|row| row.get(i)).cloned().unwrap_or(Value::Null)))))
                        .collect();
                    cs.push(RowChange::Insert { values });
                }
                RowState::Modified => {
                    let key = key_of(columns, key_cols, m.original.as_deref().unwrap_or(&[]))?;
                    let set = m.edits.iter().filter_map(|(i, v)| columns.get(*i).map(|c| (c.name.clone(), v.clone()))).collect();
                    cs.push(RowChange::Update { key, set });
                }
                RowState::Deleted => {
                    let key = key_of(columns, key_cols, m.original.as_deref().unwrap_or(&[]))?;
                    cs.push(RowChange::Delete { key });
                }
            }
        }
        Ok(cs)
    }
}

/// Key values of a row for the WHERE clause.
fn key_of(columns: &[ColumnMeta], key_cols: &[String], row: &[Value]) -> Result<Vec<(String, Value)>, PendingError> {
    if key_cols.is_empty() {
        return Err(PendingError::NoKey);
    }
    key_cols.iter().map(|k| columns.iter().position(|c| &c.name == k).and_then(|i| row.get(i)).map(|v| (k.clone(), v.clone())).ok_or_else(|| PendingError::Missing(k.clone()))).collect()
}

/// Columns that identify a row: the primary key, else the columns of the first unique index.
pub fn default_key_columns(st: Option<&TableStructure>) -> Vec<String> {
    let Some(st) = st else { return Vec::new() };
    if !st.primary_key.is_empty() {
        return st.primary_key.clone();
    }
    st.indexes.iter().find(|i| i.unique && i.columns.iter().all(|c| st.columns.iter().any(|x| &x.name == c && !x.nullable))).map(|i| i.columns.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::changes::generate_sql;
    use dbzilla_core::dialect::{dialect_for, DriverId};
    use dbzilla_core::model::IndexInfo;

    fn cols() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta { name: "id".into(), type_name: "integer".into(), is_pk: true, nullable: false, ..Default::default() },
            ColumnMeta { name: "name".into(), type_name: "text".into(), nullable: true, ..Default::default() },
            ColumnMeta { name: "qty".into(), type_name: "integer".into(), nullable: true, default: Some("0".into()), ..Default::default() },
        ]
    }

    fn rows() -> Vec<Row> {
        vec![vec![Value::I64(1), Value::Text("a".into()), Value::I64(5)], vec![Value::I64(2), Value::Text("b".into()), Value::Null]]
    }

    fn sql_for(driver: DriverId, p: &Pending, rows: &[Row]) -> Vec<String> {
        let cs = p.to_changeset(&TableRef::simple("t"), &cols(), &["id".to_owned()], rows).unwrap();
        generate_sql(&cs, dialect_for(driver).as_ref()).unwrap()
    }

    fn sql(p: &Pending, rows: &[Row]) -> Vec<String> {
        sql_for(DriverId::Sqlite, p, rows)
    }

    /// Editing marks the row modified and updates only that column by key.
    #[test]
    fn edit_generates_a_keyed_update() {
        let (mut rs, mut p) = (rows(), Pending::default());
        p.edit_cell(&mut rs, 0, 1, CellValue::Value(Value::Text("z".into())));
        assert_eq!(p.state(0), RowState::Modified);
        assert_eq!(rs[0][1], Value::Text("z".into()));
        assert_eq!(sql(&p, &rs), vec!["UPDATE \"t\" SET \"name\" = 'z' WHERE \"id\" = 1"]);
    }

    /// Typing the original value again removes the change.
    #[test]
    fn reverting_an_edit_cleans_the_row() {
        let (mut rs, mut p) = (rows(), Pending::default());
        p.edit_cell(&mut rs, 0, 1, CellValue::Value(Value::Text("z".into())));
        p.edit_cell(&mut rs, 0, 1, CellValue::Value(Value::Text("a".into())));
        assert_eq!(p.state(0), RowState::Clean);
        assert!(!p.has_changes());
        assert_eq!(rs, rows());
    }

    /// NULL, empty string and DEFAULT stay distinct.
    #[test]
    fn special_values() {
        let (mut rs, mut p) = (rows(), Pending::default());
        p.edit_cell(&mut rs, 0, 1, CellValue::Null);
        p.edit_cell(&mut rs, 1, 1, CellValue::Value(Value::Text(String::new())));
        p.edit_cell(&mut rs, 0, 2, CellValue::Default);
        let out = sql_for(DriverId::Postgres, &p, &rs);
        assert_eq!(out[0], "UPDATE \"t\" SET \"name\" = NULL, \"qty\" = DEFAULT WHERE \"id\" = 1");
        assert_eq!(out[1], "UPDATE \"t\" SET \"name\" = '' WHERE \"id\" = 2");
    }

    /// New rows start with defaults and become INSERT statements; defaults are omitted.
    #[test]
    fn insert_and_duplicate() {
        let (mut rs, mut p) = (rows(), Pending::default());
        let r = p.insert_row(&mut rs, &cols());
        assert_eq!(p.state(r), RowState::New);
        assert_eq!(rs[r][0], Value::Other(DEFAULT_TEXT.into()));
        p.edit_cell(&mut rs, r, 1, CellValue::Value(Value::Text("n".into())));
        let dup = p.duplicate(&mut rs, &cols(), &[0]);
        assert_eq!(dup.len(), 1);
        let out = sql(&p, &rs);
        assert_eq!(out[0], "INSERT INTO \"t\" (\"name\") VALUES ('n')");
        assert_eq!(out[1], "INSERT INTO \"t\" (\"name\", \"qty\") VALUES ('a', 5)");
    }

    /// Deleting a loaded row marks it; deleting a new row removes it.
    #[test]
    fn delete_rows() {
        let (mut rs, mut p) = (rows(), Pending::default());
        let r = p.insert_row(&mut rs, &cols());
        p.delete(&mut rs, &[1, r]);
        assert_eq!(rs.len(), 2, "the new row disappeared");
        assert_eq!(p.state(1), RowState::Deleted);
        assert_eq!(sql(&p, &rs), vec!["DELETE FROM \"t\" WHERE \"id\" = 2"]);
    }

    /// Undo and redo walk through the history.
    #[test]
    fn undo_redo() {
        let (mut rs, mut p) = (rows(), Pending::default());
        p.edit_cell(&mut rs, 0, 1, CellValue::Value(Value::Text("z".into())));
        p.delete(&mut rs, &[1]);
        assert!(p.undo(&mut rs));
        assert_eq!(p.state(1), RowState::Clean);
        assert!(p.undo(&mut rs));
        assert_eq!(rs, rows());
        assert!(!p.has_changes());
        assert!(!p.undo(&mut rs));
        assert!(p.redo(&mut rs));
        assert_eq!(rs[0][1], Value::Text("z".into()));
    }

    /// Without key columns updates and deletes are refused.
    #[test]
    fn missing_key_is_an_error() {
        let (mut rs, mut p) = (rows(), Pending::default());
        p.edit_cell(&mut rs, 0, 1, CellValue::Null);
        assert_eq!(p.to_changeset(&TableRef::simple("t"), &cols(), &[], &rs).unwrap_err(), PendingError::NoKey);
        assert_eq!(p.to_changeset(&TableRef::simple("t"), &cols(), &["nope".into()], &rs).unwrap_err(), PendingError::Missing("nope".into()));
    }

    /// Typed text is validated against the column type.
    #[test]
    fn parsing_edits() {
        assert_eq!(parse_edit("integer", " 42 "), Ok(CellValue::Value(Value::I64(42))));
        assert!(parse_edit("integer", "4x").is_err());
        assert_eq!(parse_edit("integer", ""), Ok(CellValue::Null));
        assert_eq!(parse_edit("numeric(10,2)", "1.50"), Ok(CellValue::Value(Value::Decimal("1.50".into()))));
        assert_eq!(parse_edit("double precision", "2.5"), Ok(CellValue::Value(Value::F64(2.5))));
        assert_eq!(parse_edit("boolean", "t"), Ok(CellValue::Value(Value::Bool(true))));
        assert!(parse_edit("boolean", "maybe").is_err());
        assert_eq!(parse_edit("point", "(1,2)"), Ok(CellValue::Value(Value::Text("(1,2)".into()))), "point is not an integer type");
        assert_eq!(parse_edit("bigint unsigned", "18446744073709551615"), Ok(CellValue::Value(Value::U64(u64::MAX))));
        assert_eq!(parse_edit("text", ""), Ok(CellValue::Value(Value::Text(String::new()))));
        assert_eq!(parse_edit("varchar(20)", " x "), Ok(CellValue::Value(Value::Text(" x ".into()))));
    }

    /// The key is the primary key, else the first unique index of non-null columns.
    #[test]
    fn key_columns() {
        let mut st = TableStructure { columns: cols(), primary_key: vec!["id".into()], ..Default::default() };
        assert_eq!(default_key_columns(Some(&st)), vec!["id"]);
        st.primary_key.clear();
        assert!(default_key_columns(Some(&st)).is_empty());
        st.indexes.push(IndexInfo { name: "u".into(), unique: true, columns: vec!["name".into()], condition: None });
        assert!(default_key_columns(Some(&st)).is_empty(), "name is nullable");
        st.indexes[0].columns = vec!["id".into()];
        assert_eq!(default_key_columns(Some(&st)), vec!["id"]);
        assert!(default_key_columns(None).is_empty());
    }
}
