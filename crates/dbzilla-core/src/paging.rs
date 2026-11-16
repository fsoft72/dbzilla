//! Builders for the table data tab: paged `SELECT` and `COUNT` (FR-D-001).

use crate::dialect::Dialect;
use crate::filter::{to_where, Filter};
use crate::model::{ColumnMeta, TableRef};

/// One sort column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub desc: bool,
}

/// `SELECT * FROM t [WHERE ..] [ORDER BY ..] LIMIT n OFFSET m` with quoted identifiers.
#[allow(clippy::too_many_arguments)]
pub fn build_select(d: &dyn Dialect, table: &TableRef, cols: &[ColumnMeta], filters: &[Filter], or: bool, sort: &[SortKey], limit: u64, offset: u64) -> String {
    let mut sql = format!("SELECT * FROM {}", d.quote_table(table));
    let cond = to_where(d, cols, filters, or);
    if !cond.is_empty() {
        sql.push_str(&format!(" WHERE {cond}"));
    }
    if !sort.is_empty() {
        let keys: Vec<String> = sort.iter().map(|k| format!("{} {}", d.quote_ident(&k.column), if k.desc { "DESC" } else { "ASC" })).collect();
        sql.push_str(&format!(" ORDER BY {}", keys.join(", ")));
    }
    sql.push(' ');
    sql.push_str(&d.limit_offset(limit, offset));
    sql
}

/// `SELECT COUNT(*) FROM t [WHERE ..]`.
pub fn build_count(d: &dyn Dialect, table: &TableRef, cols: &[ColumnMeta], filters: &[Filter], or: bool) -> String {
    let mut sql = format!("SELECT COUNT(*) FROM {}", d.quote_table(table));
    let cond = to_where(d, cols, filters, or);
    if !cond.is_empty() {
        sql.push_str(&format!(" WHERE {cond}"));
    }
    sql
}
