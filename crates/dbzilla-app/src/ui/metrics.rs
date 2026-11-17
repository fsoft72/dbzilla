//! Metrics board tab (SPEC 11.2): a canvas of objects whose data comes from SQL queries.

use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::metrics::{self, Board, MetricObject, ObjectKind, Refresh};
use dbzilla_core::model::{ColumnMeta, Row};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Layout, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2};
use std::collections::HashMap;
use std::time::Instant;

/// Height of the title bar of an object.
const TITLE_H: f32 = 26.0;
const MIN_W: f32 = 140.0;
const MIN_H: f32 = 70.0;
/// Rows kept in memory and drawn for one object.
const MAX_ROWS: usize = 500;
/// Distance between the lines of the canvas grid, and the snap of a moved object.
const GRID: f32 = 24.0;
/// Where the next added object goes, in canvas coordinates, and the shift between two of them.
const ADD_AT: f32 = 40.0;
const ADD_STEP: f32 = 24.0;

/// Requests of the metrics tab toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricsAction {
    /// Back to the workspace (close the tab).
    Close,
    /// Run the query of an object (variables already replaced).
    Run { object: u64, sql: String },
    /// The boards changed: keep them.
    Save(Vec<Board>),
    /// Something to tell the user; `true` for an error.
    Notice(bool, String),
}

/// The last result of an object.
#[derive(Debug, Clone, Default)]
pub struct ObjData {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Row>,
    pub error: Option<String>,
    pub loading: bool,
    /// Rows were dropped because there are more than `MAX_ROWS`.
    pub truncated: bool,
    last_ms: Option<u64>,
}

/// What the user did to an object in one frame.
#[derive(Default)]
struct Outcome {
    moved: Vec2,
    resized: Vec2,
    refresh: bool,
    edit: bool,
    delete: bool,
    raise: bool,
    emit: bool,
}

/// State of a metrics tab.
pub struct MetricsTab {
    boards: Vec<Board>,
    current: usize,
    data: HashMap<u64, ObjData>,
    /// Value of each variable as last submitted, to tell when it changed.
    committed: HashMap<u64, String>,
    /// Automatic refresh is on.
    running: bool,
    locked: bool,
    grid: bool,
    pan: Vec2,
    selected: Option<u64>,
    editing: Option<u64>,
    rename: Option<String>,
    started: Instant,
    dirty: bool,
    queued: Vec<u64>,
}

impl MetricsTab {
    /// A tab showing `boards`; an empty list gets a first board.
    pub fn new(mut boards: Vec<Board>) -> Self {
        if boards.is_empty() {
            boards.push(Board::new(new_board_id(), "Metrics"));
        }
        Self { boards, current: 0, data: HashMap::new(), committed: HashMap::new(), running: true, locked: false, grid: false, pan: vec2(ADD_AT, ADD_AT), selected: None, editing: None, rename: None, started: Instant::now(), dirty: false, queued: Vec::new() }
    }

    /// Milliseconds since the tab opened.
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// The board on screen.
    fn board(&self) -> &Board {
        &self.boards[self.current]
    }

    /// Mutable access to the board on screen.
    fn board_mut(&mut self) -> &mut Board {
        &mut self.boards[self.current]
    }

    /// Switches board, forgetting the data of the old one.
    fn select_board(&mut self, i: usize) {
        if i >= self.boards.len() || i == self.current {
            return;
        }
        self.current = i;
        self.data.clear();
        self.committed.clear();
        self.selected = None;
        self.editing = None;
    }

    /// Rows of a running query arrived. Results of objects that are not waiting are ignored.
    pub fn on_rows(&mut self, object: u64, columns: Option<Vec<ColumnMeta>>, rows: Vec<Row>) {
        let Some(d) = self.data.get_mut(&object).filter(|d| d.loading) else { return };
        if let Some(c) = columns {
            d.columns = c;
        }
        let room = MAX_ROWS.saturating_sub(d.rows.len());
        d.truncated |= rows.len() > room;
        d.rows.extend(rows.into_iter().take(room));
    }

    /// The query of an object failed.
    pub fn on_error(&mut self, object: u64, error: String) {
        if let Some(d) = self.data.get_mut(&object).filter(|d| d.loading) {
            d.error = Some(error);
        }
    }

    /// The query of an object ended.
    pub fn on_finished(&mut self, object: u64) {
        if let Some(d) = self.data.get_mut(&object) {
            d.loading = false;
        }
    }

    /// Asks for the data of `object` on the next frame.
    fn queue(&mut self, object: u64) {
        if !self.queued.contains(&object) {
            self.queued.push(object);
        }
    }

    /// Runs what an object emitted: the objects connected to it, and those reading its variable.
    fn emit(&mut self, source: u64) {
        for id in self.board().affected_by(source) {
            self.queue(id);
        }
    }

    /// The last result of `object`.
    #[cfg(test)]
    pub fn data(&self, object: u64) -> Option<&ObjData> {
        self.data.get(&object)
    }

