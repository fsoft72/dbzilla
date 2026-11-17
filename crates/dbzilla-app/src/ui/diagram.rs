//! ER diagram tab (SPEC 11.3): tables as boxes, one line per foreign key.

use super::diagram_pending::{DiagramPending, Markers};
use super::structure::{self, StructAction, StructEnv};
use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::ddl::StructuralOp;
use dbzilla_core::diagram::{Diagram, DiagramEdge, DiagramSort, DocLink, EdgeKind};
use dbzilla_core::model::{TableRef, TableStructure};
use std::collections::BTreeMap;
use dbzilla_store::diagrams::Positions;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Height of the table header and of one column row, at zoom 1.
const HEADER_H: f32 = 26.0;
const ROW_H: f32 = 18.0;
/// Width of one character of the box text, at zoom 1, used to size the boxes.
const CHAR_W: f32 = 7.0;
const MIN_BOX_W: f32 = 150.0;
const MAX_BOX_W: f32 = 380.0;
const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 2.5;
/// Below this zoom the box text is not drawn.
const TEXT_ZOOM: f32 = 0.45;
const MARGIN: f32 = 40.0;
/// Distance from the left or right edge of a box, in screen points, where a column can start a link.
const PORT_PX: f32 = 10.0;
/// Distance from a line, in screen points, that counts as pointing at it.
const EDGE_HIT_PX: f32 = 7.0;

/// Requests of the diagram toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagramAction {
    /// Reload the schema and lay the tables out again.
    Rearrange,
    /// Open the data of a table.
    OpenTable(String),
    /// Go back to the workspace (close the tab).
    Close,
    /// The positions changed: keep them for the next time.
    Save(Positions),
    /// The user dragged from one column to another: ask what kind of link to make.
    RequestLink(DocLink),
    /// The user deleted a link they had drawn.
    DeleteLink(DocLink),
    /// The user asked to drop a real foreign key of the database.
    DeleteForeignKey { table: String, constraint: String },
    /// The "Inferred links" switch changed: keep it for the next time.
    SetInferred(bool),
    /// A message for the user (error flag, text).
    Notice(bool, String),
    /// Apply the pending changes to the database.
    Apply,
    /// The colours chosen for tables changed: keep them (table name to palette index).
    SetColors(BTreeMap<String, usize>),
}

/// What the diagram needs to know about the database to edit tables.
pub struct DiagramEnv<'a> {
    pub caps: &'a Capabilities,
    /// Data types of the dialect.
    pub types: &'a [&'a str],
}

/// Page of the table settings window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Columns,
    Indexes,
}

/// The table settings window.
struct Settings {
    /// Key of the draft: the name of an existing table, the current name of a new one.
    key: String,
    /// Name the table is drawn with.
    shown: String,
    page: SettingsPage,
}

/// A link being dragged from a column.
struct Linking {
    table: usize,
    column: usize,
    /// The drag started on the right edge of the box.
    right: bool,
}

/// State of a diagram tab.
pub struct DiagramTab {
    pub loading: bool,
    /// Tables that could not be read.
    pub skipped: Vec<String>,
    structures: Vec<TableStructure>,
    diagram: Diagram,
    groups: Vec<Option<usize>>,
    sort: DiagramSort,
    positions: Vec<(f32, f32)>,
    sizes: Vec<(f32, f32)>,
    pan: Vec2,
    zoom: f32,
    fit_pending: bool,
    selected: Option<usize>,
    /// Draw the dashed links guessed from column names.
    show_inferred: bool,
    /// The search box takes the keyboard focus on the next frame.
    focus_search: bool,
    dragging: Option<usize>,
    search: String,
    found: Vec<usize>,
    /// Match the search stands on, as an index into `found`; none until the user steps.
    cursor: Option<usize>,
    /// The next reload ignores the saved positions (Rearrange).
    forget_saved: bool,
    /// Links drawn by the user, kept in the application only.
    links: Vec<DocLink>,
    linking: Option<Linking>,
    /// Line the context menu was opened on.
    menu_edge: Option<usize>,
    /// The positions changed and must be handed to the store.
    save_pending: bool,
    /// Why a foreign key of the database cannot be dropped here, when it cannot.
    fk_drop_blocked: Option<String>,
    /// Changes waiting to be applied.
    pending: DiagramPending,
    markers: Markers,
    settings: Option<Settings>,
    /// Table the right click menu was opened on, with the column under the pointer.
    menu_table: Option<(usize, Option<usize>)>,
    /// Palette index chosen for a table, by name.
    colors: BTreeMap<String, usize>,
    /// Where a new table is created (schema) and the type of its first column.
    template: TableRef,
    int_type: String,
    /// Centre the view on this table at the next frame.
    jump_to_name: Option<String>,
}

impl DiagramTab {
    /// An empty tab waiting for the tables of the schema.
    pub fn new() -> Self {
        DiagramTab {
            loading: true,
            skipped: Vec::new(),
            structures: Vec::new(),
            diagram: Diagram::default(),
            groups: Vec::new(),
            sort: DiagramSort::Name,
            positions: Vec::new(),
            sizes: Vec::new(),
            pan: Vec2::ZERO,
            zoom: 1.0,
            fit_pending: true,
            selected: None,
            show_inferred: true,
            focus_search: false,
            dragging: None,
            search: String::new(),
            found: Vec::new(),
            cursor: None,
            forget_saved: false,
            links: Vec::new(),
            linking: None,
            menu_edge: None,
            save_pending: false,
            fk_drop_blocked: None,
            pending: DiagramPending::default(),
            markers: Markers::default(),
            settings: None,
            menu_table: None,
            colors: BTreeMap::new(),
            template: TableRef::default(),
            int_type: "integer".into(),
            jump_to_name: None,
        }
    }

    /// Where new tables are created and the type of their first column.
    pub fn configure(&mut self, template: TableRef, int_type: &str) {
        self.template = template;
        self.int_type = int_type.to_owned();
    }

    /// Colours chosen for tables.
    pub fn set_colors(&mut self, colors: BTreeMap<String, usize>) {
        self.colors = colors;
    }

    /// Number of pending changes.
    pub fn pending_count(&self) -> usize {
        self.pending.change_count()
    }

    /// The pending changes as operations, or the reason they cannot be applied.
    pub fn pending_ops(&self, caps: &Capabilities, sqlite: bool) -> Result<Vec<StructuralOp>, String> {
        self.pending.to_ops(&self.structures, caps, sqlite)
    }

    /// Forgets every pending change and closes the settings window.
    pub fn discard_pending(&mut self) {
        self.pending.clear();
        self.settings = None;
        self.reapply(None);
    }

