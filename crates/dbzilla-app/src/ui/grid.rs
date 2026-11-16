//! The data grid: virtualised table with selection, sorting and Quick Look entry points (SPEC 8.3).

use super::pending::{Pending, RowState};
use super::widgets;
use crate::glyphs::{self, Ico};
use crate::theme;
use dbzilla_core::changes::CellValue;
use dbzilla_core::copyfmt::CopyFormat;
use dbzilla_core::filter::FilterOp;
use dbzilla_core::model::{ColumnMeta, Row};
use dbzilla_core::paging::SortKey;
use dbzilla_core::value::Value;
use eframe::egui::{self, Align, Align2, CursorIcon, FontId, Key, Layout, Sense, Stroke, StrokeKind, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::BTreeSet;

/// Longest text drawn in a cell; the rest is cut (Quick Look shows everything).
const MAX_CELL_CHARS: usize = 300;
/// Height of the column header.
const HEADER_HEIGHT: f32 = 40.0;
/// Width of the row number gutter.
const GUTTER_WIDTH: f32 = 48.0;

/// Visual class of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellStyle {
    Normal,
    Null,
    Muted,
}

/// What a cell shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellView {
    pub text: String,
    pub right_aligned: bool,
    pub style: CellStyle,
}

/// Text and style of a value in the grid.
pub fn display_cell(v: &Value) -> CellView {
    let plain = |text: String, right| CellView { text, right_aligned: right, style: CellStyle::Normal };
    match v {
        Value::Null => CellView { text: "NULL".into(), right_aligned: false, style: CellStyle::Null },
        Value::Bytes(b) => CellView { text: format!("({} bytes)", b.len()), right_aligned: false, style: CellStyle::Muted },
        Value::I64(_) | Value::U64(_) | Value::F64(_) | Value::Decimal(_) => plain(v.to_display(), true),
        Value::Text(s) | Value::Other(s) | Value::Geometry(s) => plain(one_line(s), false),
        other => plain(one_line(&other.to_display()), false),
    }
}

/// First line-ish of a text: newlines become spaces and the length is capped.
fn one_line(s: &str) -> String {
    let mut out: String = s.chars().take(MAX_CELL_CHARS).map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    if s.chars().count() > MAX_CELL_CHARS {
        out.push('…');
    }
    out
}

/// Cycles the sort of a column: none -> descending -> ascending -> none. `multi` keeps other keys.
pub fn cycle_sort(sort: &mut Vec<SortKey>, column: &str, multi: bool) {
    let current = sort.iter().position(|k| k.column == column).map(|i| sort[i].desc);
    if !multi {
        sort.retain(|k| k.column == column);
    }
    match current {
        None => sort.push(SortKey { column: column.to_owned(), desc: true }),
        Some(true) => {
            if let Some(k) = sort.iter_mut().find(|k| k.column == column) {
                k.desc = false;
            }
        }
        Some(false) => sort.retain(|k| k.column != column),
    }
}

/// Selected cells and rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Anchor of the cell range.
    pub anchor: Option<(usize, usize)>,
    /// Moving end of the cell range (the cursor).
    pub cursor: Option<(usize, usize)>,
    /// Rows selected through the gutter.
    pub rows: BTreeSet<usize>,
}

impl Selection {
    /// Selects one cell.
    pub fn select_cell(&mut self, r: usize, c: usize) {
        self.anchor = Some((r, c));
        self.cursor = Some((r, c));
        self.rows.clear();
    }

    /// Extends the range to a cell.
    pub fn extend_to(&mut self, r: usize, c: usize) {
        if self.anchor.is_none() {
            self.anchor = Some((r, c));
        }
        self.cursor = Some((r, c));
        self.rows.clear();
    }

    /// Selects whole rows: replaces, toggles (`toggle`) or extends from the last row (`extend`).
    pub fn select_row(&mut self, r: usize, toggle: bool, extend: bool, last: Option<usize>) {
        if extend {
            let from = last.unwrap_or(r);
            self.rows.extend(from.min(r)..=from.max(r));
        } else if toggle {
            if !self.rows.remove(&r) {
                self.rows.insert(r);
            }
        } else {
            self.rows.clear();
            self.rows.insert(r);
        }
        self.anchor = None;
        self.cursor = None;
    }