    /// Adds an object with `sql` to the board on screen.
    #[cfg(test)]
    pub fn add_with_sql(&mut self, kind: ObjectKind, sql: &str) -> u64 {
        let id = self.board_mut().add(kind, 0.0, 0.0);
        if let Some(o) = self.board_mut().get_mut(id) {
            o.sql = sql.into();
        }
        id
    }

    /// Turns the queued and due refreshes into Run requests.
    #[cfg(test)]
    pub fn dispatch_for_test(&mut self, actions: &mut Vec<MetricsAction>) {
        self.dispatch(actions);
    }

    /// Turns the queued and due refreshes into Run requests.
    fn dispatch(&mut self, actions: &mut Vec<MetricsAction>) {
        let now = self.now_ms();
        if self.running {
            let due: Vec<u64> = self
                .board()
                .objects
                .iter()
                .filter(|o| loads_data(o))
                .filter(|o| {
                    let last = self.data.get(&o.id).and_then(|d| d.last_ms);
                    o.refresh.is_due(now, last) || (o.kind == ObjectKind::Dropdown && last.is_none())
                })
                .map(|o| o.id)
                .collect();
            for id in due {
                self.queue(id);
            }
        }
        let vars = self.board().variables();
        for id in std::mem::take(&mut self.queued) {
            let Some(o) = self.board().get(id).filter(|o| loads_data(o)) else { continue };
            if self.data.get(&id).is_some_and(|d| d.loading) {
                continue;
            }
            let sql = metrics::substitute(o.sql.trim(), &vars);
            let d = self.data.entry(id).or_default();
            d.last_ms = Some(now);
            if sql.is_empty() {
                continue;
            }
            *d = ObjData { loading: true, last_ms: Some(now), ..ObjData::default() };
            actions.push(MetricsAction::Run { object: id, sql });
        }
    }

    /// Adds an object of `kind` where it is visible.
    fn add(&mut self, kind: ObjectKind) {
        let n = self.board().objects.len() as f32;
        let (x, y) = (ADD_AT + ADD_STEP * (n % 8.0) - self.pan.x + ADD_AT, ADD_AT + ADD_STEP * (n % 8.0) - self.pan.y + ADD_AT);
        let id = self.board_mut().add(kind, x, y);
        self.selected = Some(id);
        self.editing = Some(id);
        self.dirty = true;
    }
}

/// True for the objects that run a query: the data objects and a dropdown loaded from SQL.
fn loads_data(o: &MetricObject) -> bool {
    o.kind.runs_sql() || (o.kind == ObjectKind::Dropdown && !o.sql.trim().is_empty())
}

/// A board id that no other board has.
fn new_board_id() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("board-{ms}")
}

/// Colour of the n-th series or slice.
fn palette(n: usize) -> Color32 {
    theme::DIAGRAM_GROUPS[n % theme::DIAGRAM_GROUPS.len()]
}

/// Text of a refresh rule for the combo box.
fn refresh_label(r: Refresh) -> &'static str {
    match r {
        Refresh::Every(_) => "Every...",
        Refresh::OnEvent => "On event",
        Refresh::Manual => "Do not auto refresh",
    }
}

/// Draws the tab and returns what it asks of the workspace.
pub fn show(ui: &mut Ui, tab: &mut MetricsTab) -> Vec<MetricsAction> {
    let mut actions = Vec::new();
    toolbar(ui, tab, &mut actions);
    canvas(ui, tab);
    settings_window(ui.ctx(), tab);
    rename_window(ui.ctx(), tab);
    tab.dispatch(&mut actions);
    if tab.dirty && !ui.input(|i| i.pointer.any_down()) {
        tab.dirty = false;
        actions.push(MetricsAction::Save(tab.boards.clone()));
    }
    if tab.running || !tab.queued.is_empty() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
    actions
}

