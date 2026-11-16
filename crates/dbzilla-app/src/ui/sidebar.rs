//! The left sidebar: Items / Queries / History (SPEC 7.3).

use super::tabs::OpenMode;
use super::widgets;
use crate::fuzzy::{match_item, Strategy};
use crate::glyphs::{self, Ico};
use crate::theme;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef};
use eframe::egui::{self, CursorIcon, RichText, Sense, Ui};

/// Tabs of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Items,
    Queries,
    History,
}

/// Sidebar state.
pub struct SidebarState {
    pub tab: SidebarTab,
    pub search: String,
    pub strategy: Strategy,
    pub focus_search: bool,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self { tab: SidebarTab::Items, search: String::new(), strategy: Strategy::Fuzzy, focus_search: false }
    }
}

/// What the user did in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarAction {
    Open(TableRef, OpenMode),
    CopyName(String),
    SwitchSchema(String),
    Refresh,
}

/// Section title of an object kind.
fn section_name(k: ObjectKind) -> &'static str {
    match k {
        ObjectKind::Table => "Tables",
        ObjectKind::View => "Views",
        ObjectKind::MaterializedView => "Materialized views",
        ObjectKind::Function => "Functions",
        ObjectKind::Procedure => "Procedures",
        ObjectKind::Sequence => "Sequences",
        ObjectKind::Type => "Types",
    }
}

/// Icon of an object kind.
pub fn kind_icon(k: ObjectKind) -> Ico {
    match k {
        ObjectKind::Table => Ico::Table,
        ObjectKind::View | ObjectKind::MaterializedView => Ico::View,
        ObjectKind::Function | ObjectKind::Procedure => Ico::Function,
        ObjectKind::Sequence | ObjectKind::Type => Ico::Bolt,
    }
}

/// Order of the sections.
const SECTION_ORDER: [ObjectKind; 7] = [ObjectKind::Table, ObjectKind::View, ObjectKind::MaterializedView, ObjectKind::Function, ObjectKind::Procedure, ObjectKind::Sequence, ObjectKind::Type];

/// Objects of `kind` matching the query, best matches first (names alphabetically when no query).
pub fn filtered<'a>(objects: &'a [ObjectInfo], kind: ObjectKind, query: &str, strategy: Strategy) -> Vec<&'a ObjectInfo> {
    let mut scored: Vec<(i32, &ObjectInfo)> = objects.iter().filter(|o| o.kind == kind).filter_map(|o| match_item(query, &o.table.name, strategy).map(|s| (s, o))).collect();
    if query.trim().is_empty() {
        scored.sort_by_key(|(_, o)| o.table.name.to_lowercase());
    } else {
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.table.name.to_lowercase().cmp(&b.1.table.name.to_lowercase())));
    }
    scored.into_iter().map(|(_, o)| o).collect()
}

/// Read-only data the sidebar shows.
pub struct SidebarView<'a> {
    pub objects: &'a [ObjectInfo],
    pub schemas: &'a [String],
    pub current_schema: Option<&'a str>,
    pub caps: &'a Capabilities,
    /// Table of the focused tab.
    pub active: Option<&'a TableRef>,
}