    /// Clears everything.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Normalised cell range `(row0, col0, row1, col1)`.
    pub fn range(&self) -> Option<(usize, usize, usize, usize)> {
        let ((ar, ac), (cr, cc)) = (self.anchor?, self.cursor?);
        Some((ar.min(cr), ac.min(cc), ar.max(cr), ac.max(cc)))
    }

    /// True when the cell is inside the selection.
    pub fn contains(&self, r: usize, c: usize) -> bool {
        self.rows.contains(&r) || self.range().is_some_and(|(r0, c0, r1, c1)| (r0..=r1).contains(&r) && (c0..=c1).contains(&c))
    }

    /// Rows touched by the selection, ascending.
    pub fn selected_rows(&self) -> Vec<usize> {
        let mut v: BTreeSet<usize> = self.rows.clone();
        if let Some((r0, _, r1, _)) = self.range() {
            v.extend(r0..=r1);
        }
        v.into_iter().collect()
    }

    /// Moves the cursor by a delta, clamped to the grid; `extend` keeps the anchor.
    pub fn move_cursor(&mut self, dr: isize, dc: isize, rows: usize, cols: usize, extend: bool) {
        if rows == 0 || cols == 0 {
            return;
        }
        let (r, c) = self.cursor.unwrap_or((0, 0));
        let nr = (r as isize + dr).clamp(0, rows as isize - 1) as usize;
        let nc = (c as isize + dc).clamp(0, cols as isize - 1) as usize;
        if extend {
            self.extend_to(nr, nc);
        } else {
            self.select_cell(nr, nc);
        }
    }
}

/// A cell being edited inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editing {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub fresh: bool,
}

/// Grid state kept by the owner (one per tab).
#[derive(Default)]
pub struct GridState {
    pub editing: Option<Editing>,
    pub selection: Selection,
    pub hidden: BTreeSet<usize>,
    /// True after a click inside the grid: keyboard navigation is active.
    pub focused: bool,
    pub dragging: bool,
    pub last_row_click: Option<usize>,
}

/// Read-only inputs of a frame.
pub struct GridInput<'a> {
    pub pending: &'a Pending,
    /// False when the data cannot be edited (views).
    pub editable: bool,
    /// True when loaded rows cannot be updated or deleted because no key is known.
    pub key_missing: bool,
    pub columns: &'a [ColumnMeta],
    pub rows: &'a [Row],
    pub sort: &'a [SortKey],
    pub alternating: bool,
    pub row_height: f32,
    pub id: egui::Id,
}

/// What the user did in the grid.
#[derive(Debug, Clone, PartialEq)]
pub enum GridEvent {
    SortCycle { column: String, multi: bool },
    SortSet { column: String, desc: Option<bool> },
    HideColumn(usize),
    ShowAllColumns,
    QuickLook { row: usize, col: usize },
    FollowFk { row: usize, col: usize },
    Copy { format: CopyFormat, cells_only: bool },
    CopyColumn(usize),
    EditCommit { row: usize, col: usize, text: String },
    SetValue { row: usize, col: usize, value: CellValue },
    InsertRow,
    DuplicateRows,
    DeleteRows,
    FilterColumn(usize),
    QuickFilter { col: usize, op: FilterOp, value: String },
}

/// Initial width of a column from its name.
fn initial_width(c: &ColumnMeta) -> f32 {
    (c.name.chars().count() as f32 * 8.0 + 36.0).clamp(90.0, 260.0)
}

