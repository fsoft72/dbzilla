//! Runs one query on a connection and shapes the result for JSON.

use dbzilla_drivers::{Conn, QueryOpts, RowBatch};
use dbzilla_io::export::json_value;
use serde_json::{json, Value as Json};

/// Maximum rows returned to a client.
pub const MAX_ROWS: usize = 1000;

/// Runs `sql` and returns `{columns, rows, truncated}`.
pub async fn run_query(conn: &mut dyn Conn, sql: &str) -> Result<Json, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<RowBatch>(4);
    let opts = QueryOpts { limit: Some(MAX_ROWS as u64 + 1), ..QueryOpts::default() };
    let reader = async {
        let mut cols: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Json>> = Vec::new();
        while let Some(b) = rx.recv().await {
            if let Some(c) = b.columns {
                cols = c.into_iter().map(|c| c.name).collect();
            }
            rows.extend(b.rows.iter().map(|r| r.iter().map(json_value).collect()));
        }
        (cols, rows)
    };
    let (res, (cols, mut rows)) = tokio::join!(conn.query_stream(sql, opts, tx), reader);
    res.map_err(|e| e.to_string())?;
    let truncated = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    Ok(json!({ "columns": cols, "rows": rows, "truncated": truncated }))
}

/// Runs a write or DDL statement and returns `{affected}`.
pub async fn run_write(conn: &mut dyn Conn, sql: &str) -> Result<Json, String> {
    let affected = conn.execute_batch(&[sql.to_owned()], false).await.map_err(|e| e.to_string())?;
    Ok(json!({ "affected": affected.first().copied().unwrap_or(0) }))
}
