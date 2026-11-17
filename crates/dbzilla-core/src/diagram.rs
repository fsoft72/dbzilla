//! ER diagram model (SPEC 11.3): tables, foreign key edges, colour groups and an automatic layout.
//! Pure data and geometry, no UI and no I/O.

use crate::model::TableStructure;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Gap between two tables in the automatic layout.
const GAP_X: f32 = 80.0;
const GAP_Y: f32 = 40.0;

/// A column of a table box.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramColumn {
    pub name: String,
    pub type_name: String,
    pub is_pk: bool,
    pub is_fk: bool,
}

/// A table box.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramTable {
    pub name: String,
    pub columns: Vec<DiagramColumn>,
}

/// Where a line comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// A foreign key of the database.
    Real,
    /// Guessed from the column name.
    Inferred,
    /// Drawn by the user and kept in the application only.
    Manual,
}

/// A link drawn by the user, by table and column name. It exists in the diagram only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocLink {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

/// A foreign key line from a column to the column it references.
/// `to_column` is `None` when the target column is unknown, so the line ends on the table header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramEdge {
    pub from_table: usize,
    pub from_column: usize,
    pub to_table: usize,
    pub to_column: Option<usize>,
    pub kind: EdgeKind,
}

/// Order of the tables in the diagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiagramSort {
    #[default]
    Name,
    ColumnCount,
}

/// Tables and foreign keys of one schema.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Diagram {
    pub tables: Vec<DiagramTable>,
    pub edges: Vec<DiagramEdge>,
}

