//! The connection list pane: row model, filtering, sorting and drawing (SPEC 6.1, 6.4).

use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_store::connections::{ConnectionStore, StoredConnection};
use dbzilla_store::tags::TagStore;
use eframe::egui::{self, CursorIcon, Frame, Margin, RichText, Sense, Stroke, Ui};
use std::collections::HashSet;

/// Sort order of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnSort {
    Name,
    Tag,
    Driver,
}

impl ConnSort {
    /// Text shown in the combo box.
    pub fn label(self) -> &'static str {
        match self {
            ConnSort::Name => "Sort by name",
            ConnSort::Tag => "Sort by tag",
            ConnSort::Driver => "Sort by driver",
        }
    }
}

/// One visible row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowModel {
    Group { id: String, name: String, count: usize, collapsed: bool, depth: usize },
    Conn { id: String, depth: usize },
}

/// State of the list pane.
pub struct ListState {
    pub query: String,
    pub sort: ConnSort,
    pub collapsed: HashSet<String>,
    pub selected: Option<String>,
}

impl Default for ListState {
    fn default() -> Self {
        Self { query: String::new(), sort: ConnSort::Name, collapsed: HashSet::new(), selected: None }
    }
}

/// What the user did in the list.
#[derive(Debug, Clone, PartialEq)]
pub enum ListAction {
    NewConnection(DriverId),
    NewGroup,
    ImportUrl,
    Select(String),
    Connect(String),
    Edit(String),
    Duplicate(String),
    CopyUrl(String),
    Delete(String),
    ToggleGroup(String),
    DeleteGroup(String),
}

/// True when the connection matches the search text.
fn matches(c: &StoredConnection, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    [&c.name, &c.host, &c.database].iter().any(|s| s.to_lowercase().contains(&q)) || c.tag.as_ref().is_some_and(|t| t.to_lowercase().contains(&q))
}

/// Sorts connections in place.
fn sort_conns(v: &mut [&StoredConnection], sort: ConnSort, tags: &TagStore) {
    let tag_order = |c: &StoredConnection| c.tag.as_ref().and_then(|t| tags.all().iter().position(|x| &x.name == t)).unwrap_or(usize::MAX);
    match sort {
        ConnSort::Name => v.sort_by_key(|c| c.name.to_lowercase()),
        ConnSort::Tag => v.sort_by_key(|c| (tag_order(c), c.name.to_lowercase())),
        ConnSort::Driver => v.sort_by_key(|c| (format!("{:?}", c.driver), c.name.to_lowercase())),
    }
}

/// Builds the visible rows: groups first (nested), then connections outside any group.
pub fn build_rows(store: &ConnectionStore, tags: &TagStore, query: &str, sort: ConnSort, collapsed: &HashSet<String>) -> Vec<RowModel> {
    let mut rows = Vec::new();
    add_group_level(store, tags, query, sort, collapsed, None, 0, &mut rows);
    let mut loose: Vec<&StoredConnection> = store.list().iter().filter(|c| c.group.as_ref().is_none_or(|g| !store.groups().iter().any(|x| &x.id == g)) && matches(c, query)).collect();
    sort_conns(&mut loose, sort, tags);
    rows.extend(loose.into_iter().map(|c| RowModel::Conn { id: c.id.clone(), depth: 0 }));
    rows
}

/// Number of connections matching `query` in a group and its sub-groups.
fn count_matching(store: &ConnectionStore, group: &str, query: &str) -> usize {
    let direct = store.list().iter().filter(|c| c.group.as_deref() == Some(group) && matches(c, query)).count();
    let nested: usize = store.groups().iter().filter(|g| g.parent.as_deref() == Some(group)).map(|g| count_matching(store, &g.id, query)).sum();
    direct + nested
}

