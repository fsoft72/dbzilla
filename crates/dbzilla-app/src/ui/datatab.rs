//! The Data tab of a table: paging, state and drawing (SPEC 8.2).

use super::grid::{self, GridEvent, GridInput, GridState};
use super::quicklook::{self, QuickLook};
use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::copyfmt::{format_rows, CopyContext, CopyFormat, CsvOptions};
use dbzilla_core::dialect::Dialect;
use dbzilla_core::filter::{Filter, FilterColumn, FilterOp};
use dbzilla_core::model::{ColumnMeta, Row, TableRef, TableStructure};
use dbzilla_core::paging::SortKey;
use dbzilla_core::value::Value;
use dbzilla_session::RequestId;
use eframe::egui::{self, RichText, Ui};
use std::time::Duration;

/// Known size of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Total {
    #[default]
    Unknown,
    Estimated(u64),
    Exact(u64),
    Counting,
}

/// Rows and view state of one table tab.
pub struct DataState {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Row>,
    pub grid: GridState,
    pub sort: Vec<SortKey>,
    pub filters: Vec<Filter>,
    pub filters_or: bool,
    pub limit: u64,
    pub offset: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub elapsed: Option<Duration>,
    pub fetch_req: Option<RequestId>,
    pub count_req: Option<RequestId>,
    pub total: Total,
    pub quicklook: Option<QuickLook>,
    pub limit_text: String,
    pub offset_text: String,
}

impl DataState {
    /// Fresh state with a page size.
    pub fn new(limit: u64) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            grid: GridState::default(),
            sort: Vec::new(),
            filters: Vec::new(),
            filters_or: false,
            limit,
            offset: 0,
            loading: false,
            error: None,
            elapsed: None,
            fetch_req: None,
            count_req: None,
            total: Total::Unknown,
            quicklook: None,
            limit_text: limit.to_string(),
            offset_text: "0".into(),
        }
    }
}

/// Requests of the data tab to the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum DataAction {
    Refetch,
    CountExact,
    FollowFk { table: TableRef, column: String, value: Value },
    Toast(String),
}

/// Context needed to draw a data tab.
pub struct DataEnv<'a> {
    pub dialect: &'a dyn Dialect,
    pub csv: &'a CsvOptions,
    pub alternating: bool,
    pub row_height: f32,
    pub table: &'a TableRef,
    pub structure: Option<&'a TableStructure>,
}

/// First and last row numbers (1-based) of the loaded page; `None` when empty.
pub fn page_bounds(offset: u64, loaded: usize) -> Option<(u64, u64)> {
    (loaded > 0).then(|| (offset + 1, offset + loaded as u64))
}

/// Offset of the previous page.
pub fn prev_offset(offset: u64, limit: u64) -> u64 {
    offset.saturating_sub(limit)
}

/// True when another page may exist.
pub fn can_next(offset: u64, loaded: usize, limit: u64, total: Total) -> bool {
    match total {
        Total::Exact(n) => offset + (loaded as u64) < n,
        _ => loaded as u64 >= limit,
    }
}

/// Result columns enriched with key and type information from the structure.
pub fn merge_columns(result: &[ColumnMeta], structure: Option<&TableStructure>) -> Vec<ColumnMeta> {
    result
        .iter()
        .map(|c| match structure.and_then(|s| s.columns.iter().find(|x| x.name == c.name)) {
            Some(s) => ColumnMeta { ordinal: c.ordinal, ..s.clone() },
            None => c.clone(),
        })
        .collect()
}

/// Text of the selection in a copy format.
pub fn selection_text(columns: &[ColumnMeta], rows: &[Row], sel: &grid::Selection, fmt: CopyFormat, cells_only: bool, env: &DataEnv) -> Option<String> {
    let (cols, picked): (Vec<ColumnMeta>, Vec<Row>) = match (cells_only, sel.range()) {
        (true, Some((r0, c0, r1, c1))) => {
            let cs: Vec<ColumnMeta> = columns.get(c0..=c1.min(columns.len().saturating_sub(1)))?.to_vec();
            let rs: Vec<Row> = (r0..=r1).filter_map(|r| rows.get(r).map(|row| row.get(c0..=c1.min(row.len().saturating_sub(1))).map(<[Value]>::to_vec).unwrap_or_default())).collect();
            (cs, rs)
        }
        _ => {
            let sel_rows = sel.selected_rows();
            if sel_rows.is_empty() {
                return None;
            }
            (columns.to_vec(), sel_rows.into_iter().filter_map(|r| rows.get(r).cloned()).collect())
        }
    };
    Some(format_rows(fmt, &cols, &picked, &CopyContext { dialect: env.dialect, table: Some(env.table), csv: env.csv }))
}

/// Filter created by following a foreign key.
pub fn fk_filter(column: &str, value: &Value) -> Filter {
    Filter { enabled: true, column: FilterColumn::Column(column.to_owned()), op: FilterOp::Eq, value: value.to_display() }
}