/// The bar with the board choice and the commands.
fn toolbar(ui: &mut Ui, tab: &mut MetricsTab, actions: &mut Vec<MetricsAction>) {
    egui::TopBottomPanel::top(ui.id().with("metrics_bar")).exact_height(36.0).frame(egui::Frame::new().fill(theme::NAVY_DARK).inner_margin(egui::Margin::symmetric(10, 4))).show_inside(ui, |ui| {
        ui.horizontal_centered(|ui| {
            if widgets::secondary_button(ui, "Database").on_hover_text("Back to the workspace").clicked() {
                actions.push(MetricsAction::Close);
            }
            ui.separator();
            let mut pick = None;
            egui::ComboBox::from_id_salt("metrics_board").selected_text(tab.board().name.clone()).show_ui(ui, |ui| {
                for (i, b) in tab.boards.iter().enumerate() {
                    if ui.selectable_label(i == tab.current, &b.name).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if let Some(i) = pick {
                tab.select_board(i);
            }
            ui.menu_button("Board", |ui| board_menu(ui, tab, actions));
            ui.separator();
            let status = if tab.running { "Stop" } else { "Start" };
            if widgets::secondary_button(ui, status).on_hover_text("Start or stop the automatic refresh").clicked() {
                tab.running = !tab.running;
            }
            ui.menu_button("Add", |ui| {
                for kind in ObjectKind::ALL {
                    if ui.button(kind.label()).clicked() {
                        tab.add(kind);
                        ui.close_menu();
                    }
                }
            });
            ui.checkbox(&mut tab.locked, "Lock").on_hover_text("Stop objects from being moved, resized or deleted");
            ui.checkbox(&mut tab.grid, "Grid").on_hover_text("Show the grid and the connections between objects");
            if tab.running {
                ui.label(egui::RichText::new("Live").color(theme::OK));
            }
        });
    });
}

/// Items of the Board menu: new, rename, duplicate, delete, import and export.
fn board_menu(ui: &mut Ui, tab: &mut MetricsTab, actions: &mut Vec<MetricsAction>) {
    if ui.button("New board").clicked() {
        tab.boards.push(Board::new(new_board_id(), format!("Metrics {}", tab.boards.len() + 1)));
        let last = tab.boards.len() - 1;
        tab.select_board(last);
        tab.dirty = true;
        ui.close_menu();
    }
    if ui.button("Rename...").clicked() {
        tab.rename = Some(tab.board().name.clone());
        ui.close_menu();
    }
    if ui.button("Duplicate").clicked() {
        let mut copy = tab.board().clone();
        copy.id = new_board_id();
        copy.name = format!("{} copy", copy.name);
        tab.boards.push(copy);
        let last = tab.boards.len() - 1;
        tab.select_board(last);
        tab.dirty = true;
        ui.close_menu();
    }
    if ui.add_enabled(tab.boards.len() > 1, egui::Button::new("Delete")).clicked() {
        tab.boards.remove(tab.current);
        tab.current = 0;
        tab.data.clear();
        tab.committed.clear();
        tab.dirty = true;
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Import board...").clicked() {
        ui.close_menu();
        import_board(tab, actions);
    }
    if ui.button("Export board...").clicked() {
        ui.close_menu();
        export_board(tab, actions);
    }
}

/// Asks for a JSON file and adds the board it holds.
fn import_board(tab: &mut MetricsTab, actions: &mut Vec<MetricsAction>) {
    let Some(path) = rfd::FileDialog::new().add_filter("Metrics board", &["json"]).pick_file() else { return };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return actions.push(MetricsAction::Notice(true, format!("cannot read {}: {e}", path.display()))),
    };
    match Board::from_json(&text) {
        Ok(mut b) => {
            b.id = new_board_id();
            tab.boards.push(b);
            let last = tab.boards.len() - 1;
            tab.select_board(last);
            tab.dirty = true;
            actions.push(MetricsAction::Notice(false, "Board imported".into()));
        }
        Err(e) => actions.push(MetricsAction::Notice(true, e.to_string())),
    }
}

/// Asks for a file name and writes the board as JSON.
fn export_board(tab: &MetricsTab, actions: &mut Vec<MetricsAction>) {
    let name = format!("{}.json", tab.board().name.replace(['/', '\\'], "-"));
    let Some(path) = rfd::FileDialog::new().set_file_name(name).add_filter("Metrics board", &["json"]).save_file() else { return };
    match std::fs::write(&path, tab.board().to_json() + "\n") {
        Ok(()) => actions.push(MetricsAction::Notice(false, format!("Board saved to {}", path.display()))),
        Err(e) => actions.push(MetricsAction::Notice(true, format!("cannot write {}: {e}", path.display()))),
    }
}

/// Small window that renames the board.
fn rename_window(ctx: &egui::Context, tab: &mut MetricsTab) {
    let Some(mut text) = tab.rename.take() else { return };
    let mut keep = true;
    let mut apply = false;
    egui::Window::new("Rename board").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        let r = widgets::input(ui, &mut text, "Name");
        r.request_focus();
        ui.horizontal(|ui| {
            apply = widgets::primary_button(ui, "Rename").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if widgets::secondary_button(ui, "Cancel").clicked() {
                keep = false;
            }
        });
    });
    if apply && !text.trim().is_empty() {
        tab.board_mut().name = text.trim().to_owned();
        tab.dirty = true;
    } else if keep && !apply {
        tab.rename = Some(text);
    }
}

