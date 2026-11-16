//! Editable draft of a table structure and its translation to DDL operations (SPEC 8.6-8.8).

use dbzilla_core::caps::Capabilities;
use dbzilla_core::ddl::{ColumnDef, ForeignKeyDef, IndexDef, StructuralOp};
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, IndexInfo, TableRef, TableStructure};
use std::collections::HashSet;

/// A foreign key being edited.
#[derive(Debug, Clone, PartialEq)]
pub struct FkDraft {
    pub name: Option<String>,
    pub ref_table: TableRef,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

impl FkDraft {
    /// Converts introspected metadata.
    fn from_ref(fk: &ForeignKeyRef) -> Self {
        Self { name: fk.name.clone(), ref_table: fk.table.clone(), ref_columns: fk.columns.clone(), on_update: fk.on_update.clone(), on_delete: fk.on_delete.clone() }
    }

    /// Text such as `-> customers(id)`.
    pub fn summary(&self) -> String {
        format!("\u{2192} {}({})", self.ref_table.name, self.ref_columns.join(", "))
    }
}

/// A column being edited.
#[derive(Debug, Clone, PartialEq)]
pub struct ColDraft {
    pub orig: Option<ColumnMeta>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// Default expression; empty means none.
    pub default: String,
    pub pk: bool,
    pub deleted: bool,
    pub fk: Option<FkDraft>,
}

impl ColDraft {
    /// Draft of an existing column.
    fn from_meta(c: &ColumnMeta) -> Self {
        Self { orig: Some(c.clone()), name: c.name.clone(), type_name: c.type_name.clone(), nullable: c.nullable, default: c.default.clone().unwrap_or_default(), pk: c.is_pk, deleted: false, fk: c.fk.as_ref().map(FkDraft::from_ref) }
    }

    /// A new empty column.
    pub fn blank(type_name: &str) -> Self {
        Self { orig: None, name: String::new(), type_name: type_name.to_owned(), nullable: true, default: String::new(), pk: false, deleted: false, fk: None }
    }

    /// Definition for DDL.
    fn def(&self) -> ColumnDef {
        ColumnDef { name: self.name.trim().to_owned(), type_name: self.type_name.trim().to_owned(), nullable: self.nullable, default: if self.default.trim().is_empty() { None } else { Some(self.default.trim().to_owned()) } }
    }

    /// True when name, type, nullability or default differ from the original.
    pub fn is_modified(&self) -> bool {
        self.orig.as_ref().is_some_and(|o| o.name != self.name || o.type_name != self.type_name || o.nullable != self.nullable || o.default.clone().unwrap_or_default() != self.default)
    }
}

/// An index being edited.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexDraft {
    pub orig: Option<IndexInfo>,
    pub name: String,
    pub unique: bool,
    /// Comma separated column names.
    pub columns: String,
    pub deleted: bool,
}

impl IndexDraft {
    /// A new empty index.
    pub fn blank() -> Self {
        Self { orig: None, name: String::new(), unique: false, columns: String::new(), deleted: false }
    }

    /// Column names.
    pub fn column_list(&self) -> Vec<String> {
        self.columns.split(',').map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect()
    }

    /// True when it differs from the original.
    pub fn is_modified(&self) -> bool {
        self.orig.as_ref().is_some_and(|o| o.name != self.name || o.unique != self.unique || o.columns != self.column_list())
    }
}

/// The structure of one table being edited.
#[derive(Debug, Clone)]
pub struct StructDraft {
    pub table: TableRef,
    pub new_name: String,
    pub cols: Vec<ColDraft>,
    pub indexes: Vec<IndexDraft>,
    pub original: TableStructure,
    /// True when the table does not exist yet.
    pub is_new: bool,
}

impl StructDraft {
    /// Draft of an existing table.
    pub fn from_structure(st: &TableStructure) -> Self {
        let pk: HashSet<&String> = st.primary_key.iter().collect();
        let mut cols: Vec<ColDraft> = st.columns.iter().map(ColDraft::from_meta).collect();
        for c in &mut cols {
            c.pk = pk.contains(&c.name);
        }
        let indexes = st.indexes.iter().map(|i| IndexDraft { orig: Some(i.clone()), name: i.name.clone(), unique: i.unique, columns: i.columns.join(", "), deleted: false }).collect();
        Self { table: st.table.clone(), new_name: st.table.name.clone(), cols, indexes, original: st.clone(), is_new: false }
    }

    /// Draft of a table to be created.
    pub fn new_table(table: TableRef, default_type: &str) -> Self {
        let mut id = ColDraft::blank(default_type);
        id.name = "id".into();
        id.nullable = false;
        id.pk = true;
        Self { new_name: table.name.clone(), table, cols: vec![id], indexes: Vec::new(), original: TableStructure::default(), is_new: true }
    }