impl Diagram {
    /// Builds the diagram of the given tables. Foreign keys to tables that are not in the list are ignored.
    pub fn build(structures: &[TableStructure], sort: DiagramSort) -> Diagram {
        let mut ordered: Vec<&TableStructure> = structures.iter().collect();
        match sort {
            DiagramSort::Name => ordered.sort_by_key(|s| s.table.name.to_lowercase()),
            DiagramSort::ColumnCount => ordered.sort_by(|a, b| b.columns.len().cmp(&a.columns.len()).then_with(|| a.table.name.to_lowercase().cmp(&b.table.name.to_lowercase()))),
        }
        let tables = ordered
            .iter()
            .map(|s| DiagramTable {
                name: s.table.name.clone(),
                columns: s.columns.iter().map(|c| DiagramColumn { name: c.name.clone(), type_name: c.type_name.clone(), is_pk: c.is_pk, is_fk: c.fk.is_some() }).collect(),
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        for (from_table, s) in ordered.iter().enumerate() {
            for (from_column, col) in s.columns.iter().enumerate() {
                let Some(fk) = &col.fk else { continue };
                let Some(to_table) = ordered.iter().position(|t| t.table.name == fk.table.name) else { continue };
                let target = ordered[to_table];
                let wanted = fk.columns.first().cloned().or_else(|| target.primary_key.first().cloned());
                let to_column = wanted.and_then(|w| target.columns.iter().position(|c| c.name == w));
                edges.push(DiagramEdge { from_table, from_column, to_table, to_column, kind: EdgeKind::Real });
            }
        }
        let mut diagram = Diagram { tables, edges };
        diagram.infer_edges(&ordered);
        diagram
    }

    /// Adds the links drawn by the user. A link whose table or column no longer exists is skipped, and one
    /// that repeats a foreign key of the database is ignored. A guessed link between the same columns is replaced.
    pub fn add_manual(&mut self, links: &[DocLink]) {
        for l in links {
            let find_table = |name: &str| self.tables.iter().position(|t| t.name == name);
            let (Some(from_table), Some(to_table)) = (find_table(&l.from_table), find_table(&l.to_table)) else { continue };
            let find_col = |t: usize, name: &str| self.tables[t].columns.iter().position(|c| c.name == name);
            let (Some(from_column), Some(to_column)) = (find_col(from_table, &l.from_column), find_col(to_table, &l.to_column)) else { continue };
            let same = |e: &DiagramEdge| e.from_table == from_table && e.from_column == from_column && e.to_table == to_table;
            if self.edges.iter().any(|e| same(e) && e.kind != EdgeKind::Inferred) {
                continue;
            }
            self.edges.retain(|e| !(same(e) && e.kind == EdgeKind::Inferred));
            self.edges.push(DiagramEdge { from_table, from_column, to_table, to_column: Some(to_column), kind: EdgeKind::Manual });
        }
    }

    /// Adds a dashed link for every column that looks like a reference but has no foreign key:
    /// `id_user`, `user_id` or `userId` point to the single primary key of `user`, `users` or `user_` + `s`.
    /// The column and the key must have the same kind of type. Only real edges drive groups and layout.
    fn infer_edges(&mut self, structures: &[&TableStructure]) {
        let mut found = Vec::new();
        for (from_table, s) in structures.iter().enumerate() {
            for (from_column, col) in s.columns.iter().enumerate() {
                if col.fk.is_some() || (col.is_pk && s.primary_key.len() == 1) {
                    continue;
                }
                let Some(stem) = reference_stem(&col.name) else { continue };
                let candidates = table_name_candidates(&stem);
                let Some(to_table) = structures.iter().position(|t| candidates.contains(&t.table.name.to_lowercase())) else { continue };
                if to_table == from_table {
                    continue;
                }
                let target = structures[to_table];
                let [key] = target.primary_key.as_slice() else { continue };
                let Some(to_column) = target.columns.iter().position(|c| &c.name == key) else { continue };
                if type_family(&col.type_name) != type_family(&target.columns[to_column].type_name) {
                    continue;
                }
                found.push(DiagramEdge { from_table, from_column, to_table, to_column: Some(to_column), kind: EdgeKind::Inferred });
            }
        }
        self.edges.extend(found);
    }

    /// Colour group of each table: tables linked by foreign keys share a group, the others get `None`.
    /// Groups are numbered from 0 in order of first appearance.
    pub fn groups(&self) -> Vec<Option<usize>> {
        let mut parent: Vec<usize> = (0..self.tables.len()).collect();
        let mut linked = vec![false; self.tables.len()];
        for e in self.edges.iter().filter(|e| e.from_table != e.to_table && e.kind == EdgeKind::Real) {
            linked[e.from_table] = true;
            linked[e.to_table] = true;
            let (a, b) = (find(&mut parent, e.from_table), find(&mut parent, e.to_table));
            if a != b {
                parent[a] = b;
            }
        }
        let mut ids: Vec<(usize, usize)> = Vec::new();
        (0..self.tables.len())
            .map(|i| {
                if !linked[i] {
                    return None;
                }
                let root = find(&mut parent, i);
                if let Some((_, id)) = ids.iter().find(|(r, _)| *r == root) {
                    return Some(*id);
                }
                ids.push((root, ids.len()));
                Some(ids.len() - 1)
            })
            .collect()
    }

    /// Indexes of the tables whose name or a column name contains `needle` (case insensitive).
    pub fn search(&self, needle: &str) -> Vec<usize> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        (0..self.tables.len()).filter(|&i| self.tables[i].name.to_lowercase().contains(&needle) || self.tables[i].columns.iter().any(|c| c.name.to_lowercase().contains(&needle))).collect()
    }

    /// Top-left position of every table given the size of each box. Referenced tables go to the left of the
    /// tables that reference them; linked tables are laid out together and the groups are packed in rows.
    pub fn layout(&self, sizes: &[(f32, f32)]) -> Vec<(f32, f32)> {
        let n = self.tables.len();
        if n == 0 || sizes.len() != n {
            return Vec::new();
        }
        let mut parent: Vec<usize> = (0..n).collect();
        for e in self.edges.iter().filter(|e| e.from_table != e.to_table && e.kind == EdgeKind::Real) {
            let (a, b) = (find(&mut parent, e.from_table), find(&mut parent, e.to_table));
            if a != b {
                parent[a] = b;
            }
        }
        let mut components: Vec<Vec<usize>> = Vec::new();
        let mut roots: Vec<usize> = Vec::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            match roots.iter().position(|r| *r == root) {
                Some(k) => components[k].push(i),
                None => {
                    roots.push(root);
                    components.push(vec![i]);
                }
            }
        }
        // Biggest blocks first, so the rows are filled with the small ones at the end.
        components.sort_by_key(|c| std::cmp::Reverse(c.len()));
        let blocks: Vec<Block> = components.iter().map(|c| self.layout_component(c, sizes)).collect();
        let total_area: f32 = blocks.iter().map(|b| (b.width + GAP_X) * (b.height + GAP_Y)).sum();
        let widest = blocks.iter().map(|b| b.width).fold(0.0, f32::max);
        let row_width = widest.max(total_area.sqrt() * 1.6);
        let mut out = vec![(0.0, 0.0); n];
        let (mut x, mut y, mut row_height) = (0.0_f32, 0.0_f32, 0.0_f32);
        for block in &blocks {
            if x > 0.0 && x + block.width > row_width {
                x = 0.0;
                y += row_height + GAP_Y;
                row_height = 0.0;
            }
            for (t, (dx, dy)) in &block.positions {
                out[*t] = (x + dx, y + dy);
            }
            x += block.width + GAP_X;
            row_height = row_height.max(block.height);
        }
        out
    }

