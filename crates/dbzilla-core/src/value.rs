//! The dynamically typed cell value shared by drivers, grid and SQL builders.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One database value. `Null` and `Text("")` are always distinct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    /// Exact numeric kept as text to avoid precision loss.
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(NaiveDateTime),
    DateTimeTz(DateTime<FixedOffset>),
    Uuid(Uuid),
    Array(Vec<Value>),
    /// WKT or hex geometry text.
    Geometry(String),
    /// Anything the driver cannot map, as text.
    Other(String),
}

impl Value {
    /// True for SQL NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Human readable text used by the grid, copy and export (NULL renders as an empty string).
    pub fn to_display(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::I64(v) => v.to_string(),
            Value::U64(v) => v.to_string(),
            Value::F64(v) => v.to_string(),
            Value::Decimal(s) | Value::Text(s) | Value::Geometry(s) | Value::Other(s) => s.clone(),
            Value::Bytes(b) => format!("({} bytes)", b.len()),
            Value::Json(j) => j.to_string(),
            Value::Date(d) => d.to_string(),
            Value::Time(t) => t.to_string(),
            Value::DateTime(d) => d.to_string(),
            Value::DateTimeTz(d) => d.to_rfc3339(),
            Value::Uuid(u) => u.to_string(),
            Value::Array(items) => {
                let parts: Vec<String> = items.iter().map(Value::to_display).collect();
                format!("{{{}}}", parts.join(","))
            }
        }
    }
}
