//! The filter bar of the Data tab (SPEC 8.12).

use super::widgets::{self, muted};
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::Dialect;
use dbzilla_core::filter::{to_where, Filter, FilterColumn, FilterOp};
use dbzilla_core::model::ColumnMeta;
use eframe::egui::{self, Align, Layout, RichText, TextEdit, Ui};

/// State of the filter bar of one table tab.
#[derive(Default)]
pub struct FilterBar {
    pub open: bool,
    /// Rows being edited (not necessarily applied).
    pub rows: Vec<Filter>,
    /// Combine applied filters with OR instead of AND.
    pub or: bool,
    /// Text of the column filter (visible columns).
    pub columns_text: String,
    pub show_columns: bool,
}

/// What the bar asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterAction {
    /// Apply these filters (combined with the OR flag).
    Apply { filters: Vec<Filter>, or: bool },
    Clear,
    /// Show only the listed columns (empty shows all).
    VisibleColumns(Vec<String>),
}

/// A new filter row: the primary key column when there is one, otherwise any column.
pub fn default_filter(columns: &[ColumnMeta]) -> Filter {
    let column = columns.iter().find(|c| c.is_pk).map_or(FilterColumn::Any, |c| FilterColumn::Column(c.name.clone()));
    Filter { enabled: true, column, op: FilterOp::Eq, value: String::new() }
}

/// A filter built from a cell: `column op value`.
pub fn quick_filter(column: &str, op: FilterOp, value: &str) -> Filter {
    Filter { enabled: true, column: FilterColumn::Column(column.to_owned()), op, value: value.to_owned() }
}

/// Splits a "col1, col2" text into column names.
pub fn parse_column_list(text: &str) -> Vec<String> {
    text.split(',').map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect()
}

/// Display name of a filter column.
fn column_label(c: &FilterColumn) -> String {
    match c {
        FilterColumn::Column(n) => n.clone(),
        FilterColumn::Any => "Any column".into(),
        FilterColumn::Raw => "Raw SQL".into(),
    }
}

/// Draws the bar; returns what to do.
pub fn show(ui: &mut Ui, bar: &mut FilterBar, columns: &[ColumnMeta], caps: &Capabilities, dialect: &dyn Dialect) -> Option<FilterAction> {
    let mut action = None;
    if bar.rows.is_empty() {
        bar.rows.push(default_filter(columns));
    }
    let ops = FilterOp::available(caps);
    let mut remove = None;
    let mut add_after = None;
    let mut apply_one = None;
    for (i, f) in bar.rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut f.enabled, "").on_hover_text("Include this filter");
            egui::ComboBox::from_id_salt(("filter_col", i)).selected_text(column_label(&f.column)).width(150.0).show_ui(ui, |ui| {
                for c in columns {
                    ui.selectable_value(&mut f.column, FilterColumn::Column(c.name.clone()), &c.name);
                }
                ui.selectable_value(&mut f.column, FilterColumn::Any, "Any column");
                ui.selectable_value(&mut f.column, FilterColumn::Raw, "Raw SQL");
            });
            if f.column != FilterColumn::Raw {
                egui::ComboBox::from_id_salt(("filter_op", i)).selected_text(f.op.label()).width(120.0).show_ui(ui, |ui| {
                    for op in &ops {
                        ui.selectable_value(&mut f.op, *op, op.label());
                    }
                });
            }
            if f.op.needs_value() || f.column == FilterColumn::Raw {
                let hint = if f.column == FilterColumn::Raw { "total > 100 AND status = 'paid'" } else { "value" };
                let r = ui.add(TextEdit::singleline(&mut f.value).hint_text(hint).desired_width(ui.available_width() - 120.0).margin(egui::Margin::symmetric(8, 5)));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply_one = Some(i);
                }
            }
            if widgets::ico_button(ui, Ico::Plus, "Add a filter").clicked() {
                add_after = Some(i);
            }
            if widgets::ico_button(ui, Ico::Minus, "Remove this filter").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        if bar.rows.len() > 1 {
            bar.rows.remove(i);
        } else {
            bar.rows[0] = default_filter(columns);
        }
    }
    if let Some(i) = add_after {
        let copy = bar.rows[i].clone();
        bar.rows.insert(i + 1, Filter { value: String::new(), ..copy });
    }
    if let Some(i) = apply_one {
        let f = bar.rows[i].clone();
        action = Some(FilterAction::Apply { filters: vec![f], or: false });
    }
    ui.horizontal(|ui| {
        if widgets::primary_button(ui, "Apply all").on_hover_text("Apply every checked filter").clicked() {
            let filters: Vec<Filter> = bar.rows.iter().filter(|f| f.enabled).cloned().collect();
            action = Some(FilterAction::Apply { filters, or: bar.or });
        }
        ui.selectable_value(&mut bar.or, false, "AND");
        ui.selectable_value(&mut bar.or, true, "OR");
        if widgets::secondary_button(ui, "Clear").clicked() {
            bar.rows = vec![default_filter(columns)];
            action = Some(FilterAction::Clear);
        }
        let popup = ui.make_persistent_id("filter_sql");
        let sql_btn = widgets::secondary_button(ui, "SQL");
        if sql_btn.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        egui::popup_below_widget(ui, popup, &sql_btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(360.0);
            let checked: Vec<Filter> = bar.rows.iter().filter(|f| f.enabled).cloned().collect();
            let text = to_where(dialect, columns, &checked, bar.or);
            ui.add(egui::Label::new(RichText::new(if text.is_empty() { "(no filter)".to_owned() } else { format!("WHERE {text}") }).monospace().color(theme::TEXT)).selectable(true));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::secondary_button(ui, "Columns...").on_hover_text("Choose the visible columns").clicked() {
                bar.show_columns = !bar.show_columns;
            }
        });
    });
    if bar.show_columns {
        ui.horizontal(|ui| {
            muted(ui, "Visible columns");
            ui.add(TextEdit::singleline(&mut bar.columns_text).hint_text("id, name, status (empty shows all)").desired_width(320.0).margin(egui::Margin::symmetric(8, 5)));
            if widgets::primary_button(ui, "Apply").clicked() {
                action = Some(FilterAction::VisibleColumns(parse_column_list(&bar.columns_text)));
            }
            if widgets::secondary_button(ui, "Show all").clicked() {
                bar.columns_text.clear();
                action = Some(FilterAction::VisibleColumns(Vec::new()));
            }
        });
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), is_pk: pk, ..Default::default() }
    }

    /// New rows default to the primary key, else any column.
    #[test]
    fn default_rows() {
        assert_eq!(default_filter(&[col("a", false), col("id", true)]).column, FilterColumn::Column("id".into()));
        assert_eq!(default_filter(&[col("a", false)]).column, FilterColumn::Any);
        assert_eq!(default_filter(&[]).op, FilterOp::Eq);
    }

    /// Quick filters and column lists.
    #[test]
    fn helpers() {
        let f = quick_filter("status", FilterOp::Contains, "ship");
        assert_eq!((f.enabled, f.value.as_str()), (true, "ship"));
        assert_eq!(parse_column_list(" a, b ,,c "), vec!["a", "b", "c"]);
        assert!(parse_column_list("").is_empty());
    }
}