    /// Positions of every table, new ones included (to keep them once the tables exist).
    pub fn positions_all(&self) -> Positions {
        self.diagram.tables.iter().zip(&self.positions).map(|(t, p)| (t.name.clone(), *p)).collect()
    }

    /// Stages a foreign key from the column of a link to the other one.
    pub fn stage_foreign_key(&mut self, link: &DocLink, name: Option<String>, on_update: &str, on_delete: &str) -> Result<(), String> {
        self.pending.stage_fk(&self.structures, (&link.from_table, &link.from_column), (&link.to_table, &link.to_column), name, on_update, on_delete)?;
        self.reapply(None);
        Ok(())
    }

    /// Stages the drop of a foreign key of the database.
    pub fn stage_drop_foreign_key(&mut self, table: &str, constraint: &str) {
        self.pending.stage_drop_fk(table, constraint);
        self.reapply(None);
    }

    /// Type of a column as the diagram draws it (pending changes included).
    pub fn column_type(&self, table: &str, column: &str) -> Option<String> {
        self.diagram.tables.iter().find(|t| t.name == table)?.columns.iter().find(|c| c.name == column).map(|c| c.type_name.clone())
    }

    /// Adds a table called `untitled_table_N`, selects it and opens its settings.
    pub fn add_table(&mut self) {
        let name = self.pending.add_table(&self.structures, &self.template, &self.int_type);
        self.reapply(None);
        self.open_settings(&name);
        self.jump_to_name = Some(name);
    }

    /// Marks a table to be dropped.
    pub fn drop_table(&mut self, name: &str) {
        self.settings = None;
        self.pending.drop_table(name);
        self.reapply(None);
    }

    /// Opens the settings window of a table.
    fn open_settings(&mut self, name: &str) {
        if self.markers.dropped.contains(name) {
            return;
        }
        self.settings = Some(Settings { key: name.to_owned(), shown: name.to_owned(), page: SettingsPage::Columns });
    }

    /// Goes back one pending step.
    pub fn undo(&mut self) {
        self.settings = None;
        if self.pending.undo() {
            self.reapply(None);
        }
    }

    /// Goes forward one pending step.
    pub fn redo(&mut self) {
        self.settings = None;
        if self.pending.redo() {
            self.reapply(None);
        }
    }

    /// Shows or hides the lines guessed from column names.
    pub fn set_show_inferred(&mut self, show: bool) {
        self.show_inferred = show;
    }

    /// Whether the lines guessed from column names are shown (tests).
    #[cfg(test)]
    pub fn show_inferred(&self) -> bool {
        self.show_inferred
    }

    /// Moves the keyboard focus to the search box.
    pub fn focus_search(&mut self) {
        self.focus_search = true;
    }

    /// Steps to the next (or previous) search match and returns its table.
    fn step_found(&mut self, forward: bool) -> Option<usize> {
        let n = self.found.len();
        if n == 0 {
            return None;
        }
        let next = match self.cursor {
            None if forward => 0,
            None => n - 1,
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
        };
        self.cursor = Some(next);
        Some(self.found[next])
    }

    /// Sets, or clears, the reason a foreign key of the database cannot be dropped.
    pub fn set_fk_drop_blocked(&mut self, reason: Option<String>) {
        self.fk_drop_blocked = reason;
    }

    /// The reason a foreign key of the database cannot be dropped, when it cannot (tests).
    #[cfg(test)]
    pub fn fk_drop_blocked(&self) -> Option<&str> {
        self.fk_drop_blocked.as_deref()
    }

    /// Takes the tables read from the database and lays them out, using the saved positions
    /// unless a Rearrange asked for a fresh layout. Returns the positions to save when the layout is new.
    pub fn set_tables(&mut self, tables: Vec<TableStructure>, skipped: Vec<String>, saved: &Positions, links: Vec<DocLink>) -> Option<Positions> {
        self.structures = tables;
        self.links = links;
        self.skipped = skipped;
        self.loading = false;
        let use_saved = !self.forget_saved && !saved.is_empty();
        self.forget_saved = false;
        self.rebuild(use_saved.then_some(saved));
        std::mem::take(&mut self.save_pending).then(|| self.positions_by_name())
    }

    /// Marks the tab as reloading; the saved positions are kept.
    pub fn start_loading(&mut self) {
        self.loading = true;
    }

    /// Marks the tab as reloading with a fresh layout (Rearrange).
    pub fn start_rearrange(&mut self) {
        self.loading = true;
        self.forget_saved = true;
    }

    /// Links drawn by the user.
    pub fn links(&self) -> &[DocLink] {
        &self.links
    }

    /// Adds a link drawn by the user; false when it is already there.
    pub fn add_link(&mut self, link: DocLink) -> bool {
        if self.links.contains(&link) {
            return false;
        }
        self.links.push(link);
        self.refresh_edges();
        true
    }

    /// Removes a link drawn by the user.
    pub fn remove_link(&mut self, link: &DocLink) {
        self.links.retain(|l| l != link);
        self.refresh_edges();
    }

    /// The diagram of the structures with the pending changes and the links drawn by the user.
    fn build_diagram(&self) -> Diagram {
        let mut d = Diagram::build(&self.pending.effective(&self.structures), self.sort);
        d.add_manual(&self.links);
        d
    }

    /// Draws again after a pending change: tables keep their positions by name (`renamed` is old and new name
    /// of a table being renamed), the new ones are placed where they do not overlap.
    fn reapply(&mut self, renamed: Option<(&str, &str)>) {
        let mut by_name = self.positions_all();
        let selected = self.selected.and_then(|i| self.diagram.tables.get(i)).map(|t| t.name.clone());
        let selected = match (&selected, renamed) {
            (Some(s), Some((old, new))) if s == old => Some(new.to_owned()),
            _ => selected,
        };
        if let Some((old, new)) = renamed {
            if let Some(p) = by_name.remove(old) {
                by_name.insert(new.to_owned(), p);
            }
        }
        self.relayout(Some(&by_name));
        self.selected = selected.and_then(|n| self.diagram.tables.iter().position(|t| t.name == n));
        self.refresh_search();
    }

    /// Rebuilds only the lines: the tables keep their order, so the positions stay valid.
    fn refresh_edges(&mut self) {
        self.diagram = self.build_diagram();
        self.selected = self.selected.filter(|&i| i < self.positions.len());
    }

    /// The link a line stands for, by names.
    fn link_of(&self, e: &DiagramEdge) -> Option<DocLink> {
        let (from, to) = (&self.diagram.tables[e.from_table], &self.diagram.tables[e.to_table]);
        Some(DocLink { from_table: from.name.clone(), from_column: from.columns[e.from_column].name.clone(), to_table: to.name.clone(), to_column: to.columns.get(e.to_column?)?.name.clone() })
    }