    /// Puts the saved position on every table that has one. The other tables keep the position
    /// they have and are moved below everything else, in a row, when they would sit on a saved table.
    pub fn apply_saved(&self, positions: &mut [(f32, f32)], sizes: &[(f32, f32)], saved: &BTreeMap<String, (f32, f32)>) {
        if positions.len() != self.tables.len() || sizes.len() != positions.len() || saved.is_empty() {
            return;
        }
        let mut placed = vec![false; positions.len()];
        for (i, t) in self.tables.iter().enumerate() {
            if let Some(p) = saved.get(&t.name) {
                positions[i] = *p;
                placed[i] = true;
            }
        }
        let overlaps = |a: usize, b: usize, pos: &[(f32, f32)]| pos[a].0 < pos[b].0 + sizes[b].0 && pos[b].0 < pos[a].0 + sizes[a].0 && pos[a].1 < pos[b].1 + sizes[b].1 && pos[b].1 < pos[a].1 + sizes[a].1;
        let mut bottom = (0..positions.len()).filter(|&i| placed[i]).map(|i| positions[i].1 + sizes[i].1).fold(0.0, f32::max);
        let left = (0..positions.len()).filter(|&i| placed[i]).map(|i| positions[i].0).fold(f32::MAX, f32::min);
        let mut x = left;
        let clash: Vec<usize> = (0..positions.len()).filter(|&i| !placed[i] && (0..positions.len()).any(|j| placed[j] && overlaps(i, j, positions))).collect();
        if clash.is_empty() {
            return;
        }
        bottom += GAP_Y;
        for i in clash {
            positions[i] = (x, bottom);
            x += sizes[i].0 + GAP_X;
        }
    }

