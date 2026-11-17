//! ER diagram tab (SPEC 11.3): tables as boxes, one line per foreign key, read only.

use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::diagram::{Diagram, DiagramSort};
use dbzilla_core::model::TableStructure;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

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

/// Requests of the diagram toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagramAction {
    /// Reload the schema and lay the tables out again.
    Rearrange,
    /// Open the data of a table.
    OpenTable(String),
    /// Go back to the workspace (close the tab).
    Close,
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
    dragging: Option<usize>,
    search: String,
    found: Vec<usize>,
    found_at: usize,
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
            dragging: None,
            search: String::new(),
            found: Vec::new(),
            found_at: 0,
        }
    }

    /// Takes the tables read from the database and lays them out.
    pub fn set_tables(&mut self, tables: Vec<TableStructure>, skipped: Vec<String>) {
        self.structures = tables;
        self.skipped = skipped;
        self.loading = false;
        self.rebuild();
    }

    /// Marks the tab as reloading.
    pub fn start_loading(&mut self) {
        self.loading = true;
    }

    /// Builds the diagram from the loaded tables with the current sort, and lays it out.
    fn rebuild(&mut self) {
        self.diagram = Diagram::build(&self.structures, self.sort);
        self.groups = self.diagram.groups();
        self.sizes = self.diagram.tables.iter().map(box_size).collect();
        self.positions = self.diagram.layout(&self.sizes);
        self.selected = None;
        self.dragging = None;
        self.fit_pending = true;
        self.refresh_search();
    }

    /// Recomputes the search matches after the text or the tables changed.
    fn refresh_search(&mut self) {
        self.found = self.diagram.search(&self.search);
        self.found_at = 0;
    }

    /// Number of tables in the diagram.
    #[cfg(test)]
    pub fn table_count(&self) -> usize {
        self.diagram.tables.len()
    }

    /// Number of foreign key lines in the diagram.
    #[cfg(test)]
    pub fn edge_count(&self) -> usize {
        self.diagram.edges.len()
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
pub fn show(ui: &mut Ui, tab: &mut DiagramTab) -> Vec<DiagramAction> {
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
                tab.rebuild();
            }
            if widgets::secondary_button(ui, "Rearrange").on_hover_text("Reload the schema and lay the tables out again").clicked() {
                actions.push(DiagramAction::Rearrange);
            }
            ui.separator();
            let r = widgets::search_box(ui, &mut tab.search, "Find table or column");
            if r.changed() {
                tab.refresh_search();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !tab.found.is_empty() {
                canvas_jump = Some(tab.found[tab.found_at % tab.found.len()]);
                tab.found_at = (tab.found_at + 1) % tab.found.len();
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
    if let Some(i) = canvas_jump {
        tab.jump_to(i, canvas);
    }
    handle_input(ui, tab, &resp, origin, &mut actions);
    paint_diagram(&painter.with_clip_rect(canvas), tab, origin, resp.hover_pos());
    actions
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
        tab.dragging = resp.interact_pointer_pos().and_then(|p| hit(p, tab));
        if tab.dragging.is_some() {
            tab.selected = tab.dragging;
        }
    }
    if resp.dragged() {
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
        tab.dragging = None;
    }
    if resp.clicked() {
        tab.selected = resp.interact_pointer_pos().and_then(|p| hit(p, tab));
    }
    if resp.double_clicked() {
        if let Some(i) = resp.interact_pointer_pos().and_then(|p| hit(p, tab)) {
            actions.push(DiagramAction::OpenTable(tab.diagram.tables[i].name.clone()));
        }
    }
    if resp.hovered() {
        let over = resp.hover_pos().and_then(|p| hit(p, tab)).is_some();
        ui.ctx().set_cursor_icon(if tab.dragging.is_some() { CursorIcon::Grabbing } else if over { CursorIcon::Grab } else { CursorIcon::Default });
    }
}

/// Draws the lines first, then the boxes on top.
fn paint_diagram(p: &egui::Painter, tab: &DiagramTab, origin: Pos2, hover: Option<Pos2>) {
    let z = tab.zoom;
    let show_text = z >= TEXT_ZOOM;
    let focus = tab.selected;
    for e in &tab.diagram.edges {
        let involved = focus.is_some_and(|f| f == e.from_table || f == e.to_table);
        let base = group_color(tab.groups[e.from_table]).unwrap_or(theme::LABEL);
        let color = if involved { base } else if focus.is_some() { base.gamma_multiply(0.25) } else { base.gamma_multiply(0.7) };
        let width = if involved { 2.0_f32 } else { 1.2_f32 };
        paint_edge(p, tab, origin, e, Stroke::new(width, color));
    }
    for (i, t) in tab.diagram.tables.iter().enumerate() {
        let rect = tab.rect_of(i, origin);
        if !rect.intersects(p.clip_rect()) {
            continue;
        }
        let color = group_color(tab.groups[i]);
        let selected = tab.selected == Some(i);
        let matched = tab.found.contains(&i);
        let hovered = hover.is_some_and(|h| rect.contains(h));
        p.rect_filled(rect, theme::RADIUS_CARD, theme::SURFACE);
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H * z));
        let header_fill = color.map_or(theme::FIELD_ROW_BG, |c| c.gamma_multiply(0.38));
        p.rect_filled(header, theme::RADIUS_CARD, header_fill);
        let border = if selected {
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
        p.text(header.left_center() + vec2(8.0 * z, 0.0), Align2::LEFT_CENTER, &t.name, FontId::new(13.0 * z, theme::semibold()), theme::TEXT);
        for (r, c) in t.columns.iter().enumerate() {
            let y = rect.min.y + (HEADER_H + ROW_H * r as f32 + ROW_H / 2.0) * z;
            let name_color = if c.is_pk { theme::TERTIARY } else if c.is_fk { theme::SECONDARY } else { theme::TEXT };
            let tag = if c.is_pk { "PK " } else if c.is_fk { "FK " } else { "" };
            p.text(pos2(rect.min.x + 8.0 * z, y), Align2::LEFT_CENTER, format!("{tag}{}", c.name), FontId::proportional(11.5 * z), name_color);
            p.text(pos2(rect.max.x - 8.0 * z, y), Align2::RIGHT_CENTER, &c.type_name, FontId::proportional(10.5 * z), theme::MUTED);
        }
    }
}

/// Draws the line of one foreign key between the two column rows.
fn paint_edge(p: &egui::Painter, tab: &DiagramTab, origin: Pos2, e: &dbzilla_core::diagram::DiagramEdge, stroke: Stroke) {
    let z = tab.zoom;
    let (from, to) = (tab.rect_of(e.from_table, origin), tab.rect_of(e.to_table, origin));
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
    p.add(egui::epaint::CubicBezierShape::from_points_stroke([a, c1, c2, b], false, Color32::TRANSPARENT, stroke));
    p.circle_filled(a, 2.5 * z.max(0.6), stroke.color);
    // the referenced end gets a small arrow head
    let dir = if (b.x - c2.x).abs() < f32::EPSILON { vec2(1.0, 0.0) } else { (b - c2).normalized() };
    let side = vec2(-dir.y, dir.x);
    let s = 6.0 * z.max(0.6);
    p.add(egui::epaint::PathShape::convex_polygon(vec![b, b - dir * s + side * s * 0.5, b - dir * s - side * s * 0.5], stroke.color, Stroke::NONE));
}