/// Draws the data tab and returns requests for the workspace.
pub fn show(ui: &mut Ui, d: &mut DataState, env: &DataEnv) -> Vec<DataAction> {
    let mut actions = Vec::new();
    let columns = merge_columns(&d.columns, env.structure);
    egui::TopBottomPanel::bottom(ui.id().with("data_bar")).exact_height(34.0).frame(egui::Frame::new().fill(theme::NAVY_DARK).inner_margin(egui::Margin::symmetric(10, 4))).show_inside(ui, |ui| bottom_bar(ui, d, &mut actions));
    egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::DEEP_DARK)).show_inside(ui, |ui| {
        if let Some(e) = &d.error {
            ui.add_space(12.0);
            ui.label(RichText::new(e).color(theme::ERROR));
            return;
        }
        if d.columns.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                if d.loading {
                    ui.spinner();
                } else {
                    widgets::muted(ui, "No data");
                }
            });
            return;
        }
        let input = GridInput { columns: &columns, rows: &d.rows, sort: &d.sort, alternating: env.alternating, row_height: env.row_height, id: ui.id().with("grid") };
        let events = grid::show(ui, &mut d.grid, &input);
        for e in events {
            apply_event(ui, d, &columns, env, e, &mut actions);
        }
    });
    if let Some(ql) = d.quicklook.as_mut() {
        if !quicklook::show(ui.ctx(), ql) {
            d.quicklook = None;
        }
    }
    actions
}

/// Applies one grid event.
fn apply_event(ui: &Ui, d: &mut DataState, columns: &[ColumnMeta], env: &DataEnv, e: GridEvent, actions: &mut Vec<DataAction>) {
    match e {
        GridEvent::SortCycle { column, multi } => {
            grid::cycle_sort(&mut d.sort, &column, multi);
            d.offset = 0;
            actions.push(DataAction::Refetch);
        }
        GridEvent::SortSet { column, desc } => {
            d.sort.retain(|k| k.column != column);
            if let Some(desc) = desc {
                d.sort.insert(0, SortKey { column, desc });
            }
            d.offset = 0;
            actions.push(DataAction::Refetch);
        }
        GridEvent::HideColumn(c) => {
            d.grid.hidden.insert(c);
        }
        GridEvent::ShowAllColumns => d.grid.hidden.clear(),
        GridEvent::QuickLook { row, col } => {
            if let (Some(c), Some(v)) = (columns.get(col), d.rows.get(row).and_then(|r| r.get(col))) {
                d.quicklook = Some(QuickLook::new(&c.name, v.clone()));
            }
        }
        GridEvent::FollowFk { row, col } => {
            if let (Some(c), Some(v)) = (columns.get(col), d.rows.get(row).and_then(|r| r.get(col))) {
                if let Some(fk) = &c.fk {
                    let target = fk.table.clone();
                    let column = fk.columns.first().cloned().unwrap_or_else(|| c.name.clone());
                    actions.push(DataAction::FollowFk { table: target, column, value: v.clone() });
                }
            }
        }
        GridEvent::Copy { format, cells_only } => {
            if let Some(text) = selection_text(columns, &d.rows, &d.grid.selection, format, cells_only, env) {
                ui.ctx().copy_text(text);
                actions.push(DataAction::Toast("Copied".into()));
            }
        }
        GridEvent::CopyColumn(c) => {
            let text = d.rows.iter().filter_map(|r| r.get(c)).map(Value::to_display).collect::<Vec<_>>().join("\n");
            ui.ctx().copy_text(text);
            actions.push(DataAction::Toast("Column values copied".into()));
        }
    }
}