    /// Layered layout of the tables of one linked group.
    fn layout_component(&self, members: &[usize], sizes: &[(f32, f32)]) -> Block {
        let mut rank = vec![0_usize; self.tables.len()];
        // A table sits right of the tables it references. The relaxation is capped, so a cycle cannot loop.
        for _ in 0..members.len() {
            let mut changed = false;
            for e in self.edges.iter().filter(|e| e.from_table != e.to_table && e.kind == EdgeKind::Real && members.contains(&e.from_table)) {
                if rank[e.from_table] < rank[e.to_table] + 1 && rank[e.to_table] + 1 < members.len() {
                    rank[e.from_table] = rank[e.to_table] + 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let depth = members.iter().map(|&t| rank[t]).max().unwrap_or(0);
        let mut layers: Vec<Vec<usize>> = vec![Vec::new(); depth + 1];
        for &t in members {
            layers[rank[t]].push(t);
        }
        // Order each layer by the average slot of the neighbours in the previous one (one pass).
        let mut slot = vec![0.0_f32; self.tables.len()];
        for (li, layer) in layers.iter_mut().enumerate() {
            if li > 0 {
                let key = |t: usize| -> f32 {
                    let neighbours: Vec<f32> = self.edges.iter().filter(|e| e.from_table == t && e.kind == EdgeKind::Real && rank[e.to_table] < li).map(|e| slot[e.to_table]).collect();
                    if neighbours.is_empty() { f32::MAX } else { neighbours.iter().sum::<f32>() / neighbours.len() as f32 }
                };
                layer.sort_by(|a, b| key(*a).partial_cmp(&key(*b)).unwrap_or(std::cmp::Ordering::Equal));
            }
            for (k, &t) in layer.iter().enumerate() {
                slot[t] = k as f32;
            }
        }
        let mut positions = Vec::with_capacity(members.len());
        let (mut x, mut height) = (0.0_f32, 0.0_f32);
        for layer in &layers {
            let layer_width = layer.iter().map(|&t| sizes[t].0).fold(0.0, f32::max);
            let mut y = 0.0_f32;
            for &t in layer {
                positions.push((t, (x, y)));
                y += sizes[t].1 + GAP_Y;
            }
            height = height.max(y - GAP_Y);
            x += layer_width + GAP_X;
        }
        Block { positions, width: (x - GAP_X).max(0.0), height: height.max(0.0) }
    }
}

/// The name a reference column talks about: `id_user` and `user_id` give `user`, `userId` gives `user`.
/// `None` when the name is not of that shape (plain `id`, no stem, ...).
fn reference_stem(column: &str) -> Option<String> {
    let lower = column.to_lowercase();
    let stem = lower.strip_prefix("id_").or_else(|| lower.strip_suffix("_id")).map(str::to_owned).or_else(|| {
        // camelCase: the original name ends with "Id" after a lower case letter
        let head = column.strip_suffix("Id")?;
        head.chars().last().is_some_and(|c| c.is_lowercase()).then(|| head.to_lowercase())
    })?;
    (!stem.is_empty()).then_some(stem)
}

/// Lower case table names a stem may refer to: the stem, its plurals.
fn table_name_candidates(stem: &str) -> Vec<String> {
    let mut v = vec![stem.to_owned(), format!("{stem}s"), format!("{stem}es")];
    if let Some(base) = stem.strip_suffix('y') {
        v.push(format!("{base}ies"));
    }
    v
}

/// Coarse kind of a SQL type, to compare a reference column with the key it would point to.
#[derive(Debug, PartialEq, Eq)]
enum TypeFamily {
    Int,
    Text,
    Other(String),
}

/// Kind of a type name such as `text(64)`, `INTEGER` or `bigint unsigned`.
fn type_family(type_name: &str) -> TypeFamily {
    let lower = type_name.to_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("");
    match base {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "serial" | "bigserial" | "smallserial" | "int2" | "int4" | "int8" => TypeFamily::Int,
        "text" | "varchar" | "char" | "character" | "nvarchar" | "nchar" | "tinytext" | "mediumtext" | "longtext" | "citext" | "uuid" => TypeFamily::Text,
        other => TypeFamily::Other(other.to_owned()),
    }
}

/// A laid out group of tables, positions relative to the block origin.
struct Block {
    positions: Vec<(usize, (f32, f32))>,
    width: f32,
    height: f32,
}

/// Union-find root with path halving.
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnMeta, ForeignKeyRef, TableRef};

    fn col(name: &str, pk: bool, fk: Option<(&str, &str)>) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            type_name: "int".into(),
            is_pk: pk,
            fk: fk.map(|(t, c)| ForeignKeyRef { table: TableRef::simple(t), columns: if c.is_empty() { vec![] } else { vec![c.into()] }, on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), name: None }),
            ..Default::default()
        }
    }

    fn table(name: &str, columns: Vec<ColumnMeta>) -> TableStructure {
        let primary_key = columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()).collect();
        TableStructure { table: TableRef::simple(name), columns, primary_key, ..Default::default() }
    }

    fn shop() -> Vec<TableStructure> {
        vec![
            table("orders", vec![col("id", true, None), col("customer_id", false, Some(("customers", "id")))]),
            table("customers", vec![col("id", true, None), col("name", false, None)]),
            table("items", vec![col("id", true, None), col("order_id", false, Some(("orders", "id")))]),
            table("audit", vec![col("id", true, None)]),
        ]
    }

    #[test]
    fn build_sorts_by_name_and_resolves_edges() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let names: Vec<_> = d.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["audit", "customers", "items", "orders"]);
        assert_eq!(d.edges.len(), 2);
        assert!(d.edges.contains(&DiagramEdge { from_table: 3, from_column: 1, to_table: 1, to_column: Some(0), kind: EdgeKind::Real }));
        assert!(d.edges.contains(&DiagramEdge { from_table: 2, from_column: 1, to_table: 3, to_column: Some(0), kind: EdgeKind::Real }));
    }

    #[test]
    fn build_sorts_by_column_count() {
        let mut s = shop();
        s.push(table("wide", vec![col("a", true, None), col("b", false, None), col("c", false, None)]));
        let d = Diagram::build(&s, DiagramSort::ColumnCount);
        assert_eq!(d.tables[0].name, "wide");
    }

    #[test]
    fn fk_without_target_column_uses_the_primary_key() {
        let s = vec![table("a", vec![col("x", false, Some(("b", "")))]), table("b", vec![col("id", true, None)])];
        let d = Diagram::build(&s, DiagramSort::Name);
        assert_eq!(d.edges[0].to_column, Some(0));
    }

    #[test]
    fn fk_to_unknown_table_is_ignored() {
        let s = vec![table("a", vec![col("x", false, Some(("elsewhere", "id")))])];
        let d = Diagram::build(&s, DiagramSort::Name);
        assert!(d.edges.is_empty());
        assert!(d.tables[0].columns[0].is_fk);
    }

    #[test]
    fn linked_tables_share_a_group_and_the_others_have_none() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let g = d.groups();
        assert_eq!(g[0], None);
        assert_eq!(g[1], g[2]);
        assert_eq!(g[2], g[3]);
        assert_eq!(g[1], Some(0));
    }

    #[test]
    fn a_self_reference_does_not_make_a_group() {
        let s = vec![table("tree", vec![col("id", true, None), col("parent", false, Some(("tree", "id")))])];
        let d = Diagram::build(&s, DiagramSort::Name);
        assert_eq!(d.groups(), vec![None]);
    }

    #[test]
    fn two_separate_clusters_get_two_groups() {
        let s = vec![
            table("a", vec![col("id", true, None)]),
            table("b", vec![col("a_id", false, Some(("a", "id")))]),
            table("c", vec![col("id", true, None)]),
            table("d", vec![col("c_id", false, Some(("c", "id")))]),
        ];
        let g = Diagram::build(&s, DiagramSort::Name).groups();
        assert_eq!(g, vec![Some(0), Some(0), Some(1), Some(1)]);
    }

    #[test]
    fn search_matches_table_and_column_names() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        assert_eq!(d.search("CUST"), vec![1, 3]);
        assert!(d.search("  ").is_empty());
    }

    #[test]
    fn layout_puts_referenced_tables_on_the_left() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let sizes = vec![(100.0, 50.0); 4];
        let p = d.layout(&sizes);
        // customers < orders < items
        assert!(p[1].0 < p[3].0 && p[3].0 < p[2].0, "{p:?}");
    }

    #[test]
    fn layout_never_overlaps() {
        let mut s = shop();
        for i in 0..12 {
            s.push(table(&format!("lonely{i}"), vec![col("id", true, None), col("v", false, None)]));
        }
        let d = Diagram::build(&s, DiagramSort::Name);
        let sizes: Vec<(f32, f32)> = (0..d.tables.len()).map(|i| (120.0 + (i % 3) as f32 * 20.0, 60.0 + (i % 4) as f32 * 15.0)).collect();
        let p = d.layout(&sizes);
        assert_eq!(p.len(), sizes.len());
        for i in 0..p.len() {
            for j in (i + 1)..p.len() {
                let apart = p[i].0 + sizes[i].0 <= p[j].0 || p[j].0 + sizes[j].0 <= p[i].0 || p[i].1 + sizes[i].1 <= p[j].1 || p[j].1 + sizes[j].1 <= p[i].1;
                assert!(apart, "tables {i} and {j} overlap: {:?} {:?}", p[i], p[j]);
            }
        }
    }

    #[test]
    fn layout_survives_a_reference_cycle() {
        let s = vec![table("a", vec![col("b_id", false, Some(("b", "id"))), col("id", true, None)]), table("b", vec![col("a_id", false, Some(("a", "id"))), col("id", true, None)])];
        let d = Diagram::build(&s, DiagramSort::Name);
        let p = d.layout(&[(100.0, 50.0), (100.0, 50.0)]);
        assert_eq!(p.len(), 2);
        assert_ne!(p[0], p[1]);
    }

    #[test]
    fn saved_positions_win_and_new_tables_do_not_land_on_them() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let sizes = vec![(100.0, 50.0); 4];
        let mut p = d.layout(&sizes);
        // audit, customers and items were saved; orders is new and its auto position is on top of customers
        let saved: BTreeMap<String, (f32, f32)> = [("audit".to_owned(), (0.0, 0.0)), ("customers".to_owned(), (500.0, 500.0)), ("items".to_owned(), (700.0, 100.0))].into();
        p[3] = (510.0, 510.0);
        d.apply_saved(&mut p, &sizes, &saved);
        assert_eq!(p[0], (0.0, 0.0));
        assert_eq!(p[1], (500.0, 500.0));
        assert_eq!(p[2], (700.0, 100.0));
        assert!(p[3].1 >= 550.0, "the new table moves below the saved ones: {:?}", p[3]);
    }

    #[test]
    fn a_new_table_that_does_not_clash_keeps_its_auto_position() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let sizes = vec![(100.0, 50.0); 4];
        let mut p = d.layout(&sizes);
        let auto = p[3];
        let saved: BTreeMap<String, (f32, f32)> = [("audit".to_owned(), (5000.0, 5000.0))].into();
        d.apply_saved(&mut p, &sizes, &saved);
        assert_eq!(p[3], auto);
    }

    #[test]
    fn apply_saved_with_nothing_saved_changes_nothing() {
        let d = Diagram::build(&shop(), DiagramSort::Name);
        let sizes = vec![(100.0, 50.0); 4];
        let mut p = d.layout(&sizes);
        let before = p.clone();
        d.apply_saved(&mut p, &sizes, &BTreeMap::new());
        assert_eq!(p, before);
    }

    fn typed(name: &str, ty: &str, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), type_name: ty.into(), is_pk: pk, ..Default::default() }
    }

    fn users_and_notifications(user_col: &str, col_type: &str) -> Vec<TableStructure> {
        vec![
            table("users", vec![typed("id", "text(64)", true), typed("email", "text", false)]),
            table("notifications", vec![typed("id", "text(64)", true), typed(user_col, col_type, false)]),
        ]
    }

    fn inferred(d: &Diagram) -> Vec<&DiagramEdge> {
        d.edges.iter().filter(|e| e.kind == EdgeKind::Inferred).collect()
    }

    #[test]
    fn id_user_points_to_the_key_of_users() {
        let d = Diagram::build(&users_and_notifications("id_user", "text(64)"), DiagramSort::Name);
        // sorted: notifications, users
        assert_eq!(inferred(&d), vec![&DiagramEdge { from_table: 0, from_column: 1, to_table: 1, to_column: Some(0), kind: EdgeKind::Inferred }]);
    }

    #[test]
    fn other_name_shapes_are_recognised() {
        for col in ["user_id", "USER_ID", "ID_USER", "userId"] {
            let d = Diagram::build(&users_and_notifications(col, "text(64)"), DiagramSort::Name);
            assert_eq!(inferred(&d).len(), 1, "{col}");
        }
    }

    #[test]
    fn names_that_are_not_references_are_ignored() {
        for col in ["id", "user", "identity", "valid", "uuid", "_id", "id_"] {
            let d = Diagram::build(&users_and_notifications(col, "text(64)"), DiagramSort::Name);
            assert!(inferred(&d).is_empty(), "{col}");
        }
    }

    #[test]
    fn plurals_are_matched() {
        let s = vec![
            table("categories", vec![typed("id", "int", true)]),
            table("boxes", vec![typed("id", "int", true)]),
            table("item", vec![typed("id", "int", true), typed("category_id", "integer", false), typed("box_id", "bigint", false)]),
        ];
        assert_eq!(inferred(&Diagram::build(&s, DiagramSort::Name)).len(), 2);
    }

    #[test]
    fn a_type_of_another_kind_is_not_linked() {
        let d = Diagram::build(&users_and_notifications("id_user", "INTEGER"), DiagramSort::Name);
        assert!(inferred(&d).is_empty());
    }

    #[test]
    fn a_table_with_a_composite_key_is_not_a_target() {
        let s = vec![
            table("users", vec![typed("a", "int", true), typed("b", "int", true)]),
            table("notes", vec![typed("id", "int", true), typed("user_id", "int", false)]),
        ];
        assert!(inferred(&Diagram::build(&s, DiagramSort::Name)).is_empty());
    }

    #[test]
    fn a_real_foreign_key_wins_and_nothing_is_guessed_twice() {
        let s = vec![table("users", vec![col("id", true, None)]), table("notes", vec![col("id", true, None), col("user_id", false, Some(("users", "id")))])];
        let d = Diagram::build(&s, DiagramSort::Name);
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].kind, EdgeKind::Real);
    }

    #[test]
    fn a_table_does_not_point_to_itself() {
        let s = vec![table("user", vec![typed("id", "int", true), typed("user_id", "int", false)])];
        assert!(inferred(&Diagram::build(&s, DiagramSort::Name)).is_empty());
    }

    #[test]
    fn inferred_links_do_not_change_groups_or_layout() {
        let plain = users_and_notifications("owner", "text(64)");
        let guessed = users_and_notifications("id_user", "text(64)");
        let (a, b) = (Diagram::build(&plain, DiagramSort::Name), Diagram::build(&guessed, DiagramSort::Name));
        let sizes = vec![(100.0, 50.0); 2];
        assert_eq!(b.groups(), vec![None, None]);
        assert_eq!(a.layout(&sizes), b.layout(&sizes));
    }

    fn link(a: &str, ac: &str, b: &str, bc: &str) -> DocLink {
        DocLink { from_table: a.into(), from_column: ac.into(), to_table: b.into(), to_column: bc.into() }
    }

    #[test]
    fn manual_links_are_added_by_name() {
        let mut d = Diagram::build(&users_and_notifications("owner", "text(64)"), DiagramSort::Name);
        d.add_manual(&[link("notifications", "owner", "users", "id")]);
        assert_eq!(d.edges, vec![DiagramEdge { from_table: 0, from_column: 1, to_table: 1, to_column: Some(0), kind: EdgeKind::Manual }]);
        assert_eq!(d.groups(), vec![None, None], "a manual link does not make a group");
    }

    #[test]
    fn manual_links_to_missing_tables_or_columns_are_skipped() {
        let mut d = Diagram::build(&users_and_notifications("owner", "text(64)"), DiagramSort::Name);
        d.add_manual(&[link("gone", "x", "users", "id"), link("notifications", "nope", "users", "id"), link("notifications", "owner", "users", "nope")]);
        assert!(d.edges.is_empty());
    }

    #[test]
    fn a_manual_link_replaces_the_guess_and_never_repeats_a_real_key() {
        let mut d = Diagram::build(&users_and_notifications("id_user", "text(64)"), DiagramSort::Name);
        assert_eq!(d.edges.len(), 1);
        d.add_manual(&[link("notifications", "id_user", "users", "id"), link("notifications", "id_user", "users", "id")]);
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].kind, EdgeKind::Manual);
        let s = vec![table("users", vec![col("id", true, None)]), table("notes", vec![col("id", true, None), col("user_id", false, Some(("users", "id")))])];
        let mut d = Diagram::build(&s, DiagramSort::Name);
        d.add_manual(&[link("notes", "user_id", "users", "id")]);
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].kind, EdgeKind::Real);
    }

    #[test]
    fn layout_of_nothing_is_empty() {
        assert!(Diagram::default().layout(&[]).is_empty());
    }
}