/// The canvas with every object of the board.
fn canvas(ui: &mut Ui, tab: &mut MetricsTab) {
    let avail = ui.available_size();
    let (bg, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
    let area = bg.rect;
    painter.rect_filled(area, 0.0, theme::DEEP_DARK);
    if bg.dragged() {
        tab.pan += bg.drag_delta();
    }
    if bg.clicked() {
        tab.selected = None;
    }
    if tab.grid {
        draw_grid(&painter, area, tab.pan);
        draw_connections(&painter, tab, area);
    }
    if tab.board().objects.is_empty() {
        painter.text(area.center(), Align2::CENTER_CENTER, "Empty board. Use Add to put an object on it.", FontId::proportional(14.0), theme::MUTED);
    }
    let mut raise = None;
    let mut remove = None;
    let mut emitted = Vec::new();
    let (cur, locked, grid, pan) = (tab.current, tab.locked, tab.grid, tab.pan);
    for i in 0..tab.boards[cur].objects.len() {
        let id = tab.boards[cur].objects[i].id;
        let obj = &mut tab.boards[cur].objects[i];
        let rect = Rect::from_min_size(area.min + pan + vec2(obj.x, obj.y), vec2(obj.w, obj.h));
        if !rect.intersects(area) {
            continue;
        }
        let committed = tab.committed.entry(id).or_insert_with(|| obj.value.clone());
        let selected = tab.selected == Some(id);
        let out = object_ui(ui, area, rect, obj, tab.data.get(&id), committed, selected, locked);
        if out.moved != Vec2::ZERO || out.resized != Vec2::ZERO {
            obj.x += out.moved.x;
            obj.y += out.moved.y;
            obj.w = (obj.w + out.resized.x).max(MIN_W);
            obj.h = (obj.h + out.resized.y).max(MIN_H);
            tab.dirty = true;
        }
        if grid && out.moved == Vec2::ZERO && !ui.input(|i| i.pointer.any_down()) {
            obj.x = (obj.x / GRID).round() * GRID;
            obj.y = (obj.y / GRID).round() * GRID;
        }
        if out.raise {
            raise = Some(id);
            tab.selected = Some(id);
        }
        if out.edit {
            tab.editing = Some(id);
        }
        if out.delete {
            remove = Some(id);
        }
        if out.refresh {
            emitted.push((id, false));
        }
        if out.emit {
            emitted.push((id, true));
        }
    }
    for (id, event) in emitted {
        if event {
            tab.emit(id);
        } else {
            tab.queue(id);
        }
    }
    if let Some(id) = remove {
        tab.board_mut().remove(id);
        tab.data.remove(&id);
        tab.committed.remove(&id);
        tab.dirty = true;
        if tab.editing == Some(id) {
            tab.editing = None;
        }
    }
    if let Some(id) = raise {
        let objs = &mut tab.board_mut().objects;
        if let Some(pos) = objs.iter().position(|o| o.id == id) {
            let o = objs.remove(pos);
            objs.push(o);
        }
    }
}

/// Dots of the canvas grid.
fn draw_grid(painter: &egui::Painter, area: Rect, pan: Vec2) {
    let start = pos2(area.min.x + pan.x.rem_euclid(GRID), area.min.y + pan.y.rem_euclid(GRID));
    let mut x = start.x;
    while x < area.max.x {
        let mut y = start.y;
        while y < area.max.y {
            painter.circle_filled(pos2(x, y), 1.0, theme::FIELD_ROW_BORDER);
            y += GRID;
        }
        x += GRID;
    }
}

/// Curves from an object to the objects it refreshes.
fn draw_connections(painter: &egui::Painter, tab: &MetricsTab, area: Rect) {
    let board = tab.board();
    let origin = area.min + tab.pan;
    for src in &board.objects {
        for dst in src.targets.iter().filter_map(|t| board.get(*t)) {
            let a = origin + vec2(src.x + src.w, src.y + src.h / 2.0);
            let b = origin + vec2(dst.x, dst.y + dst.h / 2.0);
            let dx = ((b.x - a.x).abs() / 2.0).max(30.0);
            let shape = egui::epaint::CubicBezierShape::from_points_stroke([a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b], false, Color32::TRANSPARENT, Stroke::new(1.5_f32, theme::SECONDARY));
            painter.add(shape);
            painter.circle_filled(a, 3.0, theme::SECONDARY);
            painter.circle_filled(b, 3.0, theme::SECONDARY);
        }
    }
}

/// Draws one object and reports what the user did to it.
#[allow(clippy::too_many_arguments)]
fn object_ui(ui: &mut Ui, area: Rect, rect: Rect, obj: &mut MetricObject, data: Option<&ObjData>, committed: &mut String, selected: bool, locked: bool) -> Outcome {
    let mut out = Outcome::default();
    let clip = ui.clip_rect().intersect(area);
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(rect, theme::RADIUS_CARD, theme::SURFACE);
    let border = if selected { theme::PRIMARY } else { theme::FIELD_ROW_BORDER };
    painter.rect_stroke(rect, theme::RADIUS_CARD, Stroke::new(1.0_f32, border), StrokeKind::Inside);
    let title = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_H));
    let drag = ui.interact(title, ui.id().with(("metric-title", obj.id)), Sense::click_and_drag());
    if drag.clicked() || drag.drag_started() {
        out.raise = true;
    }
    if drag.dragged() && !locked {
        out.moved = drag.drag_delta();
    }
    if drag.hovered() && !locked {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    painter.text(title.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, &obj.label, FontId::proportional(13.0), theme::TEXT);
    let mut bar = ui.new_child(UiBuilder::new().max_rect(title.shrink2(vec2(4.0, 0.0))).layout(Layout::right_to_left(Align::Center)));
    bar.set_clip_rect(clip);
    if !locked && widgets::ico_button(&mut bar, Ico::Trash, "Delete the object").clicked() {
        out.delete = true;
    }
    if widgets::ico_button(&mut bar, Ico::Settings, "Object settings").clicked() {
        out.edit = true;
    }
    if loads_data(obj) && widgets::ico_button(&mut bar, Ico::Refresh, "Refresh now").clicked() {
        out.refresh = true;
    }
    if data.is_some_and(|d| d.loading) {
        bar.spinner();
    }
    let body = Rect::from_min_max(rect.min + vec2(8.0, TITLE_H + 2.0), rect.max - vec2(8.0, 8.0));
    let mut inner = ui.new_child(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)));
    inner.set_clip_rect(body.intersect(clip));
    match obj.kind {
        ObjectKind::Input => {
            let hint = format!("${}", metrics::variable_name(obj));
            let r = inner.add(egui::TextEdit::singleline(&mut obj.value).hint_text(hint).desired_width(body.width()));
            if r.lost_focus() && obj.value != *committed {
                committed.clone_from(&obj.value);
                out.emit = true;
            }
        }
        ObjectKind::Dropdown => {
            let choices: Vec<String> = if obj.sql.trim().is_empty() { obj.choices.clone() } else { data.map(|d| d.rows.iter().filter_map(|r| r.first().map(|v| v.to_display())).collect()).unwrap_or_default() };
            let before = obj.value.clone();
            egui::ComboBox::from_id_salt(("metric-dd", obj.id)).width(body.width() - 8.0).selected_text(obj.value.clone()).show_ui(&mut inner, |ui| {
                for c in &choices {
                    ui.selectable_value(&mut obj.value, c.clone(), c);
                }
            });
            if obj.value != before {
                committed.clone_from(&obj.value);
                out.emit = true;
            }
        }
        ObjectKind::Button => {
            if widgets::primary_button(&mut inner, &obj.label).clicked() {
                out.emit = true;
            }
        }
        _ => data_body(&mut inner, obj, data),
    }
    if !locked {
        let grip = Rect::from_min_size(rect.max - vec2(14.0, 14.0), vec2(14.0, 14.0));
        let r = ui.interact(grip, ui.id().with(("metric-grip", obj.id)), Sense::drag());
        if r.dragged() {
            out.resized = r.drag_delta();
        }
        if r.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
        }
        painter.line_segment([grip.max - vec2(3.0, 11.0), grip.max - vec2(11.0, 3.0)], Stroke::new(1.0_f32, theme::MUTED));
        painter.line_segment([grip.max - vec2(3.0, 6.0), grip.max - vec2(6.0, 3.0)], Stroke::new(1.0_f32, theme::MUTED));
    }
    out
}

