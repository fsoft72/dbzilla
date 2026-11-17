//! The settings screen (SPEC 14, without themes).

use super::widgets::{self, field_label, muted};
use dbzilla_core::safemode::SafeLevel;
use dbzilla_store::settings::{CompleteKey, IndentType, IndentWith, NewTabKind, ResultViewMode, Settings};
use eframe::egui::{self, Ui};

/// Tabs of the settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Table,
    Editor,
    Security,
    Mcp,
}

impl SettingsTab {
    /// Tab title.
    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Table => "Table",
            SettingsTab::Editor => "Editor",
            SettingsTab::Security => "Security",
            SettingsTab::Mcp => "MCP",
        }
    }
}

/// Draws the settings; returns true when something changed.
pub fn show(ui: &mut Ui, s: &mut Settings, tab: &mut SettingsTab, mcp: &mut dyn FnMut(&mut Ui, &mut Settings) -> bool) -> bool {
    let before = s.clone();
    ui.horizontal(|ui| {
        widgets::heading(ui, "Settings");
    });
    ui.horizontal(|ui| {
        for t in [SettingsTab::General, SettingsTab::Table, SettingsTab::Editor, SettingsTab::Security, SettingsTab::Mcp] {
            ui.selectable_value(tab, t, t.label());
        }
    });
    ui.separator();
    let mut mcp_changed = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match *tab {
        SettingsTab::General => general(ui, s),
        SettingsTab::Table => table(ui, s),
        SettingsTab::Editor => editor(ui, s),
        SettingsTab::Security => security(ui, s),
        SettingsTab::Mcp => mcp_changed = mcp(ui, s),
    });
    *s != before || mcp_changed
}

/// Numeric input stored in a `u64`.
fn number(ui: &mut Ui, label: &str, hint: &str, v: &mut u64, range: std::ops::RangeInclusive<u64>) {
    field_label(ui, label);
    ui.add(egui::DragValue::new(v).range(range));
    if !hint.is_empty() {
        muted(ui, hint);
    }
}

fn general(ui: &mut Ui, s: &mut Settings) {
    widgets::card(ui, |ui| {
        ui.checkbox(&mut s.reopen_workspaces, "Reopen closed workspaces at startup");
        ui.add_space(6.0);
        field_label(ui, "Cmd/Ctrl+T opens");
        egui::ComboBox::from_id_salt("newtab").selected_text(if s.new_tab == NewTabKind::SqlEditor { "SQL query editor" } else { "Empty tab" }).show_ui(ui, |ui| {
            ui.selectable_value(&mut s.new_tab, NewTabKind::SqlEditor, "SQL query editor");
            ui.selectable_value(&mut s.new_tab, NewTabKind::Empty, "Empty tab");
        });
    });
    widgets::card(ui, |ui| {
        ui.checkbox(&mut s.keep_alive, "Keep connection alive");
        number(ui, "Keep-alive interval (seconds)", "", &mut s.keep_alive_secs, 5..=3600);
        ui.add_space(6.0);
        number(ui, "Query time-out (seconds)", "Does not apply to SQLite.", &mut s.query_timeout_secs, 5..=86_400);
    });
    widgets::card(ui, |ui| {
        field_label(ui, "CSV delimiter");
        egui::ComboBox::from_id_salt("csvdelim").selected_text(delim_label(s.csv.delimiter)).show_ui(ui, |ui| {
            for d in [',', ';', '\t'] {
                ui.selectable_value(&mut s.csv.delimiter, d, delim_label(d));
            }
        });
        ui.add_space(6.0);
        field_label(ui, "Decimal separator");
        egui::ComboBox::from_id_salt("csvdec").selected_text(s.csv.decimal.to_string()).show_ui(ui, |ui| {
            for d in ['.', ','] {
                ui.selectable_value(&mut s.csv.decimal, d, d.to_string());
            }
        });
        if s.csv.decimal == s.csv.delimiter {
            ui.colored_label(crate::theme::TERTIARY, "The decimal separator and the delimiter are the same character.");
        }
    });
}

/// Text of a CSV delimiter.
fn delim_label(d: char) -> &'static str {
    match d {
        ',' => "Comma ,",
        ';' => "Semicolon ;",
        '\t' => "Tab",
        _ => "Custom",
    }
}