/// Draws the grid and returns what happened.
pub fn show(ui: &mut Ui, st: &mut GridState, input: &GridInput) -> Vec<GridEvent> {
    let mut events = Vec::new();
    let visible: Vec<usize> = (0..input.columns.len()).filter(|c| !st.hidden.contains(c)).collect();
    let n_rows = input.rows.len();
    handle_keys(ui, st, input, n_rows, input.columns.len(), &mut events);
    let mut table = TableBuilder::new(ui)
        .id_salt(input.id)
        .striped(false)
        .resizable(true)
        .vscroll(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(GUTTER_WIDTH));
    for (i, &c) in visible.iter().enumerate() {
        let col = if i + 1 == visible.len() { Column::remainder().at_least(initial_width(&input.columns[c]).min(140.0)) } else { Column::initial(initial_width(&input.columns[c])).at_least(48.0) };
        table = table.column(col.clip(true));
    }
    table
        .header(HEADER_HEIGHT, |mut header| {
            header.col(|ui| {
                ui.painter().rect_filled(ui.max_rect(), 0.0, theme::NAVY_DARK);
            });
            for &c in &visible {
                header.col(|ui| header_cell(ui, st, input, c, &mut events));
            }
        })
        .body(|body| {
            body.rows(input.row_height, n_rows, |mut row| {
                let r = row.index();
                row.col(|ui| gutter_cell(ui, st, r, input));
                for (vc, &c) in visible.iter().enumerate() {
                    row.col(|ui| data_cell(ui, st, input, r, c, vc, &mut events));
                }
            });
        });
    events
}

/// Header: name, type and sort indicator; click cycles the sort.
fn header_cell(ui: &mut Ui, st: &mut GridState, input: &GridInput, c: usize, events: &mut Vec<GridEvent>) {
    let col = &input.columns[c];
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, theme::NAVY_DARK);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0_f32, theme::FIELD_ROW_BORDER));
    let (r, resp) = ui.allocate_exact_size(rect.size(), Sense::click());
    let painter = ui.painter().with_clip_rect(r);
    painter.text(r.left_top() + egui::vec2(8.0, 9.0), Align2::LEFT_TOP, &col.name, FontId::new(12.5, theme::semibold()), theme::TEXT);
    painter.text(r.left_top() + egui::vec2(8.0, 24.0), Align2::LEFT_TOP, &col.type_name, FontId::new(11.0, egui::FontFamily::Proportional), theme::MUTED);
    let mut right = r.right() - 14.0;
    if let Some(k) = input.sort.iter().find(|k| k.column == col.name) {
        let icon = egui::Rect::from_center_size(egui::pos2(right, r.center().y), egui::vec2(20.0, 20.0));
        glyphs::paint(ui.painter(), icon, if k.desc { Ico::ChevronDown } else { Ico::ChevronUp }, theme::SECONDARY);
        right -= 20.0;
    }
    if col.is_pk {
        let icon = egui::Rect::from_center_size(egui::pos2(right, r.center().y), egui::vec2(20.0, 20.0));
        glyphs::paint(ui.painter(), icon, Ico::Key, theme::TERTIARY);
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        events.push(GridEvent::SortCycle { column: col.name.clone(), multi: ui.input(|i| i.modifiers.shift) });
    }
    resp.context_menu(|ui| {
        widgets::flat_menu_style(ui);
        let mut pick = |ui: &mut Ui, label: &str, e: GridEvent| {
            if ui.button(label).clicked() {
                events.push(e);
                ui.close_menu();
            }
        };
        pick(ui, "Sort ascending", GridEvent::SortSet { column: col.name.clone(), desc: Some(false) });
        pick(ui, "Sort descending", GridEvent::SortSet { column: col.name.clone(), desc: Some(true) });
        pick(ui, "Remove sort", GridEvent::SortSet { column: col.name.clone(), desc: None });
        ui.separator();
        pick(ui, "Hide this column", GridEvent::HideColumn(c));
        if !st.hidden.is_empty() {
            pick(ui, "Show all columns", GridEvent::ShowAllColumns);
        }
        pick(ui, "Copy all column values", GridEvent::CopyColumn(c));
        ui.separator();
        pick(ui, "Filter with column", GridEvent::FilterColumn(c));
    });
}