/// Paging controls, counts and timing.
fn bottom_bar(ui: &mut Ui, d: &mut DataState, actions: &mut Vec<DataAction>) {
    ui.horizontal_centered(|ui| {
        let can_prev = d.offset > 0 && !d.loading;
        ui.add_enabled_ui(can_prev, |ui| {
            if widgets::ico_button(ui, Ico::ChevronLeft, "Previous page").clicked() {
                d.offset = prev_offset(d.offset, d.limit);
                actions.push(DataAction::Refetch);
            }
        });
        let popup_id = ui.make_persistent_id("page_settings");
        let settings = widgets::ico_button(ui, Ico::Settings, "Page settings");
        if settings.clicked() {
            d.limit_text = d.limit.to_string();
            d.offset_text = d.offset.to_string();
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup_above_or_below_widget(ui, popup_id, &settings, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(200.0);
            widgets::field_label(ui, "Limit");
            ui.text_edit_singleline(&mut d.limit_text);
            widgets::field_label(ui, "Offset");
            ui.text_edit_singleline(&mut d.offset_text);
            if widgets::primary_button(ui, "Go").clicked() {
                if let (Ok(l), Ok(o)) = (d.limit_text.trim().parse::<u64>(), d.offset_text.trim().parse::<u64>()) {
                    d.limit = l.max(1);
                    d.offset = o;
                    actions.push(DataAction::Refetch);
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        });
        ui.add_enabled_ui(can_next(d.offset, d.rows.len(), d.limit, d.total) && !d.loading, |ui| {
            if widgets::ico_button(ui, Ico::ChevronRight, "Next page").clicked() {
                d.offset += d.limit;
                actions.push(DataAction::Refetch);
            }
        });
        ui.add_space(8.0);
        let range = page_bounds(d.offset, d.rows.len()).map(|(a, b)| format!("{a}-{b}")).unwrap_or_else(|| "0".into());
        let total = match d.total {
            Total::Exact(n) => format!(" of {n}"),
            Total::Estimated(n) => format!(" of ~{n}"),
            Total::Counting => " of ...".into(),
            Total::Unknown => String::new(),
        };
        ui.label(RichText::new(format!("Rows {range}{total}")).size(12.0).color(theme::TEXT));
        if matches!(d.total, Total::Estimated(_) | Total::Unknown) && ui.small_button("Count").on_hover_text("Count the exact number of rows").clicked() {
            actions.push(DataAction::CountExact);
        }
        if let Some(t) = d.elapsed {
            widgets::muted(ui, &format!("{} ms", t.as_millis()));
        }
        if d.loading {
            ui.spinner();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::dialect::{dialect_for, DriverId};
    use dbzilla_core::model::ForeignKeyRef;

    fn col(name: &str) -> ColumnMeta {
        ColumnMeta { name: name.into(), type_name: "text".into(), ..Default::default() }
    }

    fn env<'a>(d: &'a dyn Dialect, csv: &'a CsvOptions, t: &'a TableRef) -> DataEnv<'a> {
        DataEnv { dialect: d, csv, alternating: true, row_height: 24.0, table: t, structure: None }
    }

    /// Page bounds, previous offset and next availability.
    #[test]
    fn paging_math() {
        assert_eq!(page_bounds(0, 300), Some((1, 300)));
        assert_eq!(page_bounds(600, 17), Some((601, 617)));
        assert_eq!(page_bounds(0, 0), None);
        assert_eq!(prev_offset(600, 300), 300);
        assert_eq!(prev_offset(100, 300), 0);
        assert!(can_next(0, 300, 300, Total::Unknown));
        assert!(!can_next(0, 120, 300, Total::Unknown));
        assert!(can_next(0, 300, 300, Total::Exact(301)));
        assert!(!can_next(0, 300, 300, Total::Exact(300)));
    }

    /// Keys and types from the structure replace the result metadata.
    #[test]
    fn merging_structure_info() {
        let st = TableStructure {
            columns: vec![ColumnMeta { name: "id".into(), type_name: "int4".into(), is_pk: true, ordinal: 0, ..Default::default() }, ColumnMeta { name: "cid".into(), fk: Some(ForeignKeyRef { table: TableRef::simple("c"), columns: vec!["id".into()], on_update: "NO ACTION".into(), on_delete: "NO ACTION".into() }), ..col("cid") }],
            ..Default::default()
        };
        let merged = merge_columns(&[col("id"), col("cid"), col("extra")], Some(&st));
        assert!(merged[0].is_pk && merged[0].type_name == "int4");
        assert!(merged[1].fk.is_some());
        assert_eq!(merged[2].name, "extra");
    }

    /// Copy text for rows and for a cell range, in two formats.
    #[test]
    fn copy_text() {
        let cols = vec![col("id"), col("name"), col("note")];
        let rows: Vec<Row> = vec![vec![Value::I64(1), Value::Text("a".into()), Value::Null], vec![Value::I64(2), Value::Text("b".into()), Value::Text("x".into())]];
        let d = dialect_for(DriverId::Sqlite);
        let (csv, t) = (CsvOptions::default(), TableRef::simple("t"));
        let e = env(d.as_ref(), &csv, &t);
        let mut sel = grid::Selection::default();
        sel.select_row(1, false, false, None);
        assert_eq!(selection_text(&cols, &rows, &sel, CopyFormat::PlainText, false, &e).as_deref(), Some("2\tb\tx"));
        sel.select_cell(0, 1);
        sel.extend_to(1, 2);
        assert_eq!(selection_text(&cols, &rows, &sel, CopyFormat::Csv, true, &e).as_deref(), Some("a,NULL\nb,x"));
        sel.clear();
        assert_eq!(selection_text(&cols, &rows, &sel, CopyFormat::PlainText, false, &e), None);
    }

    /// Following a foreign key filters the target by equality.
    #[test]
    fn fk_filter_is_equality() {
        let f = fk_filter("id", &Value::I64(7));
        assert_eq!((f.op, f.value.as_str()), (FilterOp::Eq, "7"));
    }
}
