//! Pending schema changes made from the ER diagram (SPEC 11.3): drafts of new and edited tables, dropped
//! tables and foreign key changes, with Undo and Redo, translated to DDL when the user applies them.

use super::structdraft::StructDraft;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::ddl::{ForeignKeyDef, StructuralOp};
use dbzilla_core::model::{ColumnMeta, ForeignKeyRef, TableRef, TableStructure};
use std::collections::{BTreeMap, BTreeSet};

/// Undo steps kept.
const MAX_UNDO: usize = 100;

/// A foreign key to add to an existing table.
#[derive(Debug, Clone)]
struct FkAdd {
    table: String,
    fk: ForeignKeyDef,
}

/// A foreign key of an existing table to drop.
#[derive(Debug, Clone, PartialEq)]
struct FkDrop {
    table: String,
    constraint: String,
}

/// A table that does not exist yet.
#[derive(Debug, Clone)]
struct Created {
    id: u32,
    draft: StructDraft,
}

/// Everything that is pending, cloned for every Undo step.
#[derive(Debug, Clone, Default)]
struct State {
    /// Drafts of existing tables, by their original name.
    existing: BTreeMap<String, StructDraft>,
    created: Vec<Created>,
    dropped: BTreeSet<String>,
    fk_adds: Vec<FkAdd>,
    fk_drops: Vec<FkDrop>,
    next_id: u32,
}

/// What the diagram marks on the tables and lines.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Markers {
    pub created: BTreeSet<String>,
    pub dropped: BTreeSet<String>,
    pub edited: BTreeSet<String>,
    /// Foreign keys not in the database yet, as (table, column).
    pub new_fks: BTreeSet<(String, String)>,
}

/// The pending changes of a diagram.
#[derive(Debug, Default)]
pub struct DiagramPending {
    state: State,
    undo: Vec<State>,
    redo: Vec<State>,
    /// A settings session is open: its undo step is already taken.
    editing: bool,
}

/// Text form of a draft, used to tell whether an edit session changed anything.
fn snapshot_text(s: &State) -> String {
    format!("{s:?}")
}

/// Structure a draft stands for, as the diagram draws it.
fn structure_of(d: &StructDraft) -> TableStructure {
    let mut table = d.table.clone();
    table.name = d.new_name.trim().to_owned();
    let columns = d
        .cols
        .iter()
        .filter(|c| !c.deleted)
        .enumerate()
        .map(|(i, c)| ColumnMeta {
            name: c.name.trim().to_owned(),
            type_name: c.type_name.trim().to_owned(),
            nullable: c.nullable,
            default: (!c.default.trim().is_empty()).then(|| c.default.trim().to_owned()),
            is_pk: c.pk,
            fk: c.fk.as_ref().map(|f| ForeignKeyRef { table: f.ref_table.clone(), columns: f.ref_columns.clone(), on_update: f.on_update.clone(), on_delete: f.on_delete.clone(), name: f.name.clone() }),
            ordinal: i,
            extra: c.orig.as_ref().map(|o| o.extra.clone()).unwrap_or_default(),
        })
        .collect();
    TableStructure { table, columns, primary_key: d.pk_columns(), ..d.original.clone() }
}