    /// True when anything differs from the database.
    pub fn has_changes(&self) -> bool {
        self.is_new
            || self.new_name != self.table.name
            || self.cols.iter().any(|c| c.orig.is_none() && !c.deleted || c.deleted && c.orig.is_some() || c.is_modified() || fk_changed(c))
            || self.indexes.iter().any(|i| i.orig.is_none() && !i.deleted || i.deleted && i.orig.is_some() || i.is_modified())
    }

    /// Primary key column names.
    pub fn pk_columns(&self) -> Vec<String> {
        self.cols.iter().filter(|c| c.pk && !c.deleted).map(|c| c.name.trim().to_owned()).collect()
    }

    /// Checks names and types before generating DDL.
    fn validate(&self) -> Result<(), String> {
        if self.new_name.trim().is_empty() {
            return Err("The table needs a name".into());
        }
        let live: Vec<&ColDraft> = self.cols.iter().filter(|c| !c.deleted).collect();
        if live.is_empty() {
            return Err("A table needs at least one column".into());
        }
        let mut seen = HashSet::new();
        for c in &live {
            if c.name.trim().is_empty() {
                return Err("Every column needs a name".into());
            }
            if c.type_name.trim().is_empty() {
                return Err(format!("The column {} needs a data type", c.name));
            }
            if !seen.insert(c.name.trim().to_lowercase()) {
                return Err(format!("The column name {} is used twice", c.name));
            }
        }
        for i in self.indexes.iter().filter(|i| !i.deleted) {
            if i.name.trim().is_empty() {
                return Err("Every index needs a name".into());
            }
            let cols = i.column_list();
            if cols.is_empty() {
                return Err(format!("The index {} needs columns", i.name));
            }
            if let Some(missing) = cols.iter().find(|c| !seen.contains(&c.to_lowercase())) {
                return Err(format!("The index {} uses the unknown column {missing}", i.name));
            }
        }
        Ok(())
    }

    /// Translates the draft into structural operations, in a safe order.
    pub fn to_ops(&self, caps: &Capabilities) -> Result<Vec<StructuralOp>, String> {
        self.validate()?;
        let table = self.table.clone();
        if self.is_new {
            let mut t = table;
            t.name = self.new_name.trim().to_owned();
            let live: Vec<&ColDraft> = self.cols.iter().filter(|c| !c.deleted).collect();
            let foreign_keys = live
                .iter()
                .filter_map(|c| c.fk.as_ref().map(|f| ForeignKeyDef { name: f.name.clone(), columns: vec![c.name.trim().to_owned()], ref_table: f.ref_table.clone(), ref_columns: f.ref_columns.clone(), on_update: f.on_update.clone(), on_delete: f.on_delete.clone() }))
                .collect();
            let mut ops = vec![StructuralOp::CreateTable { table: t.clone(), columns: live.iter().map(|c| c.def()).collect(), primary_key: self.pk_columns(), foreign_keys }];
            for i in self.indexes.iter().filter(|i| !i.deleted) {
                ops.push(StructuralOp::AddIndex { table: t.clone(), index: IndexDef { name: i.name.trim().to_owned(), unique: i.unique, columns: i.column_list() } });
            }
            return Ok(ops);
        }
        let mut ops = Vec::new();
        let mut working = self.original.clone();
        for i in self.indexes.iter().filter(|i| i.orig.is_some() && (i.deleted || i.is_modified())) {
            let name = i.orig.as_ref().map(|o| o.name.clone()).unwrap_or_default();
            working.indexes.retain(|x| x.name != name);
            ops.push(StructuralOp::DropIndex { table: table.clone(), name });
        }
        for c in self.cols.iter().filter(|c| c.deleted && c.orig.is_some()) {
            let name = c.orig.as_ref().map(|o| o.name.clone()).unwrap_or_default();
            working.columns.retain(|x| x.name != name);
            working.primary_key.retain(|x| x != &name);
            ops.push(StructuralOp::DropColumn { table: table.clone(), column: name });
        }
        for c in self.cols.iter().filter(|c| !c.deleted && c.is_modified()) {
            let Some(orig) = &c.orig else { continue };
            let old = ColDraft::from_meta(orig).def();
            let new = c.def();
            ops.push(StructuralOp::AlterColumn { table: table.clone(), old: old.clone(), new: new.clone(), current: Some(Box::new(working.clone())) });
            if let Some(w) = working.columns.iter_mut().find(|x| x.name == old.name) {
                w.name = new.name.clone();
                w.type_name = new.type_name.clone();
                w.nullable = new.nullable;
                w.default = new.default.clone();
            }
            for p in &mut working.primary_key {
                if *p == old.name {
                    *p = new.name.clone();
                }
            }
            for i in &mut working.indexes {
                for col in &mut i.columns {
                    if *col == old.name {
                        *col = new.name.clone();
                    }
                }
            }
        }
        for c in self.cols.iter().filter(|c| !c.deleted && c.orig.is_none()) {
            ops.push(StructuralOp::AddColumn { table: table.clone(), column: c.def() });
        }
        for i in self.indexes.iter().filter(|i| !i.deleted && (i.orig.is_none() || i.is_modified())) {
            ops.push(StructuralOp::AddIndex { table: table.clone(), index: IndexDef { name: i.name.trim().to_owned(), unique: i.unique, columns: i.column_list() } });
        }
        for c in self.cols.iter().filter(|c| !c.deleted && fk_changed(c)) {
            if let Some(old) = c.orig.as_ref().and_then(|o| o.fk.as_ref()) {
                let name = old.name.clone().ok_or_else(|| format!("The name of the foreign key on {} is unknown, so it cannot be removed", c.name))?;
                ops.push(StructuralOp::DropForeignKey { table: table.clone(), name });
            }
            if let Some(f) = &c.fk {
                if !caps.add_fk_after_create {
                    return Err("This database cannot add a foreign key to an existing table".into());
                }
                ops.push(StructuralOp::AddForeignKey {
                    table: table.clone(),
                    fk: ForeignKeyDef { name: f.name.clone(), columns: vec![c.name.trim().to_owned()], ref_table: f.ref_table.clone(), ref_columns: f.ref_columns.clone(), on_update: f.on_update.clone(), on_delete: f.on_delete.clone() },
                });
            }
        }
        if self.new_name.trim() != table.name {
            ops.push(StructuralOp::RenameTable { table, new_name: self.new_name.trim().to_owned() });
        }
        Ok(ops)
    }
}