/// Draws the sidebar; `queries` draws the Queries and History tabs.
pub fn show(ui: &mut Ui, st: &mut SidebarState, view: &SidebarView, queries: &mut dyn FnMut(&mut Ui, SidebarTab)) -> Option<SidebarAction> {
    let SidebarView { objects, schemas, current_schema, caps, active } = *view;
    let mut action = None;
    ui.horizontal(|ui| {
        for (t, label) in [(SidebarTab::Items, "Items"), (SidebarTab::Queries, "Queries"), (SidebarTab::History, "History")] {
            ui.selectable_value(&mut st.tab, t, label);
        }
    });
    let resp = widgets::search_box(ui, &mut st.search, "Search");
    if st.focus_search {
        resp.request_focus();
        st.focus_search = false;
    }
    if st.tab != SidebarTab::Items {
        queries(ui, st.tab);
        return None;
    }
    let bottom_height = 36.0;
    egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - bottom_height).show(ui, |ui| {
        let mut any = false;
        for kind in SECTION_ORDER {
            let items = filtered(objects, kind, &st.search, st.strategy);
            if items.is_empty() {
                continue;
            }
            any = true;
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{}  {}", section_name(kind).to_uppercase(), items.len())).font(egui::FontId::new(11.0, theme::semibold())).color(theme::LABEL));
            for o in items {
                if let Some(a) = item_row(ui, o, active == Some(&o.table)) {
                    action = Some(a);
                }
            }
        }
        if !any {
            ui.add_space(16.0);
            widgets::muted(ui, if objects.is_empty() { "No objects in this database." } else { "Nothing matches the search." });
        }
    });
    ui.horizontal(|ui| {
        if caps.schemas {
            let label = current_schema.unwrap_or("schema").to_owned();
            egui::ComboBox::from_id_salt("schema_menu").selected_text(label).width(160.0).show_ui(ui, |ui| {
                for s in schemas {
                    if ui.selectable_label(current_schema == Some(s.as_str()), s).clicked() {
                        action = Some(SidebarAction::SwitchSchema(s.clone()));
                    }
                }
            });
        }
        if widgets::ico_button(ui, Ico::Refresh, "Reload items").clicked() {
            action = Some(SidebarAction::Refresh);
        }
    });
    action
}

/// One object row.
fn item_row(ui: &mut Ui, o: &ObjectInfo, active: bool) -> Option<SidebarAction> {
    let mut action = None;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::click());
    if active {
        ui.painter().rect_filled(rect, theme::RADIUS_CTRL, theme::INPUT_FILL_BLUE);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, theme::RADIUS_CTRL, theme::FIELD_ROW_BG);
    }
    let icon_rect = egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 0.0), egui::vec2(24.0, rect.height()));
    glyphs::paint(ui.painter(), icon_rect, kind_icon(o.kind), if active { theme::SECONDARY } else { theme::LABEL });
    let font = egui::FontId::new(13.5, egui::FontFamily::Proportional);
    ui.painter().text(rect.min + egui::vec2(28.0, rect.height() / 2.0), egui::Align2::LEFT_CENTER, &o.table.name, font, theme::TEXT);
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.double_clicked() {
        action = Some(SidebarAction::Open(o.table.clone(), OpenMode::Pin));
    } else if resp.clicked() {
        action = Some(SidebarAction::Open(o.table.clone(), OpenMode::Preview));
    }
    resp.context_menu(|ui| {
        widgets::flat_menu_style(ui);
        if ui.button("Open in new tab").clicked() {
            action = Some(SidebarAction::Open(o.table.clone(), OpenMode::NewTab));
            ui.close_menu();
        }
        if ui.button("Copy name").clicked() {
            action = Some(SidebarAction::CopyName(o.table.name.clone()));
            ui.close_menu();
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: ObjectKind, name: &str) -> ObjectInfo {
        ObjectInfo { kind, table: TableRef::simple(name) }
    }

    /// Without a query, names are sorted case-insensitively; kinds are separated.
    #[test]
    fn sorted_without_query() {
        let objs = [obj(ObjectKind::Table, "zeta"), obj(ObjectKind::Table, "Alpha"), obj(ObjectKind::View, "v1")];
        let names: Vec<&str> = filtered(&objs, ObjectKind::Table, "", Strategy::Fuzzy).iter().map(|o| o.table.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "zeta"]);
    }

    /// With a query only matches remain, best first.
    #[test]
    fn query_filters_and_ranks() {
        let objs = [obj(ObjectKind::Table, "bonus_info"), obj(ObjectKind::Table, "order_items"), obj(ObjectKind::Table, "customers")];
        let names: Vec<&str> = filtered(&objs, ObjectKind::Table, "oi", Strategy::Fuzzy).iter().map(|o| o.table.name.as_str()).collect();
        assert_eq!(names.first(), Some(&"order_items"));
        assert!(!names.contains(&"customers"));
    }
}