impl DiagramPending {
    /// True when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.change_count() == 0
    }

    /// Number of pending changes: new, dropped and edited tables and foreign keys.
    pub fn change_count(&self) -> usize {
        let s = &self.state;
        s.created.len() + s.dropped.len() + s.existing.iter().filter(|(n, d)| !s.dropped.contains(*n) && d.has_changes()).count() + s.fk_adds.len() + s.fk_drops.len()
    }

    /// True when Undo has a step to go back to.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when Redo has a step to go forward to.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Forgets every pending change and the undo history.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Remembers the current state as an Undo step before a change.
    fn checkpoint(&mut self) {
        self.undo.push(self.state.clone());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Goes back one step; false when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.state, prev));
        self.editing = false;
        true
    }

    /// Goes forward one step; false when there is none.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.state, next));
        self.editing = false;
        true
    }

    /// Names of every table that exists in the diagram, with the pending ones, dropped tables left out.
    pub fn live_names(&self, base: &[TableStructure]) -> Vec<String> {
        self.effective(base).iter().map(|t| t.table.name.clone()).filter(|n| !self.state.dropped.contains(n)).collect()
    }

    /// The tables as the diagram draws them: the database structures with every pending change applied.
    pub fn effective(&self, base: &[TableStructure]) -> Vec<TableStructure> {
        let s = &self.state;
        let mut out: Vec<TableStructure> = base
            .iter()
            .map(|b| {
                let mut t = match s.existing.get(&b.table.name).filter(|d| d.has_changes()) {
                    Some(d) => structure_of(d),
                    None => b.clone(),
                };
                for drop in s.fk_drops.iter().filter(|d| d.table == b.table.name) {
                    for c in &mut t.columns {
                        if c.fk.as_ref().is_some_and(|f| f.name.as_deref() == Some(drop.constraint.as_str())) {
                            c.fk = None;
                        }
                    }
                }
                for add in s.fk_adds.iter().filter(|a| a.table == b.table.name) {
                    if let Some(c) = t.columns.iter_mut().find(|c| add.fk.columns.first() == Some(&c.name)) {
                        c.fk = Some(ForeignKeyRef { table: add.fk.ref_table.clone(), columns: add.fk.ref_columns.clone(), on_update: add.fk.on_update.clone(), on_delete: add.fk.on_delete.clone(), name: add.fk.name.clone() });
                    }
                }
                t
            })
            .collect();
        out.extend(s.created.iter().map(|c| structure_of(&c.draft)));
        out
    }

    /// What to mark on the tables and lines.
    pub fn markers(&self) -> Markers {
        let s = &self.state;
        Markers {
            created: s.created.iter().map(|c| c.draft.new_name.trim().to_owned()).collect(),
            dropped: s.dropped.clone(),
            edited: s.existing.iter().filter(|(n, d)| !s.dropped.contains(*n) && d.has_changes()).map(|(_, d)| d.new_name.trim().to_owned()).collect(),
            new_fks: s.fk_adds.iter().filter_map(|a| Some((a.table.clone(), a.fk.columns.first()?.clone()))).collect(),
        }
    }

    /// Adds a table called `untitled_table_N` (the first free N) and returns its name.
    pub fn add_table(&mut self, base: &[TableStructure], template: &TableRef, int_type: &str) -> String {
        let taken = self.live_names(base);
        let name = (1..).map(|n| format!("untitled_table_{n}")).find(|n| !taken.contains(n)).unwrap_or_default();
        self.checkpoint();
        let id = self.state.next_id;
        self.state.next_id += 1;
        let table = TableRef { name: name.clone(), ..template.clone() };
        self.state.created.push(Created { id, draft: StructDraft::new_table(table, int_type) });
        name
    }

    /// Marks a table to be dropped; a table that is not in the database yet is simply forgotten.
    pub fn drop_table(&mut self, name: &str) {
        self.checkpoint();
        let s = &mut self.state;
        if let Some(i) = s.created.iter().position(|c| c.draft.new_name.trim() == name) {
            s.created.remove(i);
        } else {
            s.dropped.insert(name.to_owned());
            s.existing.remove(name);
            s.fk_drops.retain(|d| d.table != name);
        }
        s.fk_adds.retain(|a| a.table != name && a.fk.ref_table.name != name);
        for c in &mut s.created {
            for col in &mut c.draft.cols {
                if col.fk.as_ref().is_some_and(|f| f.ref_table.name == name) {
                    col.fk = None;
                }
            }
        }
    }

    /// Starts editing a table in the settings window: takes the Undo step and makes sure the draft exists.
    /// Returns false when the table is unknown or dropped.
    pub fn begin_edit(&mut self, base: &[TableStructure], name: &str) -> bool {
        if self.state.dropped.contains(name) {
            return false;
        }
        let known = self.state.created.iter().any(|c| c.draft.new_name.trim() == name) || base.iter().any(|b| b.table.name == name);
        if !known {
            return false;
        }
        if !self.editing {
            self.checkpoint();
            self.editing = true;
        }
        if !self.state.created.iter().any(|c| c.draft.new_name.trim() == name) && !self.state.existing.contains_key(name) {
            if let Some(b) = base.iter().find(|b| b.table.name == name) {
                self.state.existing.insert(name.to_owned(), StructDraft::from_structure(b));
            }
        }
        true
    }

    /// The draft of a table being edited. Call `begin_edit` first.
    pub fn draft_mut(&mut self, name: &str) -> Option<&mut StructDraft> {
        let s = &mut self.state;
        match s.created.iter_mut().find(|c| c.draft.new_name.trim() == name) {
            Some(c) => Some(&mut c.draft),
            None => s.existing.get_mut(name),
        }
    }

    /// Text that changes whenever the pending state changes.
    pub fn signature(&self) -> String {
        snapshot_text(&self.state)
    }

    /// Ends a settings session; the Undo step is dropped when nothing changed.
    pub fn end_edit(&mut self) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.state.existing.retain(|_, d| d.has_changes());
        if self.undo.last().is_some_and(|prev| snapshot_text(prev) == snapshot_text(&self.state)) {
            self.undo.pop();
        }
    }

    /// Adds a foreign key from one column to another through the queue.
    #[allow(clippy::too_many_arguments)]
    pub fn stage_fk(&mut self, base: &[TableStructure], from: (&str, &str), to: (&str, &str), name: Option<String>, on_update: &str, on_delete: &str) -> Result<(), String> {
        let tables = self.effective(base);
        let find = |t: &str| tables.iter().find(|s| s.table.name == t && !self.state.dropped.contains(t));
        let (Some(src), Some(dst)) = (find(from.0), find(to.0)) else { return Err("The table is no longer in the diagram".into()) };
        if !src.columns.iter().any(|c| c.name == from.1) || !dst.columns.iter().any(|c| c.name == to.1) {
            return Err("The column is no longer in the table".into());
        }
        let fk = ForeignKeyDef { name, columns: vec![from.1.to_owned()], ref_table: dst.table.clone(), ref_columns: vec![to.1.to_owned()], on_update: on_update.to_owned(), on_delete: on_delete.to_owned() };
        self.checkpoint();
        if let Some(c) = self.state.created.iter_mut().find(|c| c.draft.new_name.trim() == from.0) {
            if let Some(col) = c.draft.cols.iter_mut().find(|col| !col.deleted && col.name.trim() == from.1) {
                col.fk = Some(super::structdraft::FkDraft { name: fk.name, ref_table: fk.ref_table, ref_columns: fk.ref_columns, on_update: fk.on_update, on_delete: fk.on_delete });
            }
        } else {
            self.state.fk_adds.retain(|a| !(a.table == from.0 && a.fk.columns == fk.columns));
            self.state.fk_adds.push(FkAdd { table: from.0.to_owned(), fk });
        }
        Ok(())
    }

    /// Drops a foreign key of an existing table through the queue; one added in this session is just forgotten.
    pub fn stage_drop_fk(&mut self, table: &str, constraint: &str) {
        self.checkpoint();
        let s = &mut self.state;
        let before = s.fk_adds.len();
        s.fk_adds.retain(|a| !(a.table == table && a.fk.name.as_deref() == Some(constraint)));
        if s.fk_adds.len() == before {
            let drop = FkDrop { table: table.to_owned(), constraint: constraint.to_owned() };
            if !s.fk_drops.contains(&drop) {
                s.fk_drops.push(drop);
            }
        }
    }

    /// Forgets a foreign key that is not in the database yet.
    pub fn unstage_fk(&mut self, table: &str, column: &str) {
        self.checkpoint();
        let s = &mut self.state;
        s.fk_adds.retain(|a| !(a.table == table && a.fk.columns.first().is_some_and(|c| c == column)));
        if let Some(c) = s.created.iter_mut().find(|c| c.draft.new_name.trim() == table) {
            for col in c.draft.cols.iter_mut().filter(|col| col.name.trim() == column) {
                col.fk = None;
            }
        }
    }

    /// Translates the pending changes into operations, in an order the database accepts:
    /// foreign keys out, new tables (referenced ones first), table changes, foreign keys in, dropped tables.
    pub fn to_ops(&self, base: &[TableStructure], caps: &Capabilities, sqlite: bool) -> Result<Vec<StructuralOp>, String> {
        let s = &self.state;
        let live = self.live_names(base);
        let mut ops = Vec::new();
        for d in &s.fk_drops {
            ops.push(StructuralOp::DropForeignKey { table: table_of(base, &d.table)?, name: d.constraint.clone() });
        }
        for c in &s.created {
            for col in c.draft.cols.iter().filter(|col| !col.deleted) {
                if let Some(f) = &col.fk {
                    check_target(&live, &f.ref_table.name)?;
                }
            }
        }
        for c in created_in_order(&s.created)? {
            ops.extend(c.draft.to_ops(caps)?);
        }
        for (name, d) in s.existing.iter().filter(|(n, d)| !s.dropped.contains(*n) && d.has_changes()) {
            if d.new_name.trim() != name && s.fk_adds.iter().any(|a| &a.table == name || &a.fk.ref_table.name == name) {
                return Err(format!("Apply the rename of {name} before adding foreign keys that involve it"));
            }
            if sqlite && s.fk_adds.iter().any(|a| &a.table == name) {
                return Err(format!("SQLite rebuilds {name} to add a foreign key: apply its structure changes first"));
            }
            ops.extend(d.to_ops(caps)?);
        }
        for a in &s.fk_adds {
            check_target(&live, &a.fk.ref_table.name)?;
            let current = base.iter().find(|b| b.table.name == a.table).cloned().map(Box::new);
            ops.push(StructuralOp::AddForeignKey { table: table_of(base, &a.table)?, fk: a.fk.clone(), current });
        }
        let mut dropped: Vec<&String> = s.dropped.iter().collect();
        // a table that others in the list point to goes last
        let pointed_at = |n: &String| base.iter().filter(|b| s.dropped.contains(&b.table.name) && b.columns.iter().any(|c| c.fk.as_ref().is_some_and(|f| &f.table.name == n))).count();
        dropped.sort_by_key(|n| pointed_at(n));
        for n in dropped {
            ops.push(StructuralOp::DropTable { table: table_of(base, n)?, cascade: false });
        }
        Ok(ops)
    }
}