/// Appends the groups below `parent` and their connections.
#[allow(clippy::too_many_arguments)]
fn add_group_level(store: &ConnectionStore, tags: &TagStore, query: &str, sort: ConnSort, collapsed: &HashSet<String>, parent: Option<&str>, depth: usize, rows: &mut Vec<RowModel>) {
    let mut groups: Vec<_> = store.groups().iter().filter(|g| g.parent.as_deref() == parent).collect();
    groups.sort_by_key(|g| g.name.to_lowercase());
    for g in groups {
        let count = count_matching(store, &g.id, query);
        if !query.is_empty() && count == 0 {
            continue;
        }
        let is_collapsed = query.is_empty() && collapsed.contains(&g.id);
        rows.push(RowModel::Group { id: g.id.clone(), name: g.name.clone(), count, collapsed: is_collapsed, depth });
        if is_collapsed {
            continue;
        }
        add_group_level(store, tags, query, sort, collapsed, Some(&g.id), depth + 1, rows);
        let mut conns: Vec<&StoredConnection> = store.list().iter().filter(|c| c.group.as_deref() == Some(g.id.as_str()) && matches(c, query)).collect();
        sort_conns(&mut conns, sort, tags);
        rows.extend(conns.into_iter().map(|c| RowModel::Conn { id: c.id.clone(), depth: depth + 1 }));
    }
}

/// Two letter monogram of a driver.
fn monogram(d: DriverId) -> &'static str {
    match d {
        DriverId::Postgres => "PG",
        DriverId::MySql => "My",
        DriverId::Sqlite => "SQ",
    }
}

/// Subtitle of a connection row.
fn subtitle(c: &StoredConnection) -> String {
    match c.driver {
        DriverId::Sqlite => c.database.clone(),
        _ if c.database.is_empty() => c.host.clone(),
        _ => format!("{} / {}", c.host, c.database),
    }
}

/// Draws the list pane contents and returns the requested action.
pub fn show(ui: &mut Ui, state: &mut ListState, store: &ConnectionStore, tags: &TagStore) -> Option<ListAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(widgets::semibold("Connections", 18.0).color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let resp = widgets::primary_button(ui, "+  New");
            let popup_id = ui.make_persistent_id("new_menu");
            if resp.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(200.0);
                widgets::flat_menu_style(ui);
                for (label, d) in [("PostgreSQL", DriverId::Postgres), ("MySQL / MariaDB", DriverId::MySql), ("SQLite file", DriverId::Sqlite)] {
                    if ui.button(label).clicked() {
                        action = Some(ListAction::NewConnection(d));
                    }
                }
                ui.separator();
                if ui.button("Group...").clicked() {
                    action = Some(ListAction::NewGroup);
                }
                if ui.button("Import from URL...").clicked() {
                    action = Some(ListAction::ImportUrl);
                }
            });
        });
    });
    widgets::search_box(ui, &mut state.query, "Search connections");
    let rows = build_rows(store, tags, &state.query, state.sort, &state.collapsed);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("conn_sort").selected_text(state.sort.label()).width(150.0).show_ui(ui, |ui| {
            for s in [ConnSort::Name, ConnSort::Tag, ConnSort::Driver] {
                ui.selectable_value(&mut state.sort, s, s.label());
            }
        });
        let n = store.list().len();
        widgets::muted(ui, &format!("{n} {}", if n == 1 { "connection" } else { "connections" }));
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if rows.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                widgets::muted(ui, if store.list().is_empty() { "No connections yet. Press New to create one." } else { "No connection matches the search." });
            });
        }
        for row in &rows {
            match row {
                RowModel::Group { id, name, count, collapsed, depth } => {
                    if let Some(a) = group_row(ui, id, name, *count, *collapsed, *depth) {
                        action = Some(a);
                    }
                }
                RowModel::Conn { id, depth } => {
                    if let Some(c) = store.get(id) {
                        let selected = state.selected.as_deref() == Some(id.as_str());
                        if let Some(a) = conn_row(ui, c, selected, *depth, tags) {
                            action = Some(a);
                        }
                    }
                }
            }
        }
    });
    action
}

