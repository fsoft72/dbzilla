//! Structure and Index views of a table (SPEC 8.6, 8.7).

use super::structdraft::{ColDraft, IndexDraft, StructDraft};
use super::widgets::{self, muted};
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::caps::Capabilities;
use eframe::egui::{self, Align, Color32, Layout, RichText, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};

/// Height of an editable row.
const ROW_HEIGHT: f32 = 30.0;

/// Read-only inputs.
pub struct StructEnv<'a> {
    pub caps: &'a Capabilities,
    /// Data types of the dialect.
    pub types: &'a [&'a str],
    /// False for views and materialized views.
    pub editable: bool,
}

/// What the view asks the workspace to do.
#[derive(Debug, Clone, PartialEq)]
pub enum StructAction {
    EditFk(usize),
    ShowTriggers,
}

/// Tint of a changed row.
fn tint(c: &ColDraft) -> Option<Color32> {
    if c.deleted {
        Some(theme::ERROR.gamma_multiply(0.16))
    } else if c.orig.is_none() {
        Some(theme::OK.gamma_multiply(0.14))
    } else if c.is_modified() {
        Some(theme::TERTIARY.gamma_multiply(0.12))
    } else {
        None
    }
}

/// Text input filling its cell.
fn cell_edit(ui: &mut Ui, text: &mut String, enabled: bool) {
    ui.add_enabled(enabled, TextEdit::singleline(text).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(6, 4)));
}

/// Data type input with a list of the dialect types.
fn type_cell(ui: &mut Ui, id: egui::Id, ty: &mut String, types: &[&str], enabled: bool) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            let width = (ui.available_width() - widgets::ICON_BUTTON).max(60.0);
            ui.add(TextEdit::singleline(ty).desired_width(width).margin(egui::Margin::symmetric(6, 4)));
            let popup = id.with("types");
            let button = widgets::ico_button(ui, Ico::ChevronDown, "Choose a data type");
            if button.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup));
            }
            egui::popup_below_widget(ui, popup, &button, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(160.0);
                widgets::flat_menu_style(ui);
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for t in types {
                        if ui.button(*t).clicked() {
                            *ty = (*t).to_owned();
                        }
                    }
                });
            });
        });
    });
}

/// The column editor. Returns requests for the workspace.
pub fn show_columns(ui: &mut Ui, d: &mut StructDraft, env: &StructEnv) -> Vec<StructAction> {
    let mut actions = Vec::new();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            widgets::field_label(ui, "Name");
            ui.add_enabled(env.editable, TextEdit::singleline(&mut d.new_name).desired_width(260.0).margin(egui::Margin::symmetric(8, 6)));
        });
        ui.vertical(|ui| {
            widgets::field_label(ui, "Primary key");
            let pk = d.pk_columns();
            ui.label(RichText::new(if pk.is_empty() { "none".to_owned() } else { pk.join(", ") }).monospace().color(theme::TEXT));
        });
    });
    ui.add_space(6.0);
    let mut delete = None;
    let types = env.types;
    let max_h = ui.available_height() - 48.0;
    TableBuilder::new(ui)
        .id_salt("struct_cols")
        .striped(false)
        .resizable(false)
        .vscroll(true)
        .max_scroll_height(max_h)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(36.0))
        .column(Column::exact(30.0))
        .column(Column::initial(180.0).at_least(80.0).clip(true))
        .column(Column::initial(200.0).at_least(100.0).clip(true))
        .column(Column::exact(56.0))
        .column(Column::initial(160.0).at_least(80.0).clip(true))
        .column(Column::exact(48.0))
        .column(Column::remainder().at_least(160.0))
        .header(30.0, |mut h| {
            for t in ["#", "", "Name", "Type", "Null", "Default", "PK", "Foreign key"] {
                h.col(|ui| {
                    widgets::field_label(ui, t);
                });
            }
        })
        .body(|body| {
            let n = d.cols.len();
            body.rows(ROW_HEIGHT, n, |mut row| {
                let i = row.index();
                let c = &mut d.cols[i];
                if let Some(t) = tint(c) {
                    row.set_selected(false);
                    let _ = t;
                }
                let enabled = env.editable && !c.deleted;
                let new_table = d.is_new;
                row.col(|ui| {
                    if let Some(t) = tint(c) {
                        ui.painter().rect_filled(ui.max_rect().expand2(egui::vec2(0.0, 1.0)), 0.0, t);
                    }
                    muted(ui, &(i + 1).to_string());
                });
                row.col(|ui| {
                    if enabled && widgets::ico_button(ui, Ico::Trash, "Delete the column").clicked() {
                        delete = Some(i);
                    } else if c.deleted && widgets::ico_button(ui, Ico::Discard, "Keep the column").clicked() {
                        c.deleted = false;
                    }
                });
                row.col(|ui| {
                    if let Some(t) = tint(c) {
                        ui.painter().rect_filled(ui.max_rect().expand2(egui::vec2(0.0, 1.0)), 0.0, t);
                    }
                    cell_edit(ui, &mut c.name, enabled);
                });
                row.col(|ui| type_cell(ui, ui.id().with(i), &mut c.type_name, types, enabled));
                row.col(|ui| {
                    ui.add_enabled(enabled, egui::Checkbox::without_text(&mut c.nullable));
                });
                row.col(|ui| cell_edit(ui, &mut c.default, enabled));
                row.col(|ui| {
                    ui.add_enabled(enabled, egui::Checkbox::without_text(&mut c.pk));
                });
                row.col(|ui| {
                    let can_fk = env.caps.add_fk_after_create || new_table;
                    match &c.fk {
                        Some(f) => {
                            ui.label(RichText::new(f.summary()).monospace().size(12.0).color(theme::SECONDARY));
                            if enabled && (can_fk || c.orig.is_some()) && ui.small_button("x").on_hover_text("Remove the foreign key").clicked() {
                                c.fk = None;
                            }
                        }
                        None => {
                            ui.add_enabled_ui(enabled && can_fk, |ui| {
                                if ui.small_button("+ FK").on_hover_text(if can_fk { "Create a foreign key" } else { "This database cannot add a foreign key to an existing table" }).clicked() {
                                    actions.push(StructAction::EditFk(i));
                                }
                            });
                        }
                    }
                });
            });
        });
    if let Some(i) = delete {
        if d.cols[i].orig.is_some() {
            d.cols[i].deleted = true;
        } else {
            d.cols.remove(i);
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(env.editable, |ui| {
            if widgets::secondary_button(ui, "+ Column").clicked() {
                d.cols.push(ColDraft::blank(env.types.first().copied().unwrap_or("text")));
            }
        });
        let has_triggers = !d.original.triggers.is_empty();
        if (has_triggers || (env.caps.triggers && env.editable)) && ui.small_button(format!("Triggers ({})", d.original.triggers.len())).clicked() {
            actions.push(StructAction::ShowTriggers);
        }
        if !env.editable {
            muted(ui, "Views and materialized views are read only here: edit them in the DDL tab.");
        }
    });
    actions
}