fn table(ui: &mut Ui, s: &mut Settings) {
    widgets::card(ui, |ui| {
        number(ui, "Page size (rows)", "", &mut s.page_size, 10..=100_000);
        ui.add_space(6.0);
        ui.checkbox(&mut s.alternating_rows, "Alternating row background colors");
        ui.checkbox(&mut s.auto_hide_scrollers, "Auto-hide table scrollers");
        ui.checkbox(&mut s.estimate_count, "Estimate the row count of large tables");
        number(ui, "Estimate when the table has more than (rows)", "", &mut s.estimate_threshold, 1_000..=1_000_000_000);
    });
}

fn editor(ui: &mut Ui, s: &mut Settings) {
    widgets::card(ui, |ui| {
        ui.checkbox(&mut s.auto_save_queries, "Automatically save queries while editing");
        ui.checkbox(&mut s.uppercase_keywords, "Automatically UPPERCASE keywords");
        ui.checkbox(&mut s.auto_close_brackets, "Automatically insert closing braces and quotes");
        ui.add_space(6.0);
        field_label(ui, "Complete key");
        egui::ComboBox::from_id_salt("completekey").selected_text(match s.complete_key {
            CompleteKey::Both => "Enter or Tab",
            CompleteKey::EnterOnly => "Enter only",
            CompleteKey::TabOnly => "Tab only",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut s.complete_key, CompleteKey::Both, "Enter or Tab");
            ui.selectable_value(&mut s.complete_key, CompleteKey::EnterOnly, "Enter only");
            ui.selectable_value(&mut s.complete_key, CompleteKey::TabOnly, "Tab only");
        });
    });
    widgets::card(ui, |ui| {
        field_label(ui, "Key binding");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut s.key_binding, dbzilla_store::settings::KeyBinding::Default, "Default");
            ui.selectable_value(&mut s.key_binding, dbzilla_store::settings::KeyBinding::Vim, "Vim");
        });
    });
    widgets::card(ui, |ui| {
        ui.checkbox(&mut s.query_params.enabled, "Enable query parameters");
        ui.add_enabled_ui(s.query_params.enabled, |ui| {
            ui.checkbox(&mut s.query_params.allow_in_quotes, "Allow searching for variables in quoted strings");
            field_label(ui, "Variable format");
            egui::ComboBox::from_id_salt("paramformat").selected_text(s.query_params.format.label()).show_ui(ui, |ui| {
                for f in dbzilla_core::params::ParamFormat::ALL {
                    ui.selectable_value(&mut s.query_params.format, f, f.label());
                }
            });
        });
    });
    widgets::card(ui, |ui| {
        field_label(ui, "Indent with");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut s.indent_with, IndentWith::Spaces, "Spaces");
            ui.selectable_value(&mut s.indent_with, IndentWith::Tab, "Tab");
        });
        field_label(ui, "Indent width");
        ui.horizontal(|ui| {
            for w in [2u8, 4, 6, 8] {
                ui.selectable_value(&mut s.indent_width, w, w.to_string());
            }
        });
        field_label(ui, "Indent type");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut s.indent_type, IndentType::Standard, "Standard");
            ui.selectable_value(&mut s.indent_type, IndentType::TabularLeft, "Tabular left");
            ui.selectable_value(&mut s.indent_type, IndentType::TabularRight, "Tabular right");
        });
        ui.add_space(6.0);
        field_label(ui, "Font size");
        ui.add(egui::DragValue::new(&mut s.editor_font_size).range(9.0..=100.0));
    });
    widgets::card(ui, |ui| {
        field_label(ui, "Result view mode");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut s.result_view_mode, ResultViewMode::Tabs, "Split results into tabs");
            ui.selectable_value(&mut s.result_view_mode, ResultViewMode::AppendInOneTab, "Append all results in one tab");
        });
        ui.add_space(6.0);
        ui.checkbox(&mut s.history_enabled, "Keep query history");
        ui.checkbox(&mut s.log_original_values, "Log original values when updating or deleting data");
    });
}

fn security(ui: &mut Ui, s: &mut Settings) {
    widgets::card(ui, |ui| {
        field_label(ui, "Default safe mode for new connections");
        egui::ComboBox::from_id_salt("defsafe").selected_text(super::connform::safe_label(s.default_safe_level)).show_ui(ui, |ui| {
            for l in [SafeLevel::Silent, SafeLevel::Alert1, SafeLevel::Alert2, SafeLevel::Safe1, SafeLevel::Safe2] {
                ui.selectable_value(&mut s.default_safe_level, l, super::connform::safe_label(l));
            }
        });
        ui.add_space(6.0);
        muted(ui, "Safe mode asks for confirmation or the database password before statements are sent.");
    });
}