/// The row number gutter; clicking selects the row.
fn gutter_cell(ui: &mut Ui, st: &mut GridState, r: usize, input: &GridInput) {
    let alternating = input.alternating;
    let (rect, resp) = ui.allocate_exact_size(ui.available_size_before_wrap(), Sense::click());
    let selected = st.selection.rows.contains(&r);
    let fill = if selected {
        theme::INPUT_FILL_BLUE
    } else if let Some(tint) = state_tint(input.pending.state(r)) {
        tint
    } else if alternating && r % 2 == 1 {
        theme::NAVY_DARK
    } else {
        theme::INPUT_FILL
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), Align2::RIGHT_CENTER, (r + 1).to_string(), FontId::new(11.5, egui::FontFamily::Proportional), if selected { theme::SECONDARY } else { theme::MUTED });
    if resp.clicked() {
        let (toggle, extend) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let last = st.last_row_click;
        st.selection.select_row(r, toggle, extend, last);
        st.last_row_click = Some(r);
        st.focused = true;
    }
}

/// True when the row may be edited or deleted.
fn can_edit_row(input: &GridInput, r: usize) -> bool {
    input.editable && (!input.key_missing || input.pending.state(r) == RowState::New) && input.pending.state(r) != RowState::Deleted
}

/// Background tint of a changed row.
fn state_tint(state: RowState) -> Option<egui::Color32> {
    match state {
        RowState::Clean => None,
        RowState::New => Some(theme::OK.gamma_multiply(0.14)),
        RowState::Modified => Some(theme::TERTIARY.gamma_multiply(0.12)),
        RowState::Deleted => Some(theme::ERROR.gamma_multiply(0.16)),
    }
}