/// The index editor.
pub fn show_indexes(ui: &mut Ui, d: &mut StructDraft, env: &StructEnv) {
    ui.add_space(6.0);
    let mut delete = None;
    let max_h = ui.available_height() - 48.0;
    TableBuilder::new(ui)
        .id_salt("struct_idx")
        .resizable(false)
        .vscroll(true)
        .max_scroll_height(max_h)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::initial(220.0).at_least(100.0).clip(true))
        .column(Column::exact(70.0))
        .column(Column::initial(300.0).at_least(120.0).clip(true))
        .column(Column::remainder().at_least(40.0))
        .header(30.0, |mut h| {
            for t in ["Name", "Unique", "Columns (comma separated)", ""] {
                h.col(|ui| {
                    widgets::field_label(ui, t);
                });
            }
        })
        .body(|body| {
            let n = d.indexes.len();
            body.rows(ROW_HEIGHT, n, |mut row| {
                let i = row.index();
                let ix = &mut d.indexes[i];
                let enabled = env.editable && !ix.deleted;
                let fill = if ix.deleted {
                    Some(theme::ERROR.gamma_multiply(0.16))
                } else if ix.orig.is_none() {
                    Some(theme::OK.gamma_multiply(0.14))
                } else if ix.is_modified() {
                    Some(theme::TERTIARY.gamma_multiply(0.12))
                } else {
                    None
                };
                row.col(|ui| {
                    if let Some(f) = fill {
                        ui.painter().rect_filled(ui.max_rect().expand2(egui::vec2(0.0, 1.0)), 0.0, f);
                    }
                    cell_edit(ui, &mut ix.name, enabled);
                });
                row.col(|ui| {
                    ui.add_enabled(enabled, egui::Checkbox::without_text(&mut ix.unique));
                });
                row.col(|ui| cell_edit(ui, &mut ix.columns, enabled));
                row.col(|ui| {
                    if enabled && widgets::ico_button(ui, Ico::Trash, "Delete the index").clicked() {
                        delete = Some(i);
                    } else if ix.deleted && widgets::ico_button(ui, Ico::Discard, "Keep the index").clicked() {
                        ix.deleted = false;
                    }
                });
            });
        });
    if let Some(i) = delete {
        if d.indexes[i].orig.is_some() {
            d.indexes[i].deleted = true;
        } else {
            d.indexes.remove(i);
        }
    }
    ui.add_space(4.0);
    ui.add_enabled_ui(env.editable, |ui| {
        if widgets::secondary_button(ui, "+ Index").clicked() {
            d.indexes.push(IndexDraft::blank());
        }
    });
}