/// A group header row.
fn group_row(ui: &mut Ui, id: &str, name: &str, count: usize, collapsed: bool, depth: usize) -> Option<ListAction> {
    let mut action = None;
    let resp = ui
        .horizontal(|ui| {
            ui.add_space(depth as f32 * 14.0);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, theme::RADIUS_CTRL, theme::FIELD_ROW_BG);
            }
            let chevron = egui::Rect::from_min_size(rect.min, egui::vec2(24.0, rect.height()));
            crate::glyphs::paint(ui.painter(), chevron, if collapsed { Ico::ChevronRight } else { Ico::ChevronDown }, theme::LABEL);
            let folder = egui::Rect::from_min_size(rect.min + egui::vec2(22.0, 0.0), egui::vec2(24.0, rect.height()));
            crate::glyphs::paint(ui.painter(), folder, Ico::Folder, theme::SECONDARY);
            ui.painter().text(rect.min + egui::vec2(50.0, rect.height() / 2.0), egui::Align2::LEFT_CENTER, format!("{name}  ({count})"), egui::FontId::new(13.0, theme::semibold()), theme::TEXT);
            resp
        })
        .inner;
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        action = Some(ListAction::ToggleGroup(id.to_owned()));
    }
    resp.context_menu(|ui| {
        crate::ui::widgets::flat_menu_style(ui);
        if ui.add(egui::Button::new(egui::RichText::new("Delete group...").color(theme::ERROR))).clicked() {
            action = Some(ListAction::DeleteGroup(id.to_owned()));
            ui.close_menu();
        }
    });
    action
}