/// One data cell with selection, drag and context menu.
fn data_cell(ui: &mut Ui, st: &mut GridState, input: &GridInput, r: usize, c: usize, vc: usize, events: &mut Vec<GridEvent>) {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size_before_wrap(), Sense::click_and_drag());
    let selected = st.selection.contains(r, c);
    let row_state = input.pending.state(r);
    let fill = if selected {
        theme::PRIMARY.gamma_multiply(0.30)
    } else if let Some(tint) = state_tint(row_state) {
        tint
    } else if input.alternating && r % 2 == 1 {
        theme::FIELD_ROW_BG.gamma_multiply(0.55)
    } else {
        theme::DEEP_DARK
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    if st.selection.cursor == Some((r, c)) {
        ui.painter().rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, theme::SECONDARY), StrokeKind::Inside);
    }
    let view = input.rows.get(r).and_then(|row| row.get(c)).map(display_cell);
    let editing_here = st.editing.as_ref().is_some_and(|e| e.row == r && e.col == c);
    // The cell text is not painted while the inline editor covers it, or the two overlap.
    if let Some(v) = view.as_ref().filter(|_| !editing_here) {
        let painter = ui.painter().with_clip_rect(rect);
        let (font, color) = match v.style {
            CellStyle::Null => (FontId::new(13.0, egui::FontFamily::Proportional), theme::MUTED.gamma_multiply(0.7)),
            CellStyle::Muted => (FontId::new(12.5, egui::FontFamily::Proportional), theme::MUTED),
            CellStyle::Normal => (FontId::new(13.5, egui::FontFamily::Proportional), theme::TEXT),
        };
        let reserve = if input.columns[c].fk.is_some() { 24.0 } else { 8.0 };
        let (pos, align) = if v.right_aligned { (rect.right_center() - egui::vec2(reserve, 0.0), Align2::RIGHT_CENTER) } else { (rect.left_center() + egui::vec2(8.0, 0.0), Align2::LEFT_CENTER) };
        let text_rect = painter.text(pos, align, &v.text, font, color);
        if row_state == RowState::Deleted {
            painter.line_segment([text_rect.left_center(), text_rect.right_center()], Stroke::new(1.0_f32, theme::ERROR));
        }
    }
    if input.pending.is_edited(r, c) && row_state == RowState::Modified {
        let bar = egui::Rect::from_min_size(rect.left_top(), egui::vec2(2.0, rect.height()));
        ui.painter().rect_filled(bar, 0.0, theme::TERTIARY);
    }
    if let Some(ed) = st.editing.as_mut().filter(|e| e.row == r && e.col == c) {
        let edit = ui.put(rect, egui::TextEdit::singleline(&mut ed.text).frame(false).margin(egui::Margin::symmetric(6, 0)).vertical_align(Align::Center).desired_width(rect.width()));
        if ed.fresh {
            edit.request_focus();
            ed.fresh = false;
        }
        if edit.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            let text = ed.text.clone();
            st.editing = None;
            if !cancelled {
                events.push(GridEvent::EditCommit { row: r, col: c, text });
            }
        }
        return;
    }
    if input.columns[c].fk.is_some() && view.as_ref().is_some_and(|v| v.style != CellStyle::Null) {
        let arrow = egui::Rect::from_center_size(egui::pos2(rect.right() - 12.0, rect.center().y), egui::vec2(18.0, 18.0));
        let hit = ui.interact(arrow, resp.id.with("fk"), Sense::click());
        glyphs::paint(ui.painter(), arrow, Ico::ChevronRight, if hit.hovered() { Color32Ext::white() } else { theme::SECONDARY });
        if hit.on_hover_text("Open the referenced row").on_hover_cursor(CursorIcon::PointingHand).clicked() {
            events.push(GridEvent::FollowFk { row: r, col: c });
        }
    }
    let (shift, command) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    if resp.drag_started() || (resp.clicked() && !shift) {
        st.selection.select_cell(r, c);
        st.focused = true;
        st.dragging = resp.drag_started();
    } else if resp.clicked() && shift {
        st.selection.extend_to(r, c);
    }
    if st.dragging && resp.hovered() && ui.input(|i| i.pointer.primary_down()) {
        st.selection.extend_to(r, c);
    }
    if ui.input(|i| i.pointer.any_released()) {
        st.dragging = false;
    }
    if resp.clicked_by(egui::PointerButton::Middle) || (resp.double_clicked() && command) {
        events.push(GridEvent::QuickLook { row: r, col: c });
    } else if resp.double_clicked() && can_edit_row(input, r) {
        let text = input.rows.get(r).and_then(|row| row.get(c)).map(|v| if v.is_null() { String::new() } else { v.to_display() }).unwrap_or_default();
        st.editing = Some(Editing { row: r, col: c, text, fresh: true });
    }
    resp.context_menu(|ui| {
        widgets::flat_menu_style(ui);
        if !st.selection.contains(r, c) {
            st.selection.select_cell(r, c);
        }
        let mut pick = |ui: &mut Ui, label: &str, e: GridEvent| {
            if ui.button(label).clicked() {
                events.push(e);
                ui.close_menu();
            }
        };
        pick(ui, "Quick look", GridEvent::QuickLook { row: r, col: c });
        ui.separator();
        pick(ui, "Copy rows", GridEvent::Copy { format: CopyFormat::PlainText, cells_only: false });
        pick(ui, "Copy selected cells", GridEvent::Copy { format: CopyFormat::PlainText, cells_only: true });
        ui.menu_button("Copy rows as", |ui| {
            for (label, f) in FORMATS {
                pick(ui, label, GridEvent::Copy { format: f, cells_only: false });
            }
        });
        ui.menu_button("Copy selected cells as", |ui| {
            for (label, f) in FORMATS {
                pick(ui, label, GridEvent::Copy { format: f, cells_only: true });
            }
        });
        pick(ui, "Copy all column values", GridEvent::CopyColumn(c));
        ui.menu_button("Quick filter", |ui| {
            let shown = input.rows.get(r).and_then(|row| row.get(c)).map(|v| if v.is_null() { String::new() } else { v.to_display() }).unwrap_or_default();
            let is_null = input.rows.get(r).and_then(|row| row.get(c)).is_some_and(Value::is_null);
            let entries: Vec<(String, FilterOp, String)> = if is_null {
                vec![("IS NULL".into(), FilterOp::IsNull, String::new()), ("IS NOT NULL".into(), FilterOp::IsNotNull, String::new())]
            } else {
                vec![
                    (format!("= {shown}"), FilterOp::Eq, shown.clone()),
                    (format!("<> {shown}"), FilterOp::Ne, shown.clone()),
                    ("Contains".into(), FilterOp::Contains, shown.clone()),
                    ("Has prefix".into(), FilterOp::HasPrefix, shown.clone()),
                    ("Has suffix".into(), FilterOp::HasSuffix, shown.clone()),
                    ("IS NULL".into(), FilterOp::IsNull, String::new()),
                ]
            };
            for (label, op, value) in entries {
                pick(ui, &label.chars().take(40).collect::<String>(), GridEvent::QuickFilter { col: c, op, value });
            }
        });
        if input.editable {
            ui.separator();
            ui.add_enabled_ui(can_edit_row(input, r), |ui| {
            ui.menu_button("Set value", |ui| {
                for (label, v) in [("EMPTY", CellValue::Value(Value::Text(String::new()))), ("NULL", CellValue::Null), ("DEFAULT", CellValue::Default)] {
                    pick(ui, label, GridEvent::SetValue { row: r, col: c, value: v });
                }
            });
            });
            pick(ui, "Insert row", GridEvent::InsertRow);
            pick(ui, "Duplicate row", GridEvent::DuplicateRows);
            ui.add_enabled_ui(can_edit_row(input, r), |ui| {
                pick(ui, "Delete row", GridEvent::DeleteRows);
            });
        }
    });
    let _ = vc;
}

