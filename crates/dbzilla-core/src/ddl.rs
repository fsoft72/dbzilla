//! DDL generation for structural operations (SPEC 8.6-8.9, appendix A). Pure, no I/O.

use crate::caps::Capabilities;
use crate::dialect::{Dialect, DriverId};
use crate::model::{ColumnMeta, TableRef, TableStructure};
use thiserror::Error;

/// Suffix of the temporary table used by the SQLite rebuild.
const REBUILD_SUFFIX: &str = "__new";

/// Errors from DDL generation.
#[derive(Debug, Error, PartialEq)]
pub enum DdlError {
    #[error("not supported: {0}")]
    Unsupported(String),
}

/// A column definition being created or altered.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// Raw SQL default expression.
    pub default: Option<String>,
    /// MySQL attributes that must survive MODIFY/CHANGE: `AUTO_INCREMENT`, `ON UPDATE ...`.
    pub extra: Option<String>,
}

/// A foreign key definition.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignKeyDef {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_table: TableRef,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

/// An index definition.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexDef {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// A queued structural change.
#[derive(Debug, Clone, PartialEq)]
pub enum StructuralOp {
    CreateTable { table: TableRef, columns: Vec<ColumnDef>, primary_key: Vec<String>, foreign_keys: Vec<ForeignKeyDef> },
    RenameTable { table: TableRef, new_name: String },
    DropTable { table: TableRef, cascade: bool },
    Truncate { table: TableRef, cascade: bool, restart_identity: bool },
    AddColumn { table: TableRef, column: ColumnDef },
    DropColumn { table: TableRef, column: String },
    /// `current` is required for SQLite, which rebuilds the table.
    AlterColumn { table: TableRef, old: ColumnDef, new: ColumnDef, current: Option<Box<TableStructure>> },
    AddIndex { table: TableRef, index: IndexDef },
    DropIndex { table: TableRef, name: String },
    AddForeignKey { table: TableRef, fk: ForeignKeyDef },
    DropForeignKey { table: TableRef, name: String },
    CloneTable { source: TableRef, target: TableRef, with_data: bool },
    /// Raw SQL (view or function definitions typed in a DDL editor).
    Raw(String),
}

