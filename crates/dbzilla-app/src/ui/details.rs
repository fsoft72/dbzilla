//! The Details panel: one field per column of the selected row(s) (SPEC 7.4).

use super::pending::parse_edit;
use super::widgets::{self, field_label, muted};
use crate::fuzzy::{match_item, Strategy};
use crate::theme;
use dbzilla_core::changes::CellValue;
use dbzilla_core::model::{ColumnMeta, Row};
use dbzilla_core::value::Value;
use eframe::egui::{self, RichText, Ui};

/// Texts longer than this get a multi line field.
const MULTILINE_CHARS: usize = 80;

/// Panel state.
#[derive(Default)]
pub struct DetailsState {
    pub search: String,
}

/// An edit made in the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum DetailsAction {
    Edit { col: usize, value: CellValue },
    Error(String),
}

/// Text of a value in a field.
fn field_text(v: &Value) -> String {
    if v.is_null() { String::new() } else { v.to_display() }
}

/// Common text of a column over several rows, or `None` when they differ.
pub fn common_text(rows: &[Row], selected: &[usize], col: usize) -> Option<String> {
    let mut texts = selected.iter().filter_map(|r| rows.get(*r)).map(|row| row.get(col).map(field_text).unwrap_or_default());
    let first = texts.next()?;
    texts.all(|t| t == first).then_some(first)
}

/// Draws the panel and returns the edits.
pub fn show(ui: &mut Ui, st: &mut DetailsState, tab_id: u64, columns: &[ColumnMeta], rows: &[Row], selected: &[usize], editable: bool) -> Vec<DetailsAction> {
    let mut actions = Vec::new();
    ui.label(widgets::semibold("Details", 15.0).color(theme::TEXT));
    if selected.is_empty() {
        ui.add_space(8.0);
        muted(ui, "Select a row to see its fields.");
        return actions;
    }
    if selected.len() > 1 {
        muted(ui, &format!("{} rows selected: edits apply to all of them", selected.len()));
    }
    widgets::search_box(ui, &mut st.search, "Search for field");
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (c, col) in columns.iter().enumerate() {
            if match_item(&st.search, &col.name, Strategy::Fuzzy).is_none() {
                continue;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                field_label(ui, &col.name);
                ui.label(RichText::new(&col.type_name).size(11.0).color(theme::MUTED));
            });
            let shared = common_text(rows, selected, c);
            let current = shared.clone().unwrap_or_default();
            let id = ui.id().with(("detail", tab_id, c, selected[0]));
            let focused = ui.memory(|m| m.has_focus(id));
            let mut buf: String = if focused { ui.data(|d| d.get_temp(id)).unwrap_or_else(|| current.clone()) } else { current.clone() };
            let multiline = buf.chars().count() > MULTILINE_CHARS || buf.contains('\n');
            let hint = if shared.is_none() { "(multiple values)" } else { "" };
            let resp = ui.add_enabled(editable, if multiline { egui::TextEdit::multiline(&mut buf).id(id).desired_rows(3).hint_text(hint).desired_width(f32::INFINITY) } else { egui::TextEdit::singleline(&mut buf).id(id).hint_text(hint).desired_width(f32::INFINITY) });
            if resp.has_focus() || resp.changed() {
                ui.data_mut(|d| d.insert_temp(id, buf.clone()));
            }
            if resp.lost_focus() && buf != current {
                match parse_edit(&col.type_name, &buf) {
                    Ok(v) => actions.push(DetailsAction::Edit { col: c, value: v }),
                    Err(e) => actions.push(DetailsAction::Error(e)),
                }
                ui.data_mut(|d| d.remove::<String>(id));
            }
            if editable {
                ui.horizontal(|ui| {
                    for (label, v) in [("NULL", CellValue::Null), ("DEFAULT", CellValue::Default), ("EMPTY", CellValue::Value(Value::Text(String::new())))] {
                        if ui.small_button(label).clicked() {
                            actions.push(DetailsAction::Edit { col: c, value: v });
                        }
                    }
                });
            }
        }
    });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equal values are shared, different ones are not.
    #[test]
    fn common_values() {
        let rows: Vec<Row> = vec![vec![Value::I64(1), Value::Text("x".into())], vec![Value::I64(2), Value::Text("x".into())]];
        assert_eq!(common_text(&rows, &[0, 1], 1).as_deref(), Some("x"));
        assert_eq!(common_text(&rows, &[0, 1], 0), None);
        assert_eq!(common_text(&rows, &[1], 0).as_deref(), Some("2"));
        assert_eq!(common_text(&rows, &[], 0), None);
    }

    /// NULL shows as an empty field.
    #[test]
    fn null_is_empty() {
        let rows: Vec<Row> = vec![vec![Value::Null]];
        assert_eq!(common_text(&rows, &[0], 0).as_deref(), Some(""));
    }
}