/// Reference of an existing table.
fn table_of(base: &[TableStructure], name: &str) -> Result<TableRef, String> {
    base.iter().find(|b| b.table.name == name).map(|b| b.table.clone()).ok_or_else(|| format!("The table {name} is no longer in the schema"))
}

/// Fails when a foreign key points to a table that will not exist.
fn check_target(live: &[String], name: &str) -> Result<(), String> {
    if live.iter().any(|n| n == name) { Ok(()) } else { Err(format!("A foreign key points to {name}, which is not in the diagram any more")) }
}

/// New tables ordered so that a table comes after the new tables its foreign keys point to.
fn created_in_order(created: &[Created]) -> Result<Vec<&Created>, String> {
    let names: Vec<String> = created.iter().map(|c| c.draft.new_name.trim().to_owned()).collect();
    let deps = |c: &Created| -> Vec<String> { c.draft.cols.iter().filter(|col| !col.deleted).filter_map(|col| col.fk.as_ref()).map(|f| f.ref_table.name.clone()).filter(|n| names.contains(n) && n != c.draft.new_name.trim()).collect() };
    let mut done: Vec<String> = Vec::new();
    let mut out: Vec<&Created> = Vec::new();
    while out.len() < created.len() {
        let next = created.iter().find(|c| !out.iter().any(|o| o.id == c.id) && deps(c).iter().all(|d| done.contains(d)));
        let Some(c) = next else {
            let stuck: Vec<String> = created.iter().filter(|c| !out.iter().any(|o| o.id == c.id)).map(|c| c.draft.new_name.trim().to_owned()).collect();
            return Err(format!("The new tables {} point to each other: remove one of the foreign keys", stuck.join(", ")));
        };
        done.push(c.draft.new_name.trim().to_owned());
        out.push(c);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), type_name: ty.into(), nullable: !pk, is_pk: pk, ..Default::default() }
    }

    fn table(name: &str, cols: Vec<ColumnMeta>) -> TableStructure {
        let primary_key = cols.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
        TableStructure { table: TableRef::simple(name), columns: cols, primary_key, ..Default::default() }
    }

    fn base() -> Vec<TableStructure> {
        vec![table("customers", vec![col("id", "integer", true), col("name", "text", false)]), table("orders", vec![col("id", "integer", true), col("cid", "integer", false)])]
    }

    fn caps() -> Capabilities {
        Capabilities::for_driver(dbzilla_core::dialect::DriverId::Postgres)
    }

    #[test]
    fn a_new_table_gets_the_first_free_name_and_shows_up() {
        let mut p = DiagramPending::default();
        let b = base();
        let first = p.add_table(&b, &TableRef::simple(""), "integer");
        let second = p.add_table(&b, &TableRef::simple(""), "integer");
        assert_eq!((first.as_str(), second.as_str()), ("untitled_table_1", "untitled_table_2"));
        assert_eq!(p.change_count(), 2);
        assert_eq!(p.effective(&b).len(), 4);
        assert_eq!(p.markers().created.len(), 2);
    }

    #[test]
    fn undo_and_redo_walk_through_the_changes() {
        let mut p = DiagramPending::default();
        let b = base();
        p.add_table(&b, &TableRef::simple(""), "integer");
        p.drop_table("orders");
        assert_eq!(p.change_count(), 2);
        assert!(p.undo());
        assert_eq!(p.change_count(), 1);
        assert!(p.undo());
        assert!(p.is_empty());
        assert!(!p.undo());
        assert!(p.redo() && p.redo());
        assert_eq!(p.change_count(), 2);
        assert!(!p.redo());
    }

    #[test]
    fn a_new_change_clears_redo() {
        let mut p = DiagramPending::default();
        let b = base();
        p.add_table(&b, &TableRef::simple(""), "integer");
        p.undo();
        p.drop_table("orders");
        assert!(!p.can_redo());
    }

    #[test]
    fn dropping_a_new_table_forgets_it_and_dropping_an_existing_one_marks_it() {
        let mut p = DiagramPending::default();
        let b = base();
        let n = p.add_table(&b, &TableRef::simple(""), "integer");
        p.drop_table(&n);
        assert!(p.is_empty());
        p.drop_table("orders");
        assert!(p.markers().dropped.contains("orders"));
        assert!(!p.live_names(&b).contains(&"orders".to_owned()));
        let ops = p.to_ops(&b, &caps(), false).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::DropTable { table, .. }] if table.name == "orders"));
    }

    #[test]
    fn a_settings_session_that_changes_nothing_leaves_no_undo_step() {
        let mut p = DiagramPending::default();
        let b = base();
        assert!(p.begin_edit(&b, "orders"));
        p.end_edit();
        assert!(!p.can_undo());
        assert!(p.is_empty(), "an untouched draft is not a change");
    }

    #[test]
    fn editing_a_table_is_one_undo_step_and_shows_in_the_diagram() {
        let mut p = DiagramPending::default();
        let b = base();
        p.begin_edit(&b, "orders");
        p.draft_mut("orders").unwrap().new_name = "sales".into();
        p.end_edit();
        assert!(p.can_undo());
        assert_eq!(p.change_count(), 1);
        assert!(p.effective(&b).iter().any(|t| t.table.name == "sales"));
        let ops = p.to_ops(&b, &caps(), false).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::RenameTable { new_name, .. }] if new_name == "sales"));
        assert!(p.undo());
        assert!(p.is_empty());
    }

    #[test]
    fn a_foreign_key_between_existing_tables_is_added_and_can_be_forgotten() {
        let mut p = DiagramPending::default();
        let b = base();
        p.stage_fk(&b, ("orders", "cid"), ("customers", "id"), Some("fk_c".into()), "NO ACTION", "CASCADE").unwrap();
        assert!(p.markers().new_fks.contains(&("orders".to_owned(), "cid".to_owned())));
        assert!(p.effective(&b)[1].columns[1].fk.is_some());
        let ops = p.to_ops(&b, &caps(), false).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::AddForeignKey { table, fk, .. }] if table.name == "orders" && fk.name.as_deref() == Some("fk_c")));
        p.unstage_fk("orders", "cid");
        assert!(p.is_empty());
    }

    #[test]
    fn deleting_a_real_foreign_key_hides_the_line_and_drops_it_first() {
        let mut p = DiagramPending::default();
        let mut b = base();
        b[1].columns[1].fk = Some(ForeignKeyRef { table: TableRef::simple("customers"), columns: vec!["id".into()], on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), name: Some("fk_c".into()) });
        p.stage_drop_fk("orders", "fk_c");
        p.stage_drop_fk("orders", "fk_c");
        assert_eq!(p.change_count(), 1, "the same drop is not counted twice");
        assert!(p.effective(&b)[1].columns[1].fk.is_none());
        let ops = p.to_ops(&b, &caps(), false).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::DropForeignKey { name, .. }] if name == "fk_c"));
    }

    #[test]
    fn a_foreign_key_from_a_new_table_is_part_of_its_create_and_goes_after_its_target() {
        let mut p = DiagramPending::default();
        let b = base();
        let a = p.add_table(&b, &TableRef::simple(""), "integer");
        let c = p.add_table(&b, &TableRef::simple(""), "integer");
        // the first new table points to the second one: the second must be created first
        p.stage_fk(&b, (&a, "id"), (&c, "id"), None, "NO ACTION", "NO ACTION").unwrap();
        let ops = p.to_ops(&b, &caps(), false).unwrap();
        let created: Vec<&str> = ops.iter().filter_map(|o| if let StructuralOp::CreateTable { table, .. } = o { Some(table.name.as_str()) } else { None }).collect();
        assert_eq!(created, vec![c.as_str(), a.as_str()]);
    }

    #[test]
    fn new_tables_that_point_to_each_other_are_refused() {
        let mut p = DiagramPending::default();
        let b = base();
        let a = p.add_table(&b, &TableRef::simple(""), "integer");
        let c = p.add_table(&b, &TableRef::simple(""), "integer");
        p.stage_fk(&b, (&a, "id"), (&c, "id"), None, "NO ACTION", "NO ACTION").unwrap();
        p.stage_fk(&b, (&c, "id"), (&a, "id"), None, "NO ACTION", "NO ACTION").unwrap();
        assert!(p.to_ops(&b, &caps(), false).unwrap_err().contains("point to each other"));
    }

    #[test]
    fn a_foreign_key_to_a_dropped_table_is_refused_and_dropping_forgets_the_pending_ones() {
        let mut p = DiagramPending::default();
        let b = base();
        assert!(p.stage_fk(&b, ("orders", "cid"), ("customers", "id"), None, "NO ACTION", "NO ACTION").is_ok());
        p.drop_table("customers");
        assert!(p.markers().new_fks.is_empty(), "the foreign key to the dropped table is forgotten");
        assert!(p.stage_fk(&b, ("orders", "cid"), ("customers", "id"), None, "NO ACTION", "NO ACTION").is_err());
    }

    #[test]
    fn sqlite_refuses_a_foreign_key_on_a_table_with_other_changes() {
        let mut p = DiagramPending::default();
        let b = base();
        p.begin_edit(&b, "orders");
        p.draft_mut("orders").unwrap().cols[1].name = "customer_id".into();
        p.end_edit();
        p.stage_fk(&b, ("orders", "customer_id"), ("customers", "id"), None, "NO ACTION", "NO ACTION").unwrap();
        assert!(p.to_ops(&b, &caps(), true).is_err());
        assert!(p.to_ops(&b, &caps(), false).is_ok());
    }

    #[test]
    fn tables_that_point_to_other_dropped_tables_are_dropped_first() {
        let mut p = DiagramPending::default();
        let mut b = base();
        b[1].columns[1].fk = Some(ForeignKeyRef { table: TableRef::simple("customers"), columns: vec!["id".into()], on_update: String::new(), on_delete: String::new(), name: Some("fk".into()) });
        p.drop_table("customers");
        p.drop_table("orders");
        let order: Vec<String> = p.to_ops(&b, &caps(), false).unwrap().iter().filter_map(|o| if let StructuralOp::DropTable { table, .. } = o { Some(table.name.clone()) } else { None }).collect();
        assert_eq!(order, vec!["orders", "customers"]);
    }
}