    /// Positions by table name, as kept in the store.
    fn positions_by_name(&self) -> Positions {
        self.diagram.tables.iter().zip(&self.positions).filter(|(t, _)| !self.markers.created.contains(&t.name)).map(|(t, p)| (t.name.clone(), *p)).collect()
    }

    /// Builds the diagram from the loaded tables with the current sort, and lays it out.
    /// A layout that does not come from the saved positions is saved.
    fn rebuild(&mut self, saved: Option<&Positions>) {
        self.relayout(saved);
        self.selected = None;
        self.dragging = None;
        self.fit_pending = true;
        self.refresh_search();
    }

    /// Builds the diagram and lays it out, using `saved` positions when given (a new layout is saved).
    fn relayout(&mut self, saved: Option<&Positions>) {
        self.markers = self.pending.markers();
        self.diagram = self.build_diagram();
        self.groups = self.diagram.groups();
        self.sizes = self.diagram.tables.iter().map(box_size).collect();
        self.positions = self.diagram.layout(&self.sizes);
        match saved {
            Some(saved) => self.diagram.apply_saved(&mut self.positions, &self.sizes, saved),
            None => self.save_pending = true,
        }
    }

    /// True for a foreign key line that is not in the database yet.
    fn edge_is_new(&self, e: &DiagramEdge) -> bool {
        let from = &self.diagram.tables[e.from_table];
        e.kind == EdgeKind::Real && (self.markers.created.contains(&from.name) || self.markers.new_fks.contains(&(from.name.clone(), from.columns[e.from_column].name.clone())))
    }

    /// Colour of table `i`: the one chosen by the user, else the one of its group.
    fn color_of(&self, i: usize) -> Option<Color32> {
        let chosen = self.colors.get(&self.diagram.tables[i].name).map(|&c| theme::DIAGRAM_GROUPS[c % theme::DIAGRAM_GROUPS.len()]);
        chosen.or_else(|| group_color(self.groups[i]))
    }

    /// Recomputes the search matches after the text or the tables changed.
    fn refresh_search(&mut self) {
        self.found = self.diagram.search(&self.search);
        self.cursor = None;
    }

    /// Number of tables in the diagram.
    #[cfg(test)]
    pub fn table_count(&self) -> usize {
        self.diagram.tables.len()
    }

    /// Number of foreign key lines in the diagram, with or without the guessed ones.
    #[cfg(test)]
    pub fn edge_count(&self, inferred: bool) -> usize {
        self.diagram.edges.iter().filter(|e| (e.kind == EdgeKind::Inferred) == inferred).count()
    }

    /// Number of links guessed from column names.
    fn inferred_count(&self) -> usize {
        self.diagram.edges.iter().filter(|e| e.kind == EdgeKind::Inferred).count()
    }

    /// Position of the table called `name`.
    #[cfg(test)]
    pub fn position_of(&self, name: &str) -> Option<(f32, f32)> {
        self.diagram.tables.iter().position(|t| t.name == name).map(|i| self.positions[i])
    }

    /// Screen rectangle of table `i`.
    fn rect_of(&self, i: usize, origin: Pos2) -> Rect {
        let (x, y) = self.positions[i];
        let (w, h) = self.sizes[i];
        Rect::from_min_size(origin + self.pan + vec2(x, y) * self.zoom, vec2(w, h) * self.zoom)
    }

    /// Zoom and pan so every table is visible.
    fn fit(&mut self, canvas: Rect) {
        self.fit_pending = false;
        if self.positions.is_empty() {
            return;
        }
        let (mut max_x, mut max_y) = (0.0_f32, 0.0_f32);
        for (i, (x, y)) in self.positions.iter().enumerate() {
            max_x = max_x.max(x + self.sizes[i].0);
            max_y = max_y.max(y + self.sizes[i].1);
        }
        let zoom = ((canvas.width() - 2.0 * MARGIN) / max_x.max(1.0)).min((canvas.height() - 2.0 * MARGIN) / max_y.max(1.0)).clamp(MIN_ZOOM, 1.0);
        self.zoom = zoom;
        self.pan = vec2(MARGIN, MARGIN);
    }

    /// Centres the view on table `i`.
    fn jump_to(&mut self, i: usize, canvas: Rect) {
        let (x, y) = self.positions[i];
        let (w, h) = self.sizes[i];
        self.pan = canvas.size() / 2.0 - vec2(x + w / 2.0, y + h / 2.0) * self.zoom;
        self.selected = Some(i);
    }

    /// Zooms keeping the point `around` (screen coordinates) fixed.
    fn zoom_by(&mut self, factor: f32, around: Pos2, origin: Pos2) {
        let new = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = new / self.zoom;
        let rel = around - origin;
        self.pan = rel - (rel - self.pan) * k;
        self.zoom = new;
    }
}

/// Size of the box of a table at zoom 1.
fn box_size(t: &dbzilla_core::diagram::DiagramTable) -> (f32, f32) {
    let longest = t.columns.iter().map(|c| c.name.chars().count() + c.type_name.chars().count() + 4).max().unwrap_or(0).max(t.name.chars().count() + 4);
    ((longest as f32 * CHAR_W + 28.0).clamp(MIN_BOX_W, MAX_BOX_W), HEADER_H + ROW_H * t.columns.len().max(1) as f32 + 6.0)
}

/// Colour of a group (grey for tables with no link).
fn group_color(group: Option<usize>) -> Option<Color32> {
    group.map(|g| theme::DIAGRAM_GROUPS[g % theme::DIAGRAM_GROUPS.len()])
}