/// Keeps what MySQL reports in `EXTRA` and a MODIFY/CHANGE would otherwise drop.
pub fn mysql_column_suffix(extra: &str) -> Option<String> {
    let lower = extra.to_ascii_lowercase();
    let mut parts = Vec::new();
    if lower.contains("auto_increment") {
        parts.push("AUTO_INCREMENT".to_owned());
    }
    if let Some(i) = lower.find("on update ") {
        parts.push(format!("ON UPDATE {}", extra[i + "on update ".len()..].trim()));
    }
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// True for column types that carry a character set and collation.
fn is_textual(type_name: &str) -> bool {
    let t = type_name.to_ascii_lowercase();
    ["char", "varchar", "tinytext", "text", "mediumtext", "longtext", "enum", "set"].iter().any(|w| t == *w || t.starts_with(&format!("{w}(")) || t.starts_with(&format!("{w} ")))
}

/// The MySQL attributes a MODIFY/CHANGE must repeat for a column read from the server:
/// AUTO_INCREMENT, ON UPDATE, the collation of text columns and the comment.
pub fn mysql_column_attrs(extra: &std::collections::BTreeMap<String, String>, type_name: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(e) = mysql_column_suffix(extra.get("extra").map_or("", String::as_str)) {
        parts.push(e);
    }
    if let Some(c) = extra.get("collation").filter(|c| !c.is_empty() && is_textual(type_name)) {
        parts.push(format!("COLLATE {c}"));
    }
    if let Some(c) = extra.get("comment").filter(|c| !c.is_empty()) {
        parts.push(format!("COMMENT '{}'", c.replace('\\', "\\\\").replace('\'', "''")));
    }
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// `"name" type [NOT NULL] [DEFAULT x]`.
fn col_sql(d: &dyn Dialect, c: &ColumnDef) -> String {
    let mut s = format!("{} {}", d.quote_ident(&c.name), c.type_name);
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(def) = &c.default {
        s.push_str(&format!(" DEFAULT {def}"));
    }
    s
}

/// Comma separated, quoted identifier list.
fn ident_list(d: &dyn Dialect, cols: &[String]) -> String {
    cols.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ")
}

/// `[CONSTRAINT n] FOREIGN KEY (..) REFERENCES t (..) ON UPDATE x ON DELETE y`.
fn fk_sql(d: &dyn Dialect, fk: &ForeignKeyDef) -> String {
    let name = fk.name.as_ref().map(|n| format!("CONSTRAINT {} ", d.quote_ident(n))).unwrap_or_default();
    format!(
        "{name}FOREIGN KEY ({}) REFERENCES {} ({}) ON UPDATE {} ON DELETE {}",
        ident_list(d, &fk.columns),
        d.quote_table(&fk.ref_table),
        ident_list(d, &fk.ref_columns),
        fk.on_update,
        fk.on_delete
    )
}

/// Full `CREATE TABLE` statement without trailing semicolon.
fn create_table_sql(d: &dyn Dialect, table: &TableRef, cols: &[ColumnDef], pk: &[String], fks: &[ForeignKeyDef]) -> String {
    let mut lines: Vec<String> = cols.iter().map(|c| col_sql(d, c)).collect();
    if !pk.is_empty() {
        lines.push(format!("PRIMARY KEY ({})", ident_list(d, pk)));
    }
    lines.extend(fks.iter().map(|f| fk_sql(d, f)));
    format!("CREATE TABLE {} (\n  {}\n)", d.quote_table(table), lines.join(",\n  "))
}

/// Converts introspected metadata to a column definition.
fn def_from_meta(c: &ColumnMeta) -> ColumnDef {
    ColumnDef { name: c.name.clone(), type_name: c.type_name.clone(), nullable: c.nullable, default: c.default.clone(), extra: mysql_column_attrs(&c.extra, &c.type_name) }
}

/// Foreign keys implied by column metadata.
fn fks_from_columns(cols: &[ColumnMeta]) -> Vec<ForeignKeyDef> {
    cols.iter()
        .filter_map(|c| {
            let fk = c.fk.as_ref()?;
            Some(ForeignKeyDef {
                name: fk.name.clone(),
                columns: vec![c.name.clone()],
                ref_table: fk.table.clone(),
                ref_columns: fk.columns.clone(),
                on_update: fk.on_update.clone(),
                on_delete: fk.on_delete.clone(),
            })
        })
        .collect()
}

/// Builds a `CREATE TABLE` statement from introspected structure (used by Copy Script As).
pub fn create_from_structure(st: &TableStructure, d: &dyn Dialect) -> String {
    let cols: Vec<ColumnDef> = st.columns.iter().map(def_from_meta).collect();
    create_table_sql(d, &st.table, &cols, &st.primary_key, &fks_from_columns(&st.columns))
}

/// Generates the statements for one structural operation.
pub fn generate_ddl(op: &StructuralOp, d: &dyn Dialect) -> Result<Vec<String>, DdlError> {
    let caps = Capabilities::for_driver(d.id());
    let q = |t: &TableRef| d.quote_table(t);
    Ok(match op {
        StructuralOp::CreateTable { table, columns, primary_key, foreign_keys } => {
            vec![create_table_sql(d, table, columns, primary_key, foreign_keys)]
        }
        StructuralOp::RenameTable { table, new_name } => match d.id() {
            DriverId::MySql => {
                let mut target = table.clone();
                target.name = new_name.clone();
                vec![format!("RENAME TABLE {} TO {}", q(table), q(&target))]
            }
            _ => vec![format!("ALTER TABLE {} RENAME TO {}", q(table), d.quote_ident(new_name))],
        },
        StructuralOp::DropTable { table, cascade } => {
            let tail = if *cascade && caps.drop_cascade { " CASCADE" } else { "" };
            vec![format!("DROP TABLE {}{tail}", q(table))]
        }
        StructuralOp::Truncate { table, cascade, restart_identity } => match d.id() {
            DriverId::Sqlite => vec![format!("DELETE FROM {}", q(table))],
            _ => {
                let restart = if *restart_identity && caps.truncate_restart_identity { " RESTART IDENTITY" } else { "" };
                let casc = if *cascade && caps.truncate_cascade { " CASCADE" } else { "" };
                vec![format!("TRUNCATE TABLE {}{restart}{casc}", q(table))]
            }
        },
        StructuralOp::AddColumn { table, column } => vec![format!("ALTER TABLE {} ADD COLUMN {}", q(table), col_sql(d, column))],
        StructuralOp::DropColumn { table, column } => vec![format!("ALTER TABLE {} DROP COLUMN {}", q(table), d.quote_ident(column))],
        StructuralOp::AlterColumn { table, old, new, current } => alter_column(d, table, old, new, current.as_deref())?,
        StructuralOp::AddIndex { table, index } => {
            let unique = if index.unique { "UNIQUE " } else { "" };
            vec![format!("CREATE {unique}INDEX {} ON {} ({})", d.quote_ident(&index.name), q(table), ident_list(d, &index.columns))]
        }
        StructuralOp::DropIndex { table, name } => match d.id() {
            DriverId::MySql => vec![format!("DROP INDEX {} ON {}", d.quote_ident(name), q(table))],
            DriverId::Postgres => {
                let schema = table.schema.as_ref().map(|s| format!("{}.", d.quote_ident(s))).unwrap_or_default();
                vec![format!("DROP INDEX {schema}{}", d.quote_ident(name))]
            }
            DriverId::Sqlite => vec![format!("DROP INDEX {}", d.quote_ident(name))],
        },
        StructuralOp::AddForeignKey { table, fk } => {
            if !caps.add_fk_after_create {
                return Err(DdlError::Unsupported("this database cannot add a foreign key to an existing table".into()));
            }
            vec![format!("ALTER TABLE {} ADD {}", q(table), fk_sql(d, fk))]
        }
        StructuralOp::DropForeignKey { table, name } => match d.id() {
            DriverId::Sqlite => return Err(DdlError::Unsupported("this database cannot drop a foreign key from an existing table".into())),
            DriverId::MySql => vec![format!("ALTER TABLE {} DROP FOREIGN KEY {}", q(table), d.quote_ident(name))],
            DriverId::Postgres => vec![format!("ALTER TABLE {} DROP CONSTRAINT {}", q(table), d.quote_ident(name))],
        },
        StructuralOp::CloneTable { source, target, with_data } => clone_table(d, source, target, *with_data),
        StructuralOp::Raw(sql) => vec![sql.clone()],
    })
}

/// Clone statements per dialect.
fn clone_table(d: &dyn Dialect, source: &TableRef, target: &TableRef, with_data: bool) -> Vec<String> {
    let (s, t) = (d.quote_table(source), d.quote_table(target));
    let insert = format!("INSERT INTO {t} SELECT * FROM {s}");
    match d.id() {
        DriverId::Postgres => {
            let mut v = vec![format!("CREATE TABLE {t} (LIKE {s} INCLUDING ALL)")];
            if with_data {
                v.push(insert);
            }
            v
        }
        DriverId::MySql => {
            let mut v = vec![format!("CREATE TABLE {t} LIKE {s}")];
            if with_data {
                v.push(insert);
            }
            v
        }
        DriverId::Sqlite => {
            let tail = if with_data { "" } else { " WHERE 0" };
            vec![format!("CREATE TABLE {t} AS SELECT * FROM {s}{tail}")]
        }
    }
}

/// Alter column per dialect (SQLite rebuilds the table).
fn alter_column(d: &dyn Dialect, table: &TableRef, old: &ColumnDef, new: &ColumnDef, current: Option<&TableStructure>) -> Result<Vec<String>, DdlError> {
    let t = d.quote_table(table);
    match d.id() {
        DriverId::Postgres => {
            let mut v = Vec::new();
            if old.name != new.name {
                v.push(format!("ALTER TABLE {t} RENAME COLUMN {} TO {}", d.quote_ident(&old.name), d.quote_ident(&new.name)));
            }
            let c = d.quote_ident(&new.name);
            if old.type_name != new.type_name {
                v.push(format!("ALTER TABLE {t} ALTER COLUMN {c} TYPE {}", new.type_name));
            }
            if old.nullable != new.nullable {
                v.push(format!("ALTER TABLE {t} ALTER COLUMN {c} {} NOT NULL", if new.nullable { "DROP" } else { "SET" }));
            }
            if old.default != new.default {
                match &new.default {
                    Some(def) => v.push(format!("ALTER TABLE {t} ALTER COLUMN {c} SET DEFAULT {def}")),
                    None => v.push(format!("ALTER TABLE {t} ALTER COLUMN {c} DROP DEFAULT")),
                }
            }
            Ok(v)
        }
        DriverId::MySql => {
            let mut def = col_sql(d, new).replacen(&d.quote_ident(&new.name), "", 1).trim_start().to_owned();
            if let Some(extra) = &new.extra {
                def.push(' ');
                def.push_str(extra);
            }
            let def = def.as_str();
            if old.name != new.name {
                Ok(vec![format!("ALTER TABLE {t} CHANGE COLUMN {} {} {def}", d.quote_ident(&old.name), d.quote_ident(&new.name))])
            } else {
                Ok(vec![format!("ALTER TABLE {t} MODIFY COLUMN {} {def}", d.quote_ident(&new.name))])
            }
        }
        DriverId::Sqlite => {
            let st = current.ok_or_else(|| DdlError::Unsupported("altering a SQLite column needs the current table structure".into()))?;
            sqlite_rebuild(d, table, old, new, st)
        }
    }
}

/// SQLite table rebuild: create new, copy, drop old, rename, recreate indexes and triggers.
/// The new table is the original `CREATE TABLE` text with only the changed column rewritten, so
/// UNIQUE, CHECK, COLLATE, AUTOINCREMENT and table constraints are kept. When the original text is
/// not known (empty `ddl`) the table is rebuilt from the model, which only knows plain columns.
fn sqlite_rebuild(d: &dyn Dialect, table: &TableRef, old: &ColumnDef, new: &ColumnDef, st: &TableStructure) -> Result<Vec<String>, DdlError> {
    let refuse = |why: String| DdlError::Unsupported(format!("this SQLite table cannot be rebuilt safely: {why}"));
    if !st.referenced_by.is_empty() {
        return Err(refuse(format!("{} refer to it by foreign key and dropping it could delete their rows", st.referenced_by.join(", "))));
    }
    if st.ddl.trim().is_empty() {
        return Ok(sqlite_rebuild_from_model(d, table, old, new, st));
    }
    let mut tmp = table.clone();
    tmp.name = format!("{}{REBUILD_SUFFIX}", table.name);
    let ch = crate::sqlite_rewrite::ColumnChange { old_name: &old.name, new_name: &new.name, new_type: &new.type_name, nullable: new.nullable, default: new.default.as_deref() };
    let create = crate::sqlite_rewrite::rewrite_create(&st.ddl, &d.quote_table(&tmp), &ch, &|n| d.quote_ident(n)).map_err(refuse)?;
    let renamed = old.name != new.name;
    for sql in st.index_sql.iter().chain(st.triggers.iter().map(|t| &t.statement)) {
        if renamed && crate::sqlite_rewrite::mentions_ident(sql, &old.name) {
            return Err(refuse("an index or trigger refers to the renamed column".into()));
        }
    }
    let new_names: Vec<String> = st.columns.iter().map(|c| if c.name == old.name { new.name.clone() } else { c.name.clone() }).collect();
    let old_names: Vec<String> = st.columns.iter().map(|c| c.name.clone()).collect();
    let mut v = vec![
        create,
        format!("INSERT INTO {} ({}) SELECT {} FROM {}", d.quote_table(&tmp), ident_list(d, &new_names), ident_list(d, &old_names), d.quote_table(table)),
        format!("DROP TABLE {}", d.quote_table(table)),
        format!("ALTER TABLE {} RENAME TO {}", d.quote_table(&tmp), d.quote_ident(&table.name)),
    ];
    v.extend(st.index_sql.iter().cloned());
    v.extend(st.triggers.iter().map(|t| t.statement.clone()));
    Ok(v)
}

/// Rebuild from the column model alone (no original table text available).
fn sqlite_rebuild_from_model(d: &dyn Dialect, table: &TableRef, old: &ColumnDef, new: &ColumnDef, st: &TableStructure) -> Vec<String> {
    let mut tmp = table.clone();
    tmp.name = format!("{}{REBUILD_SUFFIX}", table.name);
    let rename = |n: &str| if n == old.name { new.name.clone() } else { n.to_owned() };
    let cols: Vec<ColumnDef> = st.columns.iter().map(|c| if c.name == old.name { new.clone() } else { def_from_meta(c) }).collect();
    let pk: Vec<String> = st.primary_key.iter().map(|n| rename(n)).collect();
    let fks: Vec<ForeignKeyDef> = fks_from_columns(&st.columns)
        .into_iter()
        .map(|mut f| {
            f.columns = f.columns.iter().map(|n| rename(n)).collect();
            f
        })
        .collect();
    let new_names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
    let old_names: Vec<String> = st.columns.iter().map(|c| c.name.clone()).collect();
    let mut v = vec![
        create_table_sql(d, &tmp, &cols, &pk, &fks),
        format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            d.quote_table(&tmp),
            ident_list(d, &new_names),
            ident_list(d, &old_names),
            d.quote_table(table)
        ),
        format!("DROP TABLE {}", d.quote_table(table)),
        format!("ALTER TABLE {} RENAME TO {}", d.quote_table(&tmp), d.quote_ident(&table.name)),
    ];
    for ix in &st.indexes {
        let unique = if ix.unique { "UNIQUE " } else { "" };
        let ix_cols: Vec<String> = ix.columns.iter().map(|n| rename(n)).collect();
        v.push(format!("CREATE {unique}INDEX {} ON {} ({})", d.quote_ident(&ix.name), d.quote_table(table), ident_list(d, &ix_cols)));
    }
    v
}