/// Body of an object that shows query data.
fn data_body(ui: &mut Ui, obj: &MetricObject, data: Option<&ObjData>) {
    let Some(d) = data else {
        let text = if obj.sql.trim().is_empty() { "Set the SQL in the object settings" } else { "No data yet" };
        ui.label(egui::RichText::new(text).color(theme::MUTED));
        return;
    };
    if let Some(e) = &d.error {
        ui.label(egui::RichText::new(e).color(theme::ERROR));
        return;
    }
    if d.columns.is_empty() {
        ui.label(egui::RichText::new(if d.loading { "Loading..." } else { "The query returned no result set" }).color(theme::MUTED));
        return;
    }
    match obj.kind {
        ObjectKind::DataTable => table_body(ui, d),
        ObjectKind::Scoreboard => scoreboard_body(ui, d),
        ObjectKind::BarChart => bar_chart(ui, &metrics::label_values(&d.rows)),
        ObjectKind::PieChart => pie_chart(ui, &metrics::label_values(&d.rows)),
        ObjectKind::LineChart => line_chart(ui, &metrics::line_data(&d.columns, &d.rows)),
        ObjectKind::Input | ObjectKind::Dropdown | ObjectKind::Button => {}
    }
}

/// Grid of rows.
fn table_body(ui: &mut Ui, d: &ObjData) {
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new(ui.id().with("metric-table")).striped(true).num_columns(d.columns.len()).show(ui, |ui| {
            for c in &d.columns {
                ui.label(egui::RichText::new(&c.name).strong().color(theme::SECONDARY));
            }
            ui.end_row();
            for row in &d.rows {
                for v in row {
                    ui.label(v.to_display());
                }
                ui.end_row();
            }
        });
        if d.truncated {
            ui.label(egui::RichText::new(format!("First {MAX_ROWS} rows")).color(theme::TERTIARY));
        }
    });
}