/// Draws the diagram tab and returns what the user asked for.
pub fn show(ui: &mut Ui, tab: &mut DiagramTab, env: &DiagramEnv) -> Vec<DiagramAction> {
    let mut actions = Vec::new();
    let mut canvas_jump: Option<usize> = None;
    egui::TopBottomPanel::top(ui.id().with("diagram_bar")).exact_height(36.0).frame(egui::Frame::new().fill(theme::NAVY_DARK).inner_margin(egui::Margin::symmetric(10, 4))).show_inside(ui, |ui| {
        ui.horizontal_centered(|ui| {
            if widgets::secondary_button(ui, "Workspace").clicked() {
                actions.push(DiagramAction::Close);
            }
            ui.separator();
            if widgets::ico_button(ui, Ico::Plus, "Zoom in").clicked() {
                tab.zoom_by(1.25, Pos2::ZERO, Pos2::ZERO);
            }
            if widgets::ico_button(ui, Ico::Minus, "Zoom out").clicked() {
                tab.zoom_by(0.8, Pos2::ZERO, Pos2::ZERO);
            }
            if widgets::secondary_button(ui, "Reset").on_hover_text("Zoom 100%").clicked() {
                tab.zoom = 1.0;
                tab.pan = vec2(MARGIN, MARGIN);
            }
            if widgets::secondary_button(ui, "Fit").on_hover_text("Show every table").clicked() {
                tab.fit_pending = true;
            }
            ui.separator();
            let before = tab.sort;
            egui::ComboBox::from_id_salt("diagram_sort").selected_text(if tab.sort == DiagramSort::Name { "Sort: Name" } else { "Sort: Column count" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut tab.sort, DiagramSort::Name, "Name");
                ui.selectable_value(&mut tab.sort, DiagramSort::ColumnCount, "Column count");
            });
            if tab.sort != before {
                tab.rebuild(None);
            }
            let clean = tab.pending.is_empty();
            if ui.add_enabled(clean, egui::Button::new("Rearrange")).on_hover_text("Reload the schema and lay the tables out again").on_disabled_hover_text("Apply or discard the pending changes first").clicked() {
                actions.push(DiagramAction::Rearrange);
            }
            ui.separator();
            if widgets::secondary_button(ui, "Add table").on_hover_text("Add a table (applied with the other pending changes)").clicked() {
                tab.add_table();
            }
            let selected = tab.selected.and_then(|i| tab.diagram.tables.get(i)).map(|t| t.name.clone());
            if ui.add_enabled(selected.is_some(), egui::Button::new("Table settings")).on_hover_text("Name, colour, columns and indexes of the selected table").clicked() {
                if let Some(n) = &selected {
                    tab.open_settings(n);
                }
            }
            if ui.add_enabled(tab.pending.can_undo(), egui::Button::new("Undo")).clicked() {
                tab.undo();
            }
            if ui.add_enabled(tab.pending.can_redo(), egui::Button::new("Redo")).clicked() {
                tab.redo();
            }
            let count = tab.pending.change_count();
            if ui.add_enabled(count > 0, egui::Button::new(format!("Apply ({count})"))).on_hover_text("Write the pending changes to the database (Mod+S)").clicked() {
                actions.push(DiagramAction::Apply);
            }
            if ui.add_enabled(count > 0, egui::Button::new("Discard")).on_hover_text("Forget the pending changes").clicked() {
                tab.discard_pending();
            }
            ui.separator();
            let guessed = tab.inferred_count();
            let switch = ui.add_enabled(guessed > 0, egui::Checkbox::new(&mut tab.show_inferred, format!("Inferred links ({guessed})"))).on_hover_text("Dashed lines for columns named like id_user or user_id when the database has no foreign key");
            if switch.changed() {
                actions.push(DiagramAction::SetInferred(tab.show_inferred));
            }
            if widgets::secondary_button(ui, "Export SVG").on_hover_text("Save the diagram as an SVG image").clicked() {
                actions.push(export_svg(tab));
            }
            ui.separator();
            let r = widgets::search_box(ui, &mut tab.search, "Find table or column");
            if std::mem::take(&mut tab.focus_search) {
                r.request_focus();
            }
            if r.changed() {
                tab.refresh_search();
            }
            // Enter and Down go to the next match, Shift+Enter and Up to the previous one
            let step = if r.has_focus() || r.lost_focus() {
                ui.input_mut(|i| {
                    if i.consume_key(Modifiers::NONE, Key::ArrowUp) || (r.lost_focus() && i.consume_key(Modifiers::SHIFT, Key::Enter)) {
                        Some(false)
                    } else if i.consume_key(Modifiers::NONE, Key::ArrowDown) || (r.lost_focus() && i.consume_key(Modifiers::NONE, Key::Enter)) {
                        Some(true)
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            if let Some(forward) = step {
                canvas_jump = tab.step_found(forward);
                r.request_focus();
            }
            if !tab.search.trim().is_empty() {
                ui.label(egui::RichText::new(format!("{} found", tab.found.len())).color(theme::MUTED));
            }
            if tab.loading {
                ui.spinner();
            }
        });
    });
    if !tab.skipped.is_empty() {
        ui.label(egui::RichText::new(format!("Not shown: {}", tab.skipped.join("; "))).color(theme::TERTIARY));
    }
    let avail = ui.available_size();
    let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
    let canvas = resp.rect;
    painter.rect_filled(canvas, 0.0, theme::DEEP_DARK);
    if tab.positions.is_empty() {
        let text = if tab.loading { "Reading the schema..." } else { "No tables in this schema" };
        painter.text(canvas.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), theme::MUTED);
        return actions;
    }
    let origin = canvas.min;
    if tab.fit_pending {
        tab.fit(canvas);
    }
    if let Some(name) = tab.jump_to_name.take() {
        canvas_jump = tab.diagram.tables.iter().position(|t| t.name == name).or(canvas_jump);
    }
    if let Some(i) = canvas_jump {
        tab.jump_to(i, canvas);
    }
    handle_input(ui, tab, &resp, origin, &mut actions);
    painter.text(canvas.left_bottom() + vec2(12.0, -10.0), Align2::LEFT_BOTTOM, "Drag from the edge of a column to another column to link them. Right click a drawn link to delete it.", FontId::proportional(11.0), theme::MUTED);
    if std::mem::take(&mut tab.save_pending) {
        actions.push(DiagramAction::Save(tab.positions_by_name()));
    }
    paint_diagram(&painter.with_clip_rect(canvas), tab, origin, resp.hover_pos());
    settings_window(ui.ctx(), tab, env, &mut actions);
    actions
}

/// Colour swatches, the column and index editors and Delete table, in a window over the diagram.
fn settings_window(ctx: &egui::Context, tab: &mut DiagramTab, env: &DiagramEnv, actions: &mut Vec<DiagramAction>) {
    let Some(mut st) = tab.settings.take() else { return };
    if !tab.pending.begin_edit(&tab.structures, &st.key) {
        tab.pending.end_edit();
        return;
    }
    let before = tab.pending.signature();
    let mut colors = tab.colors.clone();
    let mut open = true;
    let mut delete = false;
    let mut fk_hint = false;
    // an existing table cannot get a foreign key from the column editor: the line is dragged in the diagram
    let mut caps = env.caps.clone();
    caps.add_fk_after_create = false;
    let editor_env = StructEnv { caps: &caps, types: env.types, editable: true };
    egui::Window::new(format!("Table settings: {}", st.shown)).id(egui::Id::new("diagram_table_settings")).open(&mut open).default_size([820.0, 460.0]).resizable(true).show(ctx, |ui| {
        let Some(draft) = tab.pending.draft_mut(&st.key) else { return };
        ui.horizontal(|ui| {
            widgets::field_label(ui, "Colour");
            let current = colors.get(&st.shown).copied();
            if ui.selectable_label(current.is_none(), "Auto").clicked() {
                colors.remove(&st.shown);
            }
            for (i, c) in theme::DIAGRAM_GROUPS.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                ui.painter().rect_filled(rect, theme::RADIUS_CTRL, *c);
                if current == Some(i) {
                    ui.painter().rect_stroke(rect.expand(2.0), theme::RADIUS_CTRL, Stroke::new(2.0_f32, theme::TEXT), StrokeKind::Outside);
                }
                if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    colors.insert(st.shown.clone(), i);
                }
            }
            ui.separator();
            ui.selectable_value(&mut st.page, SettingsPage::Columns, "Columns");
            ui.selectable_value(&mut st.page, SettingsPage::Indexes, "Indexes");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::danger_button(ui, "Delete table").clicked() {
                    delete = true;
                }
            });
        });
        ui.separator();
        match st.page {
            SettingsPage::Columns => {
                for a in structure::show_columns(ui, draft, &editor_env) {
                    if matches!(a, StructAction::EditFk(_)) {
                        fk_hint = true;
                    }
                }
            }
            SettingsPage::Indexes => structure::show_indexes(ui, draft, &editor_env),
        }
    });
    if fk_hint {
        actions.push(DiagramAction::Notice(false, "Drag from the edge of a column to another column to create a foreign key".into()));
    }
    let new_shown = tab.pending.draft_mut(&st.key).map(|d| d.new_name.trim().to_owned());
    if tab.pending.signature() != before {
        let renamed = new_shown.as_deref().filter(|n| *n != st.shown).map(|n| (st.shown.clone(), n.to_owned()));
        if let Some((old, new)) = &renamed {
            if let Some(c) = colors.remove(old) {
                colors.insert(new.clone(), c);
            }
        }
        tab.reapply(renamed.as_ref().map(|(o, n)| (o.as_str(), n.as_str())));
        if let Some(n) = new_shown {
            if tab.markers.created.contains(&n) {
                st.key = n.clone();
            }
            st.shown = n;
        }
    }
    if colors != tab.colors {
        tab.colors = colors.clone();
        actions.push(DiagramAction::SetColors(colors));
    }
    if delete {
        tab.pending.end_edit();
        tab.drop_table(&st.key);
    } else if open {
        tab.settings = Some(st);
    } else {
        tab.pending.end_edit();
    }
}

