//! Open Anything: fuzzy search over objects and application actions (SPEC 7.7).

use super::tabs::OpenMode;
use super::widgets;
use crate::fuzzy::{match_item, Strategy};
use crate::keymap::{Action, KeyMap};
use crate::theme;
use dbzilla_core::model::TableRef;
use eframe::egui::{self, Align2, Key, RichText};

/// Maximum number of results shown.
const MAX_RESULTS: usize = 12;

/// An entry of the palette.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteItem {
    Object { label: String, table: TableRef },
    Action(Action),
}

impl PaletteItem {
    /// Text matched and shown.
    pub fn label(&self) -> String {
        match self {
            PaletteItem::Object { label, .. } => label.clone(),
            PaletteItem::Action(a) => a.label().to_owned(),
        }
    }
}

/// What the user chose.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteChoice {
    Open(TableRef, OpenMode),
    Run(Action),
}

/// Palette state.
#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
}

/// Items matching the query, best first; an empty query lists actions first.
pub fn rank(items: &[PaletteItem], query: &str) -> Vec<PaletteItem> {
    let mut scored: Vec<(i32, usize, &PaletteItem)> = items.iter().enumerate().filter_map(|(i, it)| match_item(query, &it.label(), Strategy::Fuzzy).map(|s| (s, i, it))).collect();
    if query.trim().is_empty() {
        scored.sort_by_key(|(_, i, it)| (matches!(it, PaletteItem::Object { .. }), *i));
    } else {
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    }
    scored.into_iter().take(MAX_RESULTS).map(|(_, _, it)| it.clone()).collect()
}

/// Draws the palette; returns the choice, and sets `close` when it should disappear.
pub fn show(ctx: &egui::Context, st: &mut PaletteState, items: &[PaletteItem], keymap: &KeyMap, close: &mut bool) -> Option<PaletteChoice> {
    let results = rank(items, &st.query);
    let mut choice = None;
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if st.query.is_empty() {
            *close = true;
        } else {
            st.query.clear();
        }
    }
    if ctx.input(|i| i.key_pressed(Key::ArrowDown) || (i.modifiers.ctrl && i.key_pressed(Key::N))) && !results.is_empty() {
        st.selected = (st.selected + 1) % results.len();
    }
    if ctx.input(|i| i.key_pressed(Key::ArrowUp) || (i.modifiers.ctrl && i.key_pressed(Key::P))) && !results.is_empty() {
        st.selected = (st.selected + results.len() - 1) % results.len();
    }
    st.selected = st.selected.min(results.len().saturating_sub(1));
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));
    let new_tab = ctx.input(|i| i.modifiers.command);
    egui::Window::new("open_anything").title_bar(false).resizable(false).anchor(Align2::CENTER_TOP, [0.0, 80.0]).fixed_size([520.0, 0.0]).show(ctx, |ui| {
        let r = widgets::search_box(ui, &mut st.query, "Open anything: tables, actions...");
        r.request_focus();
        if r.changed() {
            st.selected = 0;
        }
        ui.add_space(4.0);
        for (i, it) in results.iter().enumerate() {
            let selected = i == st.selected;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
            if selected || resp.hovered() {
                ui.painter().rect_filled(rect, theme::RADIUS_CTRL, if selected { theme::INPUT_FILL_BLUE } else { theme::FIELD_ROW_BG });
            }
            ui.painter().text(rect.min + egui::vec2(8.0, rect.height() / 2.0), Align2::LEFT_CENTER, it.label(), egui::FontId::new(13.5, egui::FontFamily::Proportional), theme::TEXT);
            let hint = match it {
                PaletteItem::Action(a) => keymap.hint(*a),
                PaletteItem::Object { .. } => "table".to_owned(),
            };
            ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), Align2::RIGHT_CENTER, hint, egui::FontId::new(12.0, egui::FontFamily::Proportional), theme::MUTED);
            if resp.clicked() || (selected && enter) {
                choice = Some(it.clone());
            }
        }
        if results.is_empty() {
            ui.label(RichText::new("No results").size(12.0).color(theme::MUTED));
        }
    });
    choice.map(|c| match c {
        PaletteItem::Object { table, .. } => PaletteChoice::Open(table, if new_tab { OpenMode::NewTab } else { OpenMode::Pin }),
        PaletteItem::Action(a) => PaletteChoice::Run(a),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PaletteItem> {
        vec![
            PaletteItem::Object { label: "orders".into(), table: TableRef::simple("orders") },
            PaletteItem::Object { label: "customers".into(), table: TableRef::simple("customers") },
            PaletteItem::Action(Action::Settings),
            PaletteItem::Action(Action::Commit),
        ]
    }

    /// An empty query lists the actions first.
    #[test]
    fn empty_query_lists_actions_first() {
        let r = rank(&items(), "");
        assert!(matches!(r[0], PaletteItem::Action(_)));
        assert_eq!(r.len(), 4);
    }

    /// Queries filter and rank across objects and actions.
    #[test]
    fn query_filters() {
        let r = rank(&items(), "ord");
        assert_eq!(r, vec![PaletteItem::Object { label: "orders".into(), table: TableRef::simple("orders") }]);
        assert!(rank(&items(), "sett").contains(&PaletteItem::Action(Action::Settings)));
        assert!(rank(&items(), "^comm").contains(&PaletteItem::Action(Action::Commit)));
    }
}