/// Big numbers with their detail.
fn scoreboard_body(ui: &mut Ui, d: &ObjData) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for s in metrics::scores(&d.rows) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(s.score).size(26.0).strong().color(theme::TEXT));
                ui.label(egui::RichText::new(s.detail).color(theme::MUTED));
            });
        }
    });
}

/// Rectangle left for a chart, with room for the labels below it.
fn plot_rect(ui: &mut Ui) -> (Rect, egui::Painter) {
    let size = ui.available_size();
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    (resp.rect, painter)
}

/// Text cut to about `max` characters.
fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}...", text.chars().take(max.saturating_sub(3)).collect::<String>()) }
}

/// Vertical bars, one per row.
fn bar_chart(ui: &mut Ui, items: &[(String, f64)]) {
    let (rect, p) = plot_rect(ui);
    if items.is_empty() {
        return no_numbers(&p, rect);
    }
    let max = items.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max).max(f64::MIN_POSITIVE);
    let plot = Rect::from_min_max(rect.min + vec2(0.0, 14.0), rect.max - vec2(0.0, 16.0));
    let slot = plot.width() / items.len() as f32;
    for (i, (label, v)) in items.iter().enumerate() {
        let h = (plot.height() * (v.max(0.0) / max) as f32).max(1.0);
        let x = plot.min.x + slot * i as f32;
        let bar = Rect::from_min_max(pos2(x + slot * 0.15, plot.max.y - h), pos2(x + slot * 0.85, plot.max.y));
        p.rect_filled(bar, 2.0, palette(0));
        p.text(pos2(bar.center().x, bar.min.y - 1.0), Align2::CENTER_BOTTOM, format_number(*v), FontId::proportional(10.0), theme::TEXT);
        p.text(pos2(bar.center().x, plot.max.y + 2.0), Align2::CENTER_TOP, short(label, (slot / 6.0) as usize), FontId::proportional(10.0), theme::MUTED);
    }
    p.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1.0_f32, theme::FIELD_ROW_BORDER));
}

/// Lines with one colour per series and a legend.
fn line_chart(ui: &mut Ui, data: &metrics::LineData) {
    let (rect, p) = plot_rect(ui);
    let values = || data.series.iter().flat_map(|s| s.points.iter().copied()).filter(|v| v.is_finite());
    if data.series.is_empty() || values().next().is_none() {
        return no_numbers(&p, rect);
    }
    let (lo, hi) = values().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let plot = Rect::from_min_max(rect.min + vec2(0.0, 16.0), rect.max - vec2(0.0, 16.0));
    let n = data.x.len().max(2);
    let at = |i: usize, v: f64| pos2(plot.min.x + plot.width() * i as f32 / (n - 1) as f32, plot.max.y - plot.height() * ((v - lo) / span) as f32);
    p.text(rect.left_top(), Align2::LEFT_TOP, format!("{} - {}", format_number(lo), format_number(hi)), FontId::proportional(10.0), theme::MUTED);
    for (k, s) in data.series.iter().enumerate() {
        let color = palette(k);
        let mut run: Vec<Pos2> = Vec::new();
        for (i, v) in s.points.iter().enumerate() {
            if v.is_finite() {
                run.push(at(i, *v));
            } else if !run.is_empty() {
                p.add(Shape::line(std::mem::take(&mut run), Stroke::new(1.8_f32, color)));
            }
        }
        if run.len() == 1 {
            p.circle_filled(run[0], 2.5, color);
        } else if !run.is_empty() {
            p.add(Shape::line(run, Stroke::new(1.8_f32, color)));
        }
        p.text(rect.right_top() - vec2(0.0, -(k as f32) * 12.0), Align2::RIGHT_TOP, &s.name, FontId::proportional(10.0), color);
    }
    if let (Some(first), Some(last)) = (data.x.first(), data.x.last()) {
        p.text(plot.left_bottom() + vec2(0.0, 2.0), Align2::LEFT_TOP, short(first, 16), FontId::proportional(10.0), theme::MUTED);
        p.text(plot.right_bottom() + vec2(0.0, 2.0), Align2::RIGHT_TOP, short(last, 16), FontId::proportional(10.0), theme::MUTED);
    }
}

