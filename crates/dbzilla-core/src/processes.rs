//! Process list (SPEC 11.4): the statements that list and stop server sessions, and the sorting and
//! filtering of the result. Pure logic, no UI and no I/O.

use crate::dialect::DriverId;
use crate::model::Row;
use crate::value::Value;
use std::cmp::Ordering;

/// Index of the column that holds the session id in the result of [`list_sql`].
pub const ID_COLUMN: usize = 0;

/// True when the driver can list and stop sessions.
pub fn supported(driver: DriverId) -> bool {
    matches!(driver, DriverId::Postgres | DriverId::MySql)
}

/// Statement that lists the running sessions, the session id first. The own session is left out.
/// `None` when the driver has no process list.
pub fn list_sql(driver: DriverId) -> Option<&'static str> {
    match driver {
        DriverId::Postgres => Some(
            "SELECT pid, datname, usename, application_name, client_addr::text AS client_addr, query_start::text AS query_start, state, query \
             FROM pg_stat_activity WHERE backend_type = 'client backend' AND state IS DISTINCT FROM 'idle' AND pid <> pg_backend_pid() ORDER BY query_start DESC NULLS LAST",
        ),
        DriverId::MySql => Some("SELECT id, user, host, db, command, time, state, info FROM information_schema.processlist WHERE id <> CONNECTION_ID() AND command <> 'Daemon' ORDER BY time"),
        DriverId::Sqlite | DriverId::SqlServer => None,
    }
}

/// Statement that stops what a session is running: PostgreSQL cancels the query and keeps the session,
/// MySQL and MariaDB kill the query only. `id` must be a plain number, so nothing else can be injected.
pub fn kill_sql(driver: DriverId, id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match driver {
        DriverId::Postgres => Some(format!("SELECT pg_cancel_backend({id});")),
        DriverId::MySql => Some(format!("KILL QUERY {id};")),
        DriverId::Sqlite | DriverId::SqlServer => None,
    }
}

/// Session id of a row, as text.
pub fn row_id(row: &Row) -> String {
    row.get(ID_COLUMN).map(Value::to_display).unwrap_or_default()
}

/// Orders two cells: numbers by value, everything else by text, NULL last.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a.is_null(), b.is_null()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    match (crate::metrics::value_to_f64(a), crate::metrics::value_to_f64(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => a.to_display().to_lowercase().cmp(&b.to_display().to_lowercase()),
    }
}

/// Indexes of `rows` ordered by `column`; a stable sort, so equal cells keep the server order. NULL
/// stays last in both directions.
pub fn sorted(rows: &[Row], column: usize, ascending: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    let cell = |i: usize| rows[i].get(column).unwrap_or(&Value::Null);
    idx.sort_by(|a, b| {
        let (x, y) = (cell(*a), cell(*b));
        if x.is_null() || y.is_null() {
            return compare(x, y);
        }
        if ascending { compare(x, y) } else { compare(y, x) }
    });
    idx
}

/// True when any cell of the row contains `needle`, ignoring case. An empty needle matches.
pub fn matches(row: &Row, needle: &str) -> bool {
    let n = needle.trim().to_lowercase();
    n.is_empty() || row.iter().any(|v| v.to_display().to_lowercase().contains(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: i64, name: &str) -> Row {
        vec![Value::I64(id), Value::Text(name.into())]
    }

    #[test]
    fn only_postgres_and_mysql_list_processes() {
        assert!(supported(DriverId::Postgres) && supported(DriverId::MySql) && !supported(DriverId::Sqlite));
        assert!(list_sql(DriverId::Sqlite).is_none());
        assert!(list_sql(DriverId::Postgres).unwrap().contains("pg_stat_activity"));
        assert!(list_sql(DriverId::Postgres).unwrap().contains("client backend"), "background workers are not sessions");
        assert!(list_sql(DriverId::MySql).unwrap().contains("information_schema.processlist"));
    }

    #[test]
    fn kill_statements_per_driver() {
        assert_eq!(kill_sql(DriverId::Postgres, "42").as_deref(), Some("SELECT pg_cancel_backend(42);"));
        assert_eq!(kill_sql(DriverId::MySql, " 7 ").as_deref(), Some("KILL QUERY 7;"));
        assert_eq!(kill_sql(DriverId::Sqlite, "1"), None);
    }

    #[test]
    fn kill_refuses_ids_that_are_not_numbers() {
        for bad in ["", "1; DROP TABLE t", "-1", "1.5", "abc", "1 OR 1"] {
            assert_eq!(kill_sql(DriverId::Postgres, bad), None, "{bad}");
            assert_eq!(kill_sql(DriverId::MySql, bad), None, "{bad}");
        }
    }

    #[test]
    fn sorts_numbers_by_value_and_text_ignoring_case() {
        let rows = vec![r(10, "b"), r(9, "A"), r(100, "c")];
        assert_eq!(sorted(&rows, 0, true), vec![1, 0, 2]);
        assert_eq!(sorted(&rows, 0, false), vec![2, 0, 1]);
        assert_eq!(sorted(&rows, 1, true), vec![1, 0, 2]);
    }

    #[test]
    fn null_is_last_in_both_directions_and_ties_keep_order() {
        let rows = vec![vec![Value::Null], vec![Value::I64(2)], vec![Value::I64(1)], vec![Value::I64(2)]];
        assert_eq!(sorted(&rows, 0, true), vec![2, 1, 3, 0]);
        assert_eq!(sorted(&rows, 0, false), vec![1, 3, 2, 0]);
    }

    #[test]
    fn filter_looks_in_every_cell() {
        let row = r(5, "Select Now()");
        assert!(matches(&row, "now") && matches(&row, "  ") && matches(&row, "5"));
        assert!(!matches(&row, "zzz"));
        assert_eq!(row_id(&row), "5");
    }
}