/// True when the foreign key of a column was added, changed or removed.
fn fk_changed(c: &ColDraft) -> bool {
    let original = c.orig.as_ref().and_then(|o| o.fk.as_ref()).map(FkDraft::from_ref);
    original != c.fk
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::dialect::{dialect_for, DriverId};
    use dbzilla_core::ddl::generate_ddl;

    fn col(name: &str, ty: &str, nullable: bool, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), type_name: ty.into(), nullable, is_pk: pk, ..Default::default() }
    }

    fn orders() -> TableStructure {
        TableStructure {
            table: TableRef::simple("orders"),
            columns: vec![col("id", "integer", false, true), col("note", "text", true, false), col("cid", "integer", true, false)],
            primary_key: vec!["id".into()],
            indexes: vec![IndexInfo { name: "ix_cid".into(), unique: false, columns: vec!["cid".into()], condition: None }],
            ..Default::default()
        }
    }

    fn sql(d: &StructDraft, driver: DriverId) -> Vec<String> {
        d.to_ops(&Capabilities::for_driver(driver)).unwrap().iter().flat_map(|o| generate_ddl(o, dialect_for(driver).as_ref()).unwrap()).collect()
    }

    /// An untouched draft has no changes and no operations.
    #[test]
    fn untouched_draft() {
        let d = StructDraft::from_structure(&orders());
        assert!(!d.has_changes());
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).unwrap().is_empty());
    }

    /// New column, dropped column and renamed table in a safe order.
    #[test]
    fn add_drop_and_rename() {
        let mut d = StructDraft::from_structure(&orders());
        d.cols[1].deleted = true;
        let mut c = ColDraft::blank("integer");
        c.name = "qty".into();
        c.default = "0".into();
        c.nullable = false;
        d.cols.push(c);
        d.new_name = "orders2".into();
        assert!(d.has_changes());
        assert_eq!(
            sql(&d, DriverId::Postgres),
            vec!["ALTER TABLE \"orders\" DROP COLUMN \"note\"", "ALTER TABLE \"orders\" ADD COLUMN \"qty\" integer NOT NULL DEFAULT 0", "ALTER TABLE \"orders\" RENAME TO \"orders2\""]
        );
    }

    /// PostgreSQL alters only what changed.
    #[test]
    fn alter_column_postgres() {
        let mut d = StructDraft::from_structure(&orders());
        d.cols[1].type_name = "varchar(20)".into();
        d.cols[1].nullable = false;
        assert_eq!(sql(&d, DriverId::Postgres), vec!["ALTER TABLE \"orders\" ALTER COLUMN \"note\" TYPE varchar(20)", "ALTER TABLE \"orders\" ALTER COLUMN \"note\" SET NOT NULL"]);
    }

    /// Two alterations on SQLite rebuild the table twice, the second from the updated structure.
    #[test]
    fn sqlite_rebuilds_chain_on_the_updated_structure() {
        let mut d = StructDraft::from_structure(&orders());
        d.cols[1].name = "memo".into();
        d.cols[2].type_name = "bigint".into();
        let out = sql(&d, DriverId::Sqlite);
        let creates: Vec<&String> = out.iter().filter(|s| s.starts_with("CREATE TABLE")).collect();
        assert_eq!(creates.len(), 2);
        assert!(creates[0].contains("\"memo\" text"), "{}", creates[0]);
        assert!(creates[1].contains("\"memo\" text") && creates[1].contains("\"cid\" bigint"), "the second rebuild keeps the first change: {}", creates[1]);
        assert!(out.iter().any(|s| s.starts_with("CREATE INDEX \"ix_cid\"")));
    }

    /// Indexes: added, removed and edited (drop and create).
    #[test]
    fn index_changes() {
        let mut d = StructDraft::from_structure(&orders());
        d.indexes[0].unique = true;
        let mut n = IndexDraft::blank();
        n.name = "ix_note".into();
        n.columns = "note".into();
        d.indexes.push(n);
        assert_eq!(sql(&d, DriverId::Postgres), vec!["DROP INDEX \"ix_cid\"", "CREATE UNIQUE INDEX \"ix_cid\" ON \"orders\" (\"cid\")", "CREATE INDEX \"ix_note\" ON \"orders\" (\"note\")"]);
        d.indexes[0].deleted = true;
        d.indexes[1].deleted = true;
        assert_eq!(sql(&d, DriverId::MySql), vec!["DROP INDEX `ix_cid` ON `orders`"]);
    }

    /// Foreign keys need a known constraint name to be removed and are refused on SQLite.
    #[test]
    fn foreign_keys() {
        let mut st = orders();
        st.columns[2].fk = Some(ForeignKeyRef { table: TableRef::simple("customers"), columns: vec!["id".into()], on_update: "NO ACTION".into(), on_delete: "CASCADE".into(), name: Some("fk_c".into()) });
        let mut d = StructDraft::from_structure(&st);
        d.cols[2].fk = None;
        assert_eq!(sql(&d, DriverId::Postgres), vec!["ALTER TABLE \"orders\" DROP CONSTRAINT \"fk_c\""]);
        let mut st2 = st.clone();
        st2.columns[2].fk.as_mut().unwrap().name = None;
        let mut d2 = StructDraft::from_structure(&st2);
        d2.cols[2].fk = None;
        assert!(d2.to_ops(&Capabilities::for_driver(DriverId::MySql)).unwrap_err().contains("unknown"));
        let mut d3 = StructDraft::from_structure(&orders());
        d3.cols[1].fk = Some(FkDraft { name: None, ref_table: TableRef::simple("customers"), ref_columns: vec!["id".into()], on_update: "NO ACTION".into(), on_delete: "NO ACTION".into() });
        assert!(d3.to_ops(&Capabilities::for_driver(DriverId::Sqlite)).unwrap_err().contains("cannot add a foreign key"));
        assert_eq!(sql(&d3, DriverId::Postgres), vec!["ALTER TABLE \"orders\" ADD FOREIGN KEY (\"note\") REFERENCES \"customers\" (\"id\") ON UPDATE NO ACTION ON DELETE NO ACTION"]);
    }

    /// A new table becomes CREATE TABLE with its primary key and indexes.
    #[test]
    fn create_table() {
        let mut d = StructDraft::new_table(TableRef::simple("untitled_table_1"), "integer");
        d.new_name = "things".into();
        let mut c = ColDraft::blank("text");
        c.name = "label".into();
        d.cols.push(c);
        let mut i = IndexDraft::blank();
        i.name = "ix_label".into();
        i.columns = "label".into();
        d.indexes.push(i);
        assert!(d.has_changes());
        assert_eq!(
            sql(&d, DriverId::Sqlite),
            vec!["CREATE TABLE \"things\" (\n  \"id\" integer NOT NULL,\n  \"label\" text,\n  PRIMARY KEY (\"id\")\n)", "CREATE INDEX \"ix_label\" ON \"things\" (\"label\")"]
        );
    }

    /// Bad drafts are refused with a clear message.
    #[test]
    fn validation() {
        let mut d = StructDraft::from_structure(&orders());
        d.cols[1].name = "ID".into();
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).unwrap_err().contains("twice"));
        let mut d = StructDraft::from_structure(&orders());
        d.cols[1].type_name = " ".into();
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).unwrap_err().contains("data type"));
        let mut d = StructDraft::from_structure(&orders());
        d.indexes[0].columns = "ghost".into();
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).unwrap_err().contains("ghost"));
        let mut d = StructDraft::from_structure(&orders());
        d.new_name = "  ".into();
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).is_err());
        for c in &mut d.cols {
            c.deleted = true;
        }
        d.new_name = "x".into();
        assert!(d.to_ops(&Capabilities::for_driver(DriverId::Postgres)).unwrap_err().contains("at least one column"));
    }
}