/// A pie with a legend on the right.
fn pie_chart(ui: &mut Ui, items: &[(String, f64)]) {
    let (rect, p) = plot_rect(ui);
    let total: f64 = items.iter().map(|(_, v)| v.max(0.0)).sum();
    if items.is_empty() || total <= 0.0 {
        return no_numbers(&p, rect);
    }
    let radius = (rect.height().min(rect.width() * 0.6) / 2.0 - 4.0).max(10.0);
    let center = pos2(rect.min.x + radius + 4.0, rect.center().y);
    let mut angle = -std::f32::consts::FRAC_PI_2;
    for (i, (_, v)) in items.iter().enumerate() {
        let sweep = (v.max(0.0) / total) as f32 * std::f32::consts::TAU;
        let steps = ((sweep / 0.12).ceil() as usize).max(1);
        for s in 0..steps {
            let (a, b) = (angle + sweep * s as f32 / steps as f32, angle + sweep * (s + 1) as f32 / steps as f32);
            let pt = |t: f32| center + vec2(t.cos(), t.sin()) * radius;
            p.add(Shape::convex_polygon(vec![center, pt(a), pt(b)], palette(i), Stroke::new(0.5_f32, palette(i))));
        }
        angle += sweep;
    }
    let mut y = rect.min.y;
    for (i, (label, v)) in items.iter().enumerate() {
        if y > rect.max.y - 12.0 {
            break;
        }
        let x = center.x + radius + 14.0;
        p.rect_filled(Rect::from_min_size(pos2(x, y + 2.0), vec2(9.0, 9.0)), 2.0, palette(i));
        p.text(pos2(x + 14.0, y), Align2::LEFT_TOP, format!("{} ({:.0}%)", short(label, 18), v.max(0.0) / total * 100.0), FontId::proportional(11.0), theme::TEXT);
        y += 15.0;
    }
}

/// Message of a chart whose result has no numbers.
fn no_numbers(p: &egui::Painter, rect: Rect) {
    p.text(rect.center(), Align2::CENTER_CENTER, "No numeric values to draw", FontId::proportional(12.0), theme::MUTED);
}

/// A number with at most two decimals.
fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{v:.0}") } else { format!("{v:.2}") }
}

/// Window with the settings of the object being edited.
fn settings_window(ctx: &egui::Context, tab: &mut MetricsTab) {
    let Some(id) = tab.editing else { return };
    let others: Vec<(u64, String)> = tab.board().objects.iter().filter(|o| o.id != id).map(|o| (o.id, format!("{} #{}", o.label, o.id))).collect();
    let Some(obj) = tab.board_mut().get_mut(id) else {
        tab.editing = None;
        return;
    };
    let before = obj.clone();
    let mut open = true;
    egui::Window::new(format!("{} settings", obj.kind.label())).id(egui::Id::new("metric-settings")).open(&mut open).default_width(420.0).show(ctx, |ui| {
        widgets::field_label(ui, "Label");
        widgets::input(ui, &mut obj.label, "Label");
        if obj.kind.is_variable() {
            widgets::field_label(ui, "Name (used as $name in queries)");
            widgets::input(ui, &mut obj.name, metrics::DEFAULT_VARIABLE);
            widgets::field_label(ui, "Value");
            widgets::input(ui, &mut obj.value, "Value, typed as it goes into the query");
        }
        if obj.kind == ObjectKind::Dropdown {
            let mut text = obj.choices.join("\n");
            widgets::field_label(ui, "Fixed choices, one per line (used when the SQL is empty)");
            if widgets::text_area(ui, &mut text, 3).changed() {
                obj.choices = text.lines().map(str::to_owned).filter(|l| !l.is_empty()).collect();
            }
        }
        if loads_data(obj) || obj.kind == ObjectKind::Dropdown {
            widgets::field_label(ui, "SQL (use $name for variables, strings are inserted as typed)");
            widgets::text_area(ui, &mut obj.sql, 6);
        }
        if obj.kind.runs_sql() {
            refresh_editor(ui, obj);
        }
        if !others.is_empty() {
            widgets::field_label(ui, "Refreshes these objects when it emits an event");
            for (oid, name) in &others {
                let mut on = obj.targets.contains(oid);
                if ui.checkbox(&mut on, name).changed() {
                    if on {
                        obj.targets.push(*oid);
                    } else {
                        obj.targets.retain(|t| t != oid);
                    }
                }
            }
        }
    });
    if *obj != before {
        tab.dirty = true;
    }
    if !open {
        tab.editing = None;
    }
}