/// Pan, zoom, table drag, selection and double click.
fn handle_input(ui: &Ui, tab: &mut DiagramTab, resp: &egui::Response, origin: Pos2, actions: &mut Vec<DiagramAction>) {
    let hit = |p: Pos2, tab: &DiagramTab| (0..tab.positions.len()).rev().find(|&i| tab.rect_of(i, origin).contains(p));
    if resp.hovered() {
        let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            if let Some(p) = resp.hover_pos() {
                tab.zoom_by(zoom_delta, p, origin);
            }
        } else if scroll != Vec2::ZERO {
            tab.pan += scroll;
        }
    }
    if resp.drag_started() {
        // egui reports a drag after the pointer moved a little: the press position is where it started
        let start = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos());
        tab.linking = start.and_then(|p| port_at(tab, p, origin)).map(|(table, column, right)| Linking { table, column, right });
        if tab.linking.is_none() {
            tab.dragging = start.and_then(|p| hit(p, tab));
            if tab.dragging.is_some() {
                tab.selected = tab.dragging;
            }
        }
    }
    if resp.dragged() && tab.linking.is_none() {
        match tab.dragging {
            Some(i) => {
                let d = resp.drag_delta() / tab.zoom;
                tab.positions[i].0 += d.x;
                tab.positions[i].1 += d.y;
            }
            None => tab.pan += resp.drag_delta(),
        }
    }
    if resp.drag_stopped() {
        if let Some(l) = tab.linking.take() {
            let target = ui.input(|i| i.pointer.latest_pos()).and_then(|p| column_at(tab, p, origin));
            if let Some((t, c)) = target.filter(|&(t, c)| (t, c) != (l.table, l.column)) {
                let (from, to) = (&tab.diagram.tables[l.table], &tab.diagram.tables[t]);
                actions.push(DiagramAction::RequestLink(DocLink { from_table: from.name.clone(), from_column: from.columns[l.column].name.clone(), to_table: to.name.clone(), to_column: to.columns[c].name.clone() }));
            }
        } else if tab.dragging.take().is_some() {
            tab.save_pending = true;
        }
    }
    if resp.secondary_clicked() {
        let at = resp.interact_pointer_pos();
        tab.menu_edge = at.and_then(|p| edge_near(tab, p, origin));
        tab.menu_table = if tab.menu_edge.is_some() { None } else { at.and_then(|p| hit(p, tab).map(|t| (t, column_at(tab, p, origin).filter(|&(ct, _)| ct == t).map(|(_, c)| c)))) };
        if let Some((t, _)) = tab.menu_table {
            tab.selected = Some(t);
        }
    }
    resp.context_menu(|ui| {
        widgets::flat_menu_style(ui);
        if let Some((t, column)) = tab.menu_table {
            let name = tab.diagram.tables[t].name.clone();
            let dropped = tab.markers.dropped.contains(&name);
            if ui.add_enabled(!dropped, egui::Button::new("Table settings...")).clicked() {
                tab.open_settings(&name);
                ui.close_menu();
            }
            if let Some(c) = column {
                let col = tab.diagram.tables[t].columns[c].name.clone();
                if ui.button(format!("Copy column name ({col})")).clicked() {
                    ui.ctx().copy_text(col);
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.add_enabled(!dropped, egui::Button::new("Delete table")).on_disabled_hover_text("Already marked for deletion: use Undo to keep it").clicked() {
                tab.drop_table(&name);
                ui.close_menu();
            }
            return;
        }
        let edge = tab.menu_edge.and_then(|i| tab.diagram.edges.get(i));
        match edge {
            Some(e) if e.kind == EdgeKind::Manual => {
                if ui.button("Delete link").clicked() {
                    if let Some(l) = tab.link_of(e) {
                        actions.push(DiagramAction::DeleteLink(l));
                    }
                    ui.close_menu();
                }
            }
            Some(e) if e.kind == EdgeKind::Real && tab.edge_is_new(e) => {
                if ui.button("Remove new foreign key").clicked() {
                    let (table, column) = (tab.diagram.tables[e.from_table].name.clone(), tab.diagram.tables[e.from_table].columns[e.from_column].name.clone());
                    tab.pending.unstage_fk(&table, &column);
                    tab.reapply(None);
                    ui.close_menu();
                }
            }
            Some(e) if e.kind == EdgeKind::Real => {
                let table = tab.diagram.tables[e.from_table].name.clone();
                let reason = tab.fk_drop_blocked.clone().or_else(|| e.constraint.is_none().then(|| "This foreign key has no name".to_owned()));
                let resp = ui.add_enabled(reason.is_none(), egui::Button::new("Delete foreign key"));
                if let Some(r) = &reason {
                    resp.on_disabled_hover_text(r);
                } else if resp.clicked() {
                    if let Some(constraint) = e.constraint.clone() {
                        actions.push(DiagramAction::DeleteForeignKey { table, constraint });
                    }
                    ui.close_menu();
                }
            }
            Some(_) => {
                ui.add_enabled(false, egui::Button::new("Only links drawn here can be deleted"));
            }
            None => ui.close_menu(),
        }
    });
    if resp.clicked() {
        tab.selected = resp.interact_pointer_pos().and_then(|p| hit(p, tab));
    }
    if resp.double_clicked() {
        if let Some(i) = resp.interact_pointer_pos().and_then(|p| hit(p, tab)) {
            actions.push(DiagramAction::OpenTable(tab.diagram.tables[i].name.clone()));
        }
    }
    if resp.hovered() {
        let pointer = resp.hover_pos();
        let over = pointer.and_then(|p| hit(p, tab)).is_some();
        let port = pointer.and_then(|p| port_at(tab, p, origin)).is_some();
        let icon = if tab.linking.is_some() || port {
            CursorIcon::Crosshair
        } else if tab.dragging.is_some() {
            CursorIcon::Grabbing
        } else if over {
            CursorIcon::Grab
        } else {
            CursorIcon::Default
        };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// Column row under `p`, as (table, column).
fn column_at(tab: &DiagramTab, p: Pos2, origin: Pos2) -> Option<(usize, usize)> {
    let z = tab.zoom;
    (0..tab.positions.len()).rev().find_map(|i| {
        let rect = tab.rect_of(i, origin);
        if !rect.contains(p) {
            return None;
        }
        let row = ((p.y - rect.min.y) / z - HEADER_H) / ROW_H;
        (row >= 0.0 && (row as usize) < tab.diagram.tables[i].columns.len()).then_some((i, row as usize))
    })
}

/// Column whose left or right edge is under `p`, with the side: where a link can start.
fn port_at(tab: &DiagramTab, p: Pos2, origin: Pos2) -> Option<(usize, usize, bool)> {
    let (t, c) = column_at(tab, p, origin)?;
    let rect = tab.rect_of(t, origin);
    if (p.x - rect.max.x).abs() <= PORT_PX {
        Some((t, c, true))
    } else if (p.x - rect.min.x).abs() <= PORT_PX {
        Some((t, c, false))
    } else {
        None
    }
}

/// Screen point of the edge of a column row.
fn port_pos(tab: &DiagramTab, table: usize, column: usize, right: bool, origin: Pos2) -> Pos2 {
    let rect = tab.rect_of(table, origin);
    pos2(if right { rect.max.x } else { rect.min.x }, rect.min.y + (HEADER_H + ROW_H * column as f32 + ROW_H / 2.0) * tab.zoom)
}

/// Index of the visible line closest to `p`, when it is near enough.
fn edge_near(tab: &DiagramTab, p: Pos2, origin: Pos2) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, e) in tab.diagram.edges.iter().enumerate() {
        if e.kind == EdgeKind::Inferred && !tab.show_inferred {
            continue;
        }
        let (a, b, c1, c2) = edge_geometry(tab, origin, e);
        let points = egui::epaint::CubicBezierShape::from_points_stroke([a, c1, c2, b], false, Color32::TRANSPARENT, Stroke::NONE).flatten(Some(1.0));
        let d = points.windows(2).map(|w| distance_to_segment(p, w[0], w[1])).fold(f32::MAX, f32::min);
        if d <= EDGE_HIT_PX && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

/// Distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 == 0.0 {
        return (p - a).length();
    }
    let t = (((p - a).x * ab.x + (p - a).y * ab.y) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Draws the lines first, then the boxes on top.
fn paint_diagram(p: &egui::Painter, tab: &DiagramTab, origin: Pos2, hover: Option<Pos2>) {
    let z = tab.zoom;
    let show_text = z >= TEXT_ZOOM;
    let focus = tab.selected;
    for e in tab.diagram.edges.iter().filter(|e| tab.show_inferred || e.kind != EdgeKind::Inferred) {
        let involved = focus.is_some_and(|f| f == e.from_table || f == e.to_table);
        let base = match e.kind {
            EdgeKind::Real if tab.edge_is_new(e) => theme::OK,
            EdgeKind::Real => tab.color_of(e.from_table).unwrap_or(theme::LABEL),
            EdgeKind::Inferred => theme::LABEL,
            EdgeKind::Manual => theme::SECONDARY,
        };
        let color = if involved && e.kind == EdgeKind::Inferred { theme::TEXT } else if involved { base } else if focus.is_some() { base.gamma_multiply(0.25) } else { base.gamma_multiply(0.7) };
        let width = if involved { 2.0_f32 } else { 1.2_f32 };
        paint_edge(p, tab, origin, e, Stroke::new(width, color), tab.edge_is_new(e));
    }
    for (i, t) in tab.diagram.tables.iter().enumerate() {
        let rect = tab.rect_of(i, origin);
        if !rect.intersects(p.clip_rect()) {
            continue;
        }
        let color = tab.color_of(i);
        let (is_new, is_dropped, is_edited) = (tab.markers.created.contains(&t.name), tab.markers.dropped.contains(&t.name), tab.markers.edited.contains(&t.name));
        let selected = tab.selected == Some(i);
        let matched = tab.found.contains(&i);
        let hovered = hover.is_some_and(|h| rect.contains(h));
        p.rect_filled(rect, theme::RADIUS_CARD, theme::SURFACE);
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H * z));
        let header_fill = color.map_or(theme::FIELD_ROW_BG, |c| c.gamma_multiply(0.38));
        p.rect_filled(header, theme::RADIUS_CARD, header_fill);
        let border = if is_dropped {
            Stroke::new(2.0_f32, theme::ERROR)
        } else if is_new {
            Stroke::new(2.0_f32, theme::OK)
        } else if selected {
            Stroke::new(2.0_f32, theme::SECONDARY)
        } else if matched {
            Stroke::new(2.0_f32, theme::TERTIARY)
        } else if hovered {
            Stroke::new(1.5_f32, theme::LABEL)
        } else {
            Stroke::new(1.0_f32, color.map_or(theme::FIELD_ROW_BORDER, |c| c.gamma_multiply(0.7)))
        };
        p.rect_stroke(rect, theme::RADIUS_CARD, border, StrokeKind::Inside);
        if !show_text {
            continue;
        }
        p.text(header.left_center() + vec2(8.0 * z, 0.0), Align2::LEFT_CENTER, &t.name, FontId::new(13.0 * z, theme::semibold()), if is_dropped { theme::MUTED } else { theme::TEXT });
        let tag = if is_dropped { Some(("DROP", theme::ERROR)) } else if is_new { Some(("NEW", theme::OK)) } else if is_edited { Some(("EDITED", theme::TERTIARY)) } else { None };
        if let Some((text, c)) = tag {
            p.text(header.right_center() - vec2(8.0 * z, 0.0), Align2::RIGHT_CENTER, text, FontId::proportional(10.0 * z), c);
        }
        if is_dropped {
            p.line_segment([header.left_center() + vec2(6.0 * z, 0.0), header.left_center() + vec2(8.0 * z + t.name.chars().count() as f32 * 7.5 * z, 0.0)], Stroke::new(1.5_f32, theme::ERROR));
        }
        for (r, c) in t.columns.iter().enumerate() {
            let y = rect.min.y + (HEADER_H + ROW_H * r as f32 + ROW_H / 2.0) * z;
            let name_color = if c.is_pk { theme::TERTIARY } else if c.is_fk { theme::SECONDARY } else { theme::TEXT };
            let tag = if c.is_pk { "PK " } else if c.is_fk { "FK " } else { "" };
            p.text(pos2(rect.min.x + 8.0 * z, y), Align2::LEFT_CENTER, format!("{tag}{}", c.name), FontId::proportional(11.5 * z), name_color);
            p.text(pos2(rect.max.x - 8.0 * z, y), Align2::RIGHT_CENTER, &c.type_name, FontId::proportional(10.5 * z), theme::MUTED);
        }
    }
    paint_link_overlay(p, tab, origin, hover);
}

/// The handle under the pointer, and while a link is dragged the rubber band and the column it would land on.
fn paint_link_overlay(p: &egui::Painter, tab: &DiagramTab, origin: Pos2, hover: Option<Pos2>) {
    let z = tab.zoom.max(0.6);
    let Some(h) = hover else { return };
    match &tab.linking {
        None => {
            if let Some((t, c, right)) = port_at(tab, h, origin) {
                let at = port_pos(tab, t, c, right, origin);
                p.circle_filled(at, 5.0 * z, theme::PRIMARY);
                p.circle_stroke(at, 5.0 * z, Stroke::new(1.5_f32, theme::TEXT));
            }
        }
        Some(l) => {
            let from = port_pos(tab, l.table, l.column, l.right, origin);
            let target = column_at(tab, h, origin).filter(|&(t, c)| (t, c) != (l.table, l.column));
            let end = match target {
                Some((t, c)) => {
                    let rect = tab.rect_of(t, origin);
                    let row = Rect::from_min_size(pos2(rect.min.x, rect.min.y + (HEADER_H + ROW_H * c as f32) * tab.zoom), vec2(rect.width(), ROW_H * tab.zoom));
                    p.rect_filled(row, 0.0, theme::PRIMARY.gamma_multiply(0.35));
                    p.rect_stroke(row, 0.0, Stroke::new(1.5_f32, theme::SECONDARY), StrokeKind::Inside);
                    h
                }
                None => h,
            };
            p.circle_filled(from, 4.0 * z, theme::SECONDARY);
            p.extend(egui::Shape::dashed_line(&[from, end], Stroke::new(2.0_f32, theme::SECONDARY), 6.0, 4.0));
        }
    }
}

/// End points and control points of the line of one edge.
fn edge_geometry(tab: &DiagramTab, origin: Pos2, e: &DiagramEdge) -> (Pos2, Pos2, Pos2, Pos2) {
    edge_curve(tab.rect_of(e.from_table, origin), tab.rect_of(e.to_table, origin), e, tab.zoom)
}

/// End points and control points of the line of one edge, given the rectangles of its two tables and the zoom.
fn edge_curve(from: Rect, to: Rect, e: &DiagramEdge, z: f32) -> (Pos2, Pos2, Pos2, Pos2) {
    let row_y = |r: Rect, col: Option<usize>| r.min.y + col.map_or(HEADER_H / 2.0, |c| HEADER_H + ROW_H * c as f32 + ROW_H / 2.0) * z;
    let (y1, y2) = (row_y(from, Some(e.from_column)), row_y(to, e.to_column));
    let reach = 40.0 * z;
    let (a, b, c1, c2) = if e.from_table == e.to_table {
        let a = pos2(from.max.x, y1);
        let b = pos2(from.max.x, y2);
        (a, b, a + vec2(reach * 1.5, 0.0), b + vec2(reach * 1.5, 0.0))
    } else if from.max.x < to.min.x {
        let (a, b) = (pos2(from.max.x, y1), pos2(to.min.x, y2));
        (a, b, a + vec2(reach, 0.0), b - vec2(reach, 0.0))
    } else if to.max.x < from.min.x {
        let (a, b) = (pos2(from.min.x, y1), pos2(to.max.x, y2));
        (a, b, a - vec2(reach, 0.0), b + vec2(reach, 0.0))
    } else {
        // the boxes overlap horizontally: leave and enter on the right side
        let (a, b) = (pos2(from.max.x, y1), pos2(to.max.x, y2));
        (a, b, a + vec2(reach, 0.0), b + vec2(reach, 0.0))
    };
    (a, b, c1, c2)
}

/// Draws the line of one edge between the two column rows: solid for a foreign key, dashed for the others.
fn paint_edge(p: &egui::Painter, tab: &DiagramTab, origin: Pos2, e: &DiagramEdge, stroke: Stroke, pending: bool) {
    let z = tab.zoom;
    let (a, b, c1, c2) = edge_geometry(tab, origin, e);
    let curve = egui::epaint::CubicBezierShape::from_points_stroke([a, c1, c2, b], false, Color32::TRANSPARENT, stroke);
    if e.kind != EdgeKind::Real || pending {
        let dash = 6.0 * z.max(0.6);
        p.extend(egui::Shape::dashed_line(&curve.flatten(Some(0.5)), stroke, dash, dash * 0.7));
    } else {
        p.add(curve);
    }
    p.circle_filled(a, 2.5 * z.max(0.6), stroke.color);
    // the referenced end gets a small arrow head
    let dir = if (b.x - c2.x).abs() < f32::EPSILON { vec2(1.0, 0.0) } else { (b - c2).normalized() };
    let side = vec2(-dir.y, dir.x);
    let s = 6.0 * z.max(0.6);
    p.add(egui::epaint::PathShape::convex_polygon(vec![b, b - dir * s + side * s * 0.5, b - dir * s - side * s * 0.5], stroke.color, Stroke::NONE));
}

/// Asks where to save, writes the diagram as SVG and reports the outcome.
fn export_svg(tab: &DiagramTab) -> DiagramAction {
    let Some(path) = rfd::FileDialog::new().set_file_name("diagram.svg").add_filter("SVG image", &["svg"]).save_file() else {
        return DiagramAction::Notice(false, "Export cancelled".into());
    };
    match std::fs::write(&path, tab.to_svg()) {
        Ok(()) => DiagramAction::Notice(false, format!("Diagram saved to {}", path.display())),
        Err(e) => DiagramAction::Notice(true, format!("cannot save the diagram: {e}")),
    }
}

/// `#rrggbb` form of a colour (the alpha is returned apart, as 0 to 1).
fn hex(c: Color32) -> (String, f32) {
    let c = c.to_srgba_unmultiplied();
    (format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]), f32::from(c[3]) / 255.0)
}