/// A connection row with its context menu.
fn conn_row(ui: &mut Ui, c: &StoredConnection, selected: bool, depth: usize, tags: &TagStore) -> Option<ListAction> {
    let mut action = None;
    let tag_color = c.tag.as_ref().and_then(|t| tags.color(t)).and_then(|h| theme::parse_hex_color(&h));
    let accent = tag_color.unwrap_or(theme::SECONDARY);
    let indent = depth as f32 * 14.0;
    let (fill, border) = if selected { (theme::INPUT_FILL_BLUE, theme::PRIMARY) } else { (theme::FIELD_ROW_BG, theme::FIELD_ROW_BORDER) };
    let inner = ui.horizontal(|ui| {
        ui.add_space(indent);
        Frame::new().fill(fill).stroke(Stroke::new(1.0_f32, border)).corner_radius(theme::RADIUS_CARD).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                widgets::text_badge(ui, monogram(c.driver), accent, 34.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(widgets::semibold(&c.name, 14.0).color(theme::TEXT));
                    ui.add(egui::Label::new(RichText::new(subtitle(c)).size(12.0).color(theme::MUTED)).truncate());
                });
                if let Some(t) = &c.tag {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widgets::status_dot(ui, accent, t);
                    });
                }
            });
        })
    });
    let resp = inner.response.interact(Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    if resp.double_clicked() {
        action = Some(ListAction::Connect(c.id.clone()));
    } else if resp.clicked() {
        action = Some(ListAction::Select(c.id.clone()));
    }
    resp.context_menu(|ui| {
        widgets::flat_menu_style(ui);
        let mut pick = |ui: &mut Ui, label: &str, a: ListAction| {
            if ui.button(label).clicked() {
                action = Some(a);
                ui.close_menu();
            }
        };
        pick(ui, "Connect", ListAction::Connect(c.id.clone()));
        pick(ui, "Edit...", ListAction::Edit(c.id.clone()));
        pick(ui, "Duplicate", ListAction::Duplicate(c.id.clone()));
        pick(ui, "Copy as URL", ListAction::CopyUrl(c.id.clone()));
        ui.separator();
        if ui.add(egui::Button::new(RichText::new("Delete").color(theme::ERROR))).clicked() {
            action = Some(ListAction::Delete(c.id.clone()));
            ui.close_menu();
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_store::connections::StoredConnection;

    fn conn(name: &str, driver: DriverId, tag: Option<&str>, group: Option<&str>) -> StoredConnection {
        let mut c = StoredConnection::new(name, driver);
        c.tag = tag.map(str::to_owned);
        c.group = group.map(str::to_owned);
        c
    }

    fn ids(rows: &[RowModel], store: &ConnectionStore) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                RowModel::Group { name, .. } => format!("[{name}]"),
                RowModel::Conn { id, .. } => store.get(id).unwrap().name.clone(),
            })
            .collect()
    }

    fn sample() -> (ConnectionStore, TagStore) {
        let mut s = ConnectionStore::default();
        let g = s.add_group("Work", None);
        s.upsert(conn("beta", DriverId::MySql, Some("production"), Some(&g)));
        s.upsert(conn("Alpha", DriverId::Postgres, Some("local"), Some(&g)));
        s.upsert(conn("zed", DriverId::Sqlite, None, None));
        s.upsert(conn("Cat", DriverId::Postgres, Some("staging"), None));
        (s, TagStore::default())
    }

    /// Groups come first, names sort case-insensitively.
    #[test]
    fn default_order() {
        let (s, t) = sample();
        let rows = build_rows(&s, &t, "", ConnSort::Name, &HashSet::new());
        assert_eq!(ids(&rows, &s), vec!["[Work]", "Alpha", "beta", "Cat", "zed"]);
    }

    /// Sorting by tag follows the tag order, untagged last; by driver groups drivers.
    #[test]
    fn sort_by_tag_and_driver() {
        let (s, t) = sample();
        let rows = build_rows(&s, &t, "", ConnSort::Tag, &HashSet::new());
        assert_eq!(ids(&rows, &s), vec!["[Work]", "Alpha", "beta", "Cat", "zed"]);
        let rows = build_rows(&s, &t, "", ConnSort::Driver, &HashSet::new());
        assert_eq!(ids(&rows, &s), vec!["[Work]", "beta", "Alpha", "Cat", "zed"]);
    }

    /// Search filters by name, host, database and tag; empty groups disappear.
    #[test]
    fn search_filters_and_hides_empty_groups() {
        let (s, t) = sample();
        assert_eq!(ids(&build_rows(&s, &t, "cat", ConnSort::Name, &HashSet::new()), &s), vec!["Cat"]);
        assert_eq!(ids(&build_rows(&s, &t, "production", ConnSort::Name, &HashSet::new()), &s), vec!["[Work]", "beta"]);
        assert!(build_rows(&s, &t, "nomatch", ConnSort::Name, &HashSet::new()).is_empty());
    }

    /// A collapsed group hides its connections, unless a search is active.
    #[test]
    fn collapsed_groups() {
        let (s, t) = sample();
        let gid = s.groups()[0].id.clone();
        let collapsed: HashSet<String> = [gid].into_iter().collect();
        assert_eq!(ids(&build_rows(&s, &t, "", ConnSort::Name, &collapsed), &s), vec!["[Work]", "Cat", "zed"]);
        assert_eq!(ids(&build_rows(&s, &t, "alpha", ConnSort::Name, &collapsed), &s), vec!["[Work]", "Alpha"]);
    }

    /// The list renders its rows and reports a click as a selection (egui_kittest smoke test).
    #[test]
    fn list_renders_rows_and_reports_clicks() {
        use egui_kittest::kittest::Queryable;
        let (store, tags) = sample();
        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, state: &mut (ListState, Option<ListAction>)| {
                // fonts apply from the next frame: skip drawing until the theme is active
                let themed = egui::Id::new("themed");
                if ui.ctx().data(|d| d.get_temp::<bool>(themed)).is_none() {
                    ui.ctx().data_mut(|d| d.insert_temp(themed, true));
                    crate::theme::apply(ui.ctx());
                    ui.ctx().request_repaint();
                    return;
                }
                if let Some(a) = show(ui, &mut state.0, &store, &tags) {
                    state.1 = Some(a);
                }
            },
            (ListState::default(), None),
        );
        harness.run();
        harness.get_by_label("Connections");
        // labels are not clickable widgets, so click the row with real pointer events
        let b = harness.get_by_label("Cat").bounding_box().expect("label has a position");
        let pos = egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        harness.step();
        harness.input_mut().events.push(button(true));
        harness.step();
        harness.input_mut().events.push(button(false));
        harness.run();
        let picked = harness.state().1.clone();
        let expected = store.list().iter().find(|c| c.name == "Cat").map(|c| c.id.clone()).unwrap();
        assert_eq!(picked, Some(ListAction::Select(expected)));
    }
}