/// Combo box and interval of the refresh rule.
fn refresh_editor(ui: &mut Ui, obj: &mut MetricObject) {
    widgets::field_label(ui, "Refresh rate");
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("metric-refresh").selected_text(refresh_label(obj.refresh)).show_ui(ui, |ui| {
            if ui.selectable_label(matches!(obj.refresh, Refresh::Every(_)), "Every...").clicked() && !matches!(obj.refresh, Refresh::Every(_)) {
                obj.refresh = Refresh::Every(30);
            }
            ui.selectable_value(&mut obj.refresh, Refresh::OnEvent, "On event");
            ui.selectable_value(&mut obj.refresh, Refresh::Manual, "Do not auto refresh");
        });
        if let Refresh::Every(s) = &mut obj.refresh {
            ui.add(egui::DragValue::new(s).range(metrics::MIN_INTERVAL_SECS..=metrics::MAX_INTERVAL_SECS).suffix(" s"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::value::Value;

    fn tab_with(kind: ObjectKind, sql: &str) -> (MetricsTab, u64) {
        let mut b = Board::new("b", "B");
        let id = b.add(kind, 0.0, 0.0);
        b.get_mut(id).unwrap().sql = sql.into();
        (MetricsTab::new(vec![b]), id)
    }

    fn runs(a: &[MetricsAction]) -> Vec<(u64, String)> {
        a.iter().filter_map(|a| if let MetricsAction::Run { object, sql } = a { Some((*object, sql.clone())) } else { None }).collect()
    }

    #[test]
    fn a_new_tab_always_has_a_board() {
        assert_eq!(MetricsTab::new(Vec::new()).boards.len(), 1);
    }

    #[test]
    fn due_objects_run_once_with_variables_replaced() {
        let (mut t, id) = tab_with(ObjectKind::Scoreboard, "select count(*) from t where c = $input");
        let v = t.board_mut().add(ObjectKind::Input, 0.0, 0.0);
        t.board_mut().get_mut(v).unwrap().value = "'x'".into();
        let mut a = Vec::new();
        t.dispatch(&mut a);
        assert_eq!(runs(&a), vec![(id, "select count(*) from t where c = 'x'".to_owned())]);
        a.clear();
        t.dispatch(&mut a);
        assert!(runs(&a).is_empty(), "still loading, must not run twice");
    }

    #[test]
    fn stopped_board_does_not_refresh_by_itself_but_manual_refresh_works() {
        let (mut t, id) = tab_with(ObjectKind::DataTable, "select 1");
        t.running = false;
        let mut a = Vec::new();
        t.dispatch(&mut a);
        assert!(runs(&a).is_empty());
        t.queue(id);
        t.dispatch(&mut a);
        assert_eq!(runs(&a).len(), 1);
    }

    #[test]
    fn results_are_collected_and_capped() {
        let (mut t, id) = tab_with(ObjectKind::DataTable, "select 1");
        let mut a = Vec::new();
        t.dispatch(&mut a);
        let cols = vec![ColumnMeta { name: "n".into(), ..ColumnMeta::default() }];
        t.on_rows(id, Some(cols), (0..MAX_ROWS as i64 + 5).map(|i| vec![Value::I64(i)]).collect());
        t.on_finished(id);
        let d = &t.data[&id];
        assert_eq!(d.rows.len(), MAX_ROWS);
        assert!(d.truncated && !d.loading);
    }

    #[test]
    fn late_results_of_an_object_not_waiting_are_ignored() {
        let (mut t, id) = tab_with(ObjectKind::DataTable, "select 1");
        t.on_rows(id, None, vec![vec![Value::I64(1)]]);
        t.on_error(id, "boom".into());
        assert!(!t.data.contains_key(&id));
    }

    #[test]
    fn a_variable_event_refreshes_the_objects_that_read_it() {
        let (mut t, reads) = tab_with(ObjectKind::Scoreboard, "select $input");
        t.running = false;
        let v = t.board_mut().add(ObjectKind::Input, 0.0, 0.0);
        t.emit(v);
        let mut a = Vec::new();
        t.dispatch(&mut a);
        assert_eq!(runs(&a).iter().map(|r| r.0).collect::<Vec<_>>(), vec![reads]);
    }

    #[test]
    fn switching_board_forgets_the_data() {
        let (mut t, id) = tab_with(ObjectKind::DataTable, "select 1");
        t.boards.push(Board::new("b2", "Two"));
        t.queue(id);
        let mut a = Vec::new();
        t.dispatch(&mut a);
        t.select_board(1);
        assert!(t.data.is_empty());
    }

    /// Every kind of object draws, with data, with an error and with nothing, without panicking.
    #[test]
    fn every_object_kind_renders() {
        let mut b = Board::new("b", "B");
        for (i, kind) in ObjectKind::ALL.into_iter().enumerate() {
            let id = b.add(kind, 10.0 + 30.0 * i as f32, 10.0 + 20.0 * i as f32);
            b.get_mut(id).unwrap().sql = "select 1".into();
            b.get_mut(id).unwrap().choices = vec!["a".into(), "b".into()];
        }
        let mut tab = MetricsTab::new(vec![b]);
        tab.grid = true;
        let cols = vec![ColumnMeta { name: "label".into(), ..ColumnMeta::default() }, ColumnMeta { name: "n".into(), ..ColumnMeta::default() }];
        let rows: Vec<Row> = (1..=4).map(|i| vec![Value::Text(format!("item {i}")), Value::I64(i * 10)]).collect();
        for o in tab.boards[0].objects.clone() {
            let d = if o.id % 3 == 0 { ObjData { error: Some("boom".into()), ..ObjData::default() } } else { ObjData { columns: cols.clone(), rows: rows.clone(), ..ObjData::default() } };
            tab.data.insert(o.id, d);
        }
        tab.editing = Some(1);
        tab.rename = Some("x".into());
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))), ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &mut tab);
                });
            });
        }
    }
}
