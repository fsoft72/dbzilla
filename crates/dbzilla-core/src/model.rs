//! Metadata and result types exchanged between drivers and the UI.

use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// One result row.
pub type Row = Vec<Value>;

/// A foreign key target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyRef {
    pub table: TableRef,
    pub columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

/// Column description.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_pk: bool,
    pub fk: Option<ForeignKeyRef>,
    pub ordinal: usize,
    pub extra: BTreeMap<String, String>,
}

/// Fully qualified table name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TableRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
}

impl TableRef {
    /// A table in the default schema.
    pub fn simple(name: &str) -> Self {
        Self { database: None, schema: None, name: name.to_owned() }
    }
}

/// A server message (notice, warning).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
}

/// A fully materialised result set.
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Row>,
    pub affected: Option<u64>,
    pub elapsed: Duration,
    pub messages: Vec<Message>,
}

/// Index description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
    pub condition: Option<String>,
}

/// Trigger description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerInfo {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub statement: String,
}

/// Table structure: columns, keys, indexes, triggers and DDL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableStructure {
    pub table: TableRef,
    pub columns: Vec<ColumnMeta>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexInfo>,
    pub triggers: Vec<TriggerInfo>,
    pub ddl: String,
}

/// Kinds of objects listed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Sequence,
    Type,
}

/// One entry of the objects list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub kind: ObjectKind,
    pub table: TableRef,
}