/// Formats offered in the copy menus.
pub const FORMATS: [(&str, CopyFormat); 7] = [
    ("Plain text", CopyFormat::PlainText),
    ("JSON", CopyFormat::Json),
    ("HTML", CopyFormat::Html),
    ("Markdown table", CopyFormat::Markdown),
    ("CSV", CopyFormat::Csv),
    ("CSV with header", CopyFormat::CsvWithHeader),
    ("INSERT statement", CopyFormat::InsertStatement),
];

/// Tiny helper to keep icon colours named.
struct Color32Ext;

impl Color32Ext {
    /// White, used for hovered icons.
    fn white() -> egui::Color32 {
        egui::Color32::WHITE
    }
}

/// Keyboard navigation and copy when the grid has focus.
fn handle_keys(ui: &mut Ui, st: &mut GridState, input: &GridInput, rows: usize, cols: usize, events: &mut Vec<GridEvent>) {
    if !st.focused || ui.ctx().wants_keyboard_input() {
        return;
    }
    let (dr, dc, shift, cmd, esc, all, copy, qlook) = ui.input(|i| {
        let mut dr = 0isize;
        let mut dc = 0isize;
        if i.key_pressed(Key::ArrowDown) {
            dr = 1;
        }
        if i.key_pressed(Key::ArrowUp) {
            dr = -1;
        }
        if i.key_pressed(Key::ArrowRight) {
            dc = 1;
        }
        if i.key_pressed(Key::ArrowLeft) {
            dc = -1;
        }
        (dr, dc, i.modifiers.shift, i.modifiers.command, i.key_pressed(Key::Escape), i.key_pressed(Key::A), i.events.iter().any(|e| matches!(e, egui::Event::Copy)), i.key_pressed(Key::Enter))
    });
    if dr != 0 || dc != 0 {
        let (dr, dc) = if cmd { (dr * rows as isize, dc * cols as isize) } else { (dr, dc) };
        st.selection.move_cursor(dr, dc, rows, cols, shift);
    }
    if esc {
        st.selection.clear();
    }
    if cmd && all && rows > 0 && cols > 0 {
        st.selection.anchor = Some((0, 0));
        st.selection.cursor = Some((rows - 1, cols - 1));
        st.selection.rows.clear();
    }
    if copy {
        events.push(GridEvent::Copy { format: CopyFormat::PlainText, cells_only: shift });
    }
    if cmd && qlook {
        if let Some((r, c)) = st.selection.cursor {
            events.push(GridEvent::QuickLook { row: r, col: c });
        }
    } else if qlook && !cmd {
        if let Some((r, c)) = st.selection.cursor.filter(|(r, _)| can_edit_row(input, *r)) {
            let text = input.rows.get(r).and_then(|row| row.get(c)).map(|v| if v.is_null() { String::new() } else { v.to_display() }).unwrap_or_default();
            st.editing = Some(Editing { row: r, col: c, text, fresh: true });
        }
    }
    let rows_ok = st.selection.selected_rows().iter().all(|r| can_edit_row(input, *r));
    if input.editable && rows_ok && ui.input(|i| i.key_pressed(Key::Delete)) && !cmd && !st.selection.selected_rows().is_empty() {
        events.push(GridEvent::DeleteRows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NULL, empty text, blobs and long text render distinctly.
    #[test]
    fn cell_display() {
        assert_eq!(display_cell(&Value::Null), CellView { text: "NULL".into(), right_aligned: false, style: CellStyle::Null });
        assert_eq!(display_cell(&Value::Text(String::new())).text, "");
        assert_eq!(display_cell(&Value::Text(String::new())).style, CellStyle::Normal);
        assert_eq!(display_cell(&Value::Bytes(vec![1, 2, 3])).text, "(3 bytes)");
        assert!(display_cell(&Value::I64(5)).right_aligned);
        let long = display_cell(&Value::Text("x".repeat(1000)));
        assert!(long.text.chars().count() <= MAX_CELL_CHARS + 1 && long.text.ends_with('…'));
        assert_eq!(display_cell(&Value::Text("a\nb".into())).text, "a b");
    }

    /// Sorting cycles descending, ascending, none; shift keeps other keys.
    #[test]
    fn sort_cycle() {
        let mut s = Vec::new();
        cycle_sort(&mut s, "a", false);
        assert_eq!(s, vec![SortKey { column: "a".into(), desc: true }]);
        cycle_sort(&mut s, "a", false);
        assert_eq!(s, vec![SortKey { column: "a".into(), desc: false }]);
        cycle_sort(&mut s, "a", false);
        assert!(s.is_empty());
        cycle_sort(&mut s, "a", false);
        cycle_sort(&mut s, "b", false);
        assert_eq!(s.len(), 1, "a plain click replaces other keys");
        cycle_sort(&mut s, "c", true);
        assert_eq!(s.iter().map(|k| k.column.as_str()).collect::<Vec<_>>(), vec!["b", "c"]);
    }

    /// Cell ranges, row toggling and extension.
    #[test]
    fn selection_model() {
        let mut s = Selection::default();
        s.select_cell(2, 1);
        s.extend_to(4, 3);
        assert_eq!(s.range(), Some((2, 1, 4, 3)));
        assert!(s.contains(3, 2) && !s.contains(5, 2) && !s.contains(3, 0));
        assert_eq!(s.selected_rows(), vec![2, 3, 4]);
        s.select_row(1, false, false, None);
        s.select_row(4, true, false, Some(1));
        assert_eq!(s.selected_rows(), vec![1, 4]);
        s.select_row(6, false, true, Some(4));
        assert_eq!(s.selected_rows(), vec![1, 4, 5, 6]);
        s.select_row(4, true, false, None);
        assert!(!s.rows.contains(&4));
        s.clear();
        assert_eq!(s, Selection::default());
    }

    /// The cursor moves within bounds and can extend the range.
    #[test]
    fn cursor_movement() {
        let mut s = Selection::default();
        s.select_cell(0, 0);
        s.move_cursor(-1, -1, 10, 3, false);
        assert_eq!(s.cursor, Some((0, 0)));
        s.move_cursor(100, 100, 10, 3, true);
        assert_eq!(s.range(), Some((0, 0, 9, 2)));
        s.move_cursor(-1, 0, 10, 3, false);
        assert_eq!(s.cursor, Some((8, 2)));
        assert_eq!(s.anchor, Some((8, 2)));
    }
}