/// Escapes the characters that mean something in XML text.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl DiagramTab {
    /// The diagram as an SVG document: the boxes and the visible lines at zoom 1.
    pub fn to_svg(&self) -> String {
        use std::fmt::Write;
        let rects: Vec<Rect> = (0..self.positions.len()).map(|i| Rect::from_min_size(pos2(self.positions[i].0, self.positions[i].1), vec2(self.sizes[i].0, self.sizes[i].1))).collect();
        let bounds = rects.iter().fold(Rect::NOTHING, |acc, r| acc.union(*r)).expand(MARGIN);
        let bounds = if rects.is_empty() { Rect::from_min_size(Pos2::ZERO, vec2(200.0, 100.0)) } else { bounds };
        let mut out = String::new();
        let _ = writeln!(out, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{x} {y} {w} {h}" font-family="sans-serif">"#, x = bounds.min.x, y = bounds.min.y, w = bounds.width(), h = bounds.height());
        let (bg, _) = hex(theme::DEEP_DARK);
        let _ = writeln!(out, r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{bg}"/>"#, bounds.min.x, bounds.min.y, bounds.width(), bounds.height());
        for e in self.diagram.edges.iter().filter(|e| self.show_inferred || e.kind != EdgeKind::Inferred) {
            let base = match e.kind {
                EdgeKind::Real => self.color_of(e.from_table).unwrap_or(theme::LABEL),
                EdgeKind::Inferred => theme::LABEL,
                EdgeKind::Manual => theme::SECONDARY,
            };
            let (a, b, c1, c2) = edge_curve(rects[e.from_table], rects[e.to_table], e, 1.0);
            let (color, _) = hex(base);
            let dash = if e.kind == EdgeKind::Real { "" } else { r#" stroke-dasharray="6 4""# };
            let _ = writeln!(out, r#"<path d="M {} {} C {} {}, {} {}, {} {}" fill="none" stroke="{color}" stroke-width="1.5"{dash}/>"#, a.x, a.y, c1.x, c1.y, c2.x, c2.y, b.x, b.y);
            let _ = writeln!(out, r#"<circle cx="{}" cy="{}" r="2.5" fill="{color}"/>"#, a.x, a.y);
        }
        for (i, t) in self.diagram.tables.iter().enumerate() {
            let r = rects[i];
            let color = self.color_of(i);
            let (fill, _) = hex(theme::SURFACE);
            let (head, _) = hex(color.map_or(theme::FIELD_ROW_BG, |c| c.gamma_multiply(0.38)));
            let (border, _) = hex(color.map_or(theme::FIELD_ROW_BORDER, |c| c.gamma_multiply(0.7)));
            let (text, _) = hex(theme::TEXT);
            let _ = writeln!(out, r#"<rect x="{}" y="{}" width="{}" height="{}" rx="6" fill="{fill}" stroke="{border}"/>"#, r.min.x, r.min.y, r.width(), r.height());
            let _ = writeln!(out, r#"<rect x="{}" y="{}" width="{}" height="{HEADER_H}" rx="6" fill="{head}"/>"#, r.min.x, r.min.y, r.width());
            let _ = writeln!(out, r#"<text x="{}" y="{}" font-size="13" font-weight="600" fill="{text}" dominant-baseline="middle">{}</text>"#, r.min.x + 8.0, r.min.y + HEADER_H / 2.0, xml_escape(&t.name));
            for (row, c) in t.columns.iter().enumerate() {
                let y = r.min.y + HEADER_H + ROW_H * row as f32 + ROW_H / 2.0;
                let (name_color, _) = hex(if c.is_pk { theme::TERTIARY } else if c.is_fk { theme::SECONDARY } else { theme::TEXT });
                let tag = if c.is_pk { "PK " } else if c.is_fk { "FK " } else { "" };
                let (muted, _) = hex(theme::MUTED);
                let _ = writeln!(out, r#"<text x="{}" y="{y}" font-size="11.5" fill="{name_color}" dominant-baseline="middle">{}</text>"#, r.min.x + 8.0, xml_escape(&format!("{tag}{}", c.name)));
                let _ = writeln!(out, r#"<text x="{}" y="{y}" font-size="10.5" fill="{muted}" text-anchor="end" dominant-baseline="middle">{}</text>"#, r.max.x - 8.0, xml_escape(&c.type_name));
            }
        }
        out.push_str("</svg>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::model::{ColumnMeta, TableRef};

    fn column(name: &str, pk: bool) -> ColumnMeta {
        ColumnMeta { name: name.into(), type_name: "int".into(), nullable: false, default: None, is_pk: pk, fk: None, ordinal: 0, extra: Default::default() }
    }

    fn tab(names: &[&str]) -> DiagramTab {
        let tables = names.iter().map(|n| TableStructure { table: TableRef { name: (*n).into(), ..Default::default() }, columns: vec![column("id", true), column("name", false)], ..Default::default() }).collect();
        let mut t = DiagramTab::new();
        t.set_tables(tables, Vec::new(), &Positions::new(), Vec::new());
        t
    }

    #[test]
    fn search_steps_forward_and_back_and_wraps() {
        let mut t = tab(&["alpha", "beta", "gamma"]);
        t.search = "a".into();
        t.refresh_search();
        assert_eq!(t.found.len(), 3);
        let first = t.step_found(true).unwrap();
        let second = t.step_found(true).unwrap();
        assert_ne!(first, second);
        assert_eq!(t.step_found(false), Some(first), "Up goes back");
        assert_eq!(t.step_found(false), t.found.last().copied(), "and wraps to the last match");
        t.search = "zzz".into();
        t.refresh_search();
        assert_eq!(t.step_found(true), None);
    }

    #[test]
    fn svg_has_one_box_per_table_and_escapes_names() {
        let t = tab(&["a<b", "orders"]);
        let svg = t.to_svg();
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("a&lt;b"));
        assert!(!svg.contains("a<b"));
        assert_eq!(svg.matches(r#"rx="6" fill=""#).count(), 4, "a box and a header for each table");
    }

    #[test]
    fn svg_of_an_empty_diagram_is_valid() {
        assert!(DiagramTab::new().to_svg().contains("</svg>"));
    }
}
