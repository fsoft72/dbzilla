//! The SQL editor tab: editor, run controls and result tabs (SPEC 10).

use super::grid::{self, GridEvent, GridInput, GridState};
use super::pending::Pending;
use crate::autocomplete::{self, Kind, Suggestion};
use super::widgets;
use crate::glyphs::Ico;
use crate::sqlhl;
use crate::sqltext;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ColumnMeta, Row};
use dbzilla_core::rowlimit::ROW_LIMITS;
use dbzilla_core::splitter::{split_statements, Statement};
use dbzilla_store::settings::CompleteKey;
use dbzilla_session::RequestId;
use eframe::egui::text::{CCursor, CCursorRange, LayoutJob};
use eframe::egui::{self, Align, Align2, FontId, Key, Layout, Modifiers, Order, RichText, Sense, Shape, Stroke, TextEdit, Ui};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// Width reserved for line numbers.
const GUTTER: f32 = 44.0;
/// Width of the completion popup.
const POPUP_WIDTH: f32 = 320.0;
/// Single result tab used when a script has more statements than this.
pub const MAX_RESULT_TABS: usize = 100;

/// Outcome of one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum ResStatus {
    Running,
    Done { elapsed: Duration, affected: Option<u64>, messages: Vec<String> },
    Error(String),
    Cancelled,
}

/// View of a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResView {
    Data,
    Message,
}

/// One result tab.
pub struct ResultTab {
    pub index: usize,
    pub title: String,
    pub sql: String,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Row>,
    pub grid: GridState,
    pub no_changes: Pending,
    pub status: ResStatus,
    pub pinned: bool,
    pub view: ResView,
}

impl ResultTab {
    /// A running result for statement `index`.
    pub fn running(index: usize, title: String, sql: String) -> Self {
        Self { index, title, sql, columns: Vec::new(), rows: Vec::new(), grid: GridState::default(), no_changes: Pending::default(), status: ResStatus::Running, pinned: false, view: ResView::Data }
    }

    /// Text of the Message view.
    pub fn message(&self) -> String {
        match &self.status {
            ResStatus::Running => "Running...".into(),
            ResStatus::Cancelled => "The statement was cancelled.".into(),
            ResStatus::Error(e) => e.clone(),
            ResStatus::Done { elapsed, affected, messages } => {
                let mut lines = Vec::new();
                if !self.columns.is_empty() {
                    lines.push(format!("{} rows returned in {} ms", self.rows.len(), elapsed.as_millis()));
                } else if let Some(n) = affected {
                    lines.push(format!("{n} rows affected in {} ms", elapsed.as_millis()));
                } else {
                    lines.push(format!("Done in {} ms", elapsed.as_millis()));
                }
                lines.extend(messages.iter().cloned());
                lines.join("\n")
            }
        }
    }
}

/// State of the completion popup.
#[derive(Default)]
pub struct Completion {
    pub items: Vec<Suggestion>,
    pub selected: usize,
    pub start: usize,
    pub open: bool,
    pub explicit: bool,
    dismissed_key: Option<u64>,
}

/// Find and replace bar state.
#[derive(Default)]
pub struct FindState {
    pub query: String,
    pub replace: String,
    pub case_sensitive: bool,
    pub focus: bool,
}

/// One SQL editor tab.
pub struct SqlTab {
    pub no: usize,
    pub text: String,
    pub row_limit: Option<u64>,
    pub results: Vec<ResultTab>,
    pub active_result: usize,
    pub run_req: Option<RequestId>,
    pub running: bool,
    pub find: Option<FindState>,
    pub selection: (usize, usize),
    /// Names of the statements of the current run (from `-- name:` comments).
    pub labels: Vec<Option<String>>,
    /// Number of statements of the current run.
    pub total: usize,
    /// The statements as typed (without the injected row limit), for the history.
    pub run_texts: Vec<String>,
    /// Saved query file this tab edits, relative to the queries folder.
    pub file: Option<String>,
    pub query_keywords: Vec<String>,
    pub saved_text: String,
    pub last_edit: Option<Instant>,
    pub complete: Completion,
    last_key: u64,
    edit_id: egui::Id,
    hl: Option<(u64, LayoutJob)>,
    pending_selection: Option<(usize, usize)>,
}

/// Default row limit of new editors.
const DEFAULT_ROW_LIMIT: Option<u64> = Some(1000);

impl SqlTab {
    /// A new empty editor.
    pub fn new(no: usize, text: String) -> Self {
        Self {
            no,
            text,
            row_limit: DEFAULT_ROW_LIMIT,
            results: Vec::new(),
            active_result: 0,
            run_req: None,
            running: false,
            find: None,
            selection: (0, 0),
            labels: Vec::new(),
            total: 0,
            run_texts: Vec::new(),
            file: None,
            query_keywords: Vec::new(),
            saved_text: String::new(),
            last_edit: None,
            complete: Completion::default(),
            last_key: 0,
            edit_id: egui::Id::new(("sql_editor", no)),
            hl: None,
            pending_selection: None,
        }
    }

    /// Statements to run: all of them, the selected ones, or the one under the cursor.
    pub fn statements_to_run(&self, all: bool, driver: DriverId) -> Vec<Statement> {
        if all {
            return split_statements(&self.text, driver);
        }
        let (a, b) = (self.selection.0.min(self.selection.1), self.selection.0.max(self.selection.1));
        if b > a && self.text.is_char_boundary(a) && self.text.is_char_boundary(b) {
            let selected = split_statements(&self.text[a..b], driver);
            if !selected.is_empty() {
                return selected;
            }
        }
        sqltext::statement_at(&self.text, a, driver).into_iter().collect()
    }

    /// Replaces the selection range (applied on the next frame).
    pub fn set_selection(&mut self, range: (usize, usize)) {
        self.pending_selection = Some(range);
    }

    /// Formats the selection, or the statement under the cursor.
    pub fn beautify(&mut self, driver: DriverId, f: impl Fn(&str) -> String) {
        let (a, b) = (self.selection.0.min(self.selection.1), self.selection.0.max(self.selection.1));
        let range = if b > a { Some((a, b)) } else { sqltext::statement_at(&self.text, a, driver).map(|s| (s.start, s.end)) };
        let Some((s, e)) = range.filter(|(s, e)| self.text.is_char_boundary(*s) && self.text.is_char_boundary(*e)) else { return };
        let new = f(&self.text[s..e]);
        let new = new.trim_end().to_owned();
        self.text.replace_range(s..e, &new);
        self.set_selection((s, s + new.len()));
    }

    /// Comments or uncomments the selected lines.
    pub fn toggle_comment(&mut self) {
        let (a, b) = (self.selection.0.min(self.selection.1), self.selection.0.max(self.selection.1));
        let (text, range) = sqltext::toggle_line_comment(&self.text, (a, b));
        self.text = text;
        self.set_selection(range);
    }

    /// Result by statement index.
    pub fn result_mut(&mut self, index: usize) -> Option<&mut ResultTab> {
        self.results.iter_mut().find(|r| r.index == index)
    }
}

/// Inputs of a frame.
pub struct SqlEnv<'a> {
    pub keywords: &'a HashSet<String>,
    pub driver: DriverId,
    pub font_size: f32,
    pub alternating: bool,
    pub row_height: f32,
    /// Tables and views `(name, is_view)`.
    pub tables: &'a [(&'a str, bool)],
    pub columns: crate::autocomplete::ColumnLookup<'a>,
    pub dialect_keywords: &'a [&'a str],
    pub saved: &'a HashMap<String, String>,
    pub uppercase_keywords: bool,
    pub complete_key: CompleteKey,
    pub quote: &'a dyn Fn(&str) -> String,
    pub completion: bool,
    /// Changes whenever the schema knowledge changes, so suggestions are recomputed.
    pub schema_rev: usize,
}

/// What the tab asks the workspace to do.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlAction {
    RunCurrent,
    RunAll,
    Cancel,
    Beautify,
    Minify,
    Comment,
    SaveQuery,
    NeedStructure(String),
    Toast(String),
}

/// Byte offset to char index.
fn char_index(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Char index to byte offset.
fn byte_index(text: &str, ch: usize) -> usize {
    text.char_indices().nth(ch).map_or(text.len(), |(i, _)| i)
}

/// Hash of the highlight inputs.
fn hl_key(text: &str, size: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    size.to_bits().hash(&mut h);
    h.finish()
}

/// Draws the tab; returns requested actions.
pub fn show(ui: &mut Ui, tab: &mut SqlTab, env: &SqlEnv) -> Vec<SqlAction> {
    let mut actions = Vec::new();
    egui::TopBottomPanel::top(ui.id().with("sql_bar")).exact_height(40.0).frame(egui::Frame::new().fill(theme::DEEP_DARK).inner_margin(egui::Margin::symmetric(8, 4))).show_inside(ui, |ui| run_bar(ui, tab, &mut actions));
    egui::TopBottomPanel::bottom(ui.id().with("sql_results")).resizable(true).default_height(260.0).height_range(80.0..=700.0).frame(egui::Frame::new().fill(theme::DEEP_DARK)).show_inside(ui, |ui| results(ui, tab, env, &mut actions));
    egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::INPUT_FILL)).show_inside(ui, |ui| {
        if tab.find.is_some() {
            find_bar(ui, tab);
        }
        editor(ui, tab, env, &mut actions);
    });
    actions
}

/// Run, cancel, format and row limit controls.
fn run_bar(ui: &mut Ui, tab: &mut SqlTab, actions: &mut Vec<SqlAction>) {
    ui.horizontal_centered(|ui| {
        ui.add_enabled_ui(!tab.running, |ui| {
            if widgets::primary_button(ui, "Run").on_hover_text("Run the statement under the cursor (Ctrl+Enter)").clicked() {
                actions.push(SqlAction::RunCurrent);
            }
            if widgets::secondary_button(ui, "Run all").on_hover_text("Run every statement (Ctrl+Shift+Enter)").clicked() {
                actions.push(SqlAction::RunAll);
            }
        });
        if tab.running && widgets::danger_button(ui, "Cancel").on_hover_text("Cancel the running statement (Ctrl+.)").clicked() {
            actions.push(SqlAction::Cancel);
        }
        ui.add_space(8.0);
        if widgets::ico_button(ui, Ico::Structure, "Beautify (Ctrl+I)").clicked() {
            actions.push(SqlAction::Beautify);
        }
        if widgets::ico_button(ui, Ico::Minus, "Minify the statement").clicked() {
            actions.push(SqlAction::Minify);
        }
        if widgets::ico_button(ui, Ico::Bookmark, "Add to Queries").clicked() {
            actions.push(SqlAction::SaveQuery);
        }
        if widgets::ico_button(ui, Ico::Search, "Find (Ctrl+F)").clicked() {
            tab.find = if tab.find.is_some() { None } else { Some(FindState { focus: true, ..Default::default() }) };
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = tab.row_limit.map_or("No limit".to_owned(), |n| format!("{n} rows"));
            egui::ComboBox::from_id_salt("row_limit").selected_text(label).width(120.0).show_ui(ui, |ui| {
                for l in ROW_LIMITS {
                    ui.selectable_value(&mut tab.row_limit, l, l.map_or("No limit".to_owned(), |n| n.to_string()));
                }
            });
            widgets::muted(ui, "Limit");
            if tab.running {
                ui.spinner();
            }
        });
    });
}

/// Find and replace bar.
fn find_bar(ui: &mut Ui, tab: &mut SqlTab) {
    let Some(f) = tab.find.as_mut() else { return };
    let mut close = false;
    let mut next = None;
    let mut replace_all = false;
    ui.horizontal(|ui| {
        let r = ui.add(TextEdit::singleline(&mut f.query).hint_text("Find").desired_width(180.0));
        if f.focus {
            r.request_focus();
            f.focus = false;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            next = Some(true);
        }
        ui.add(TextEdit::singleline(&mut f.replace).hint_text("Replace").desired_width(140.0));
        ui.checkbox(&mut f.case_sensitive, "Aa");
        if widgets::ico_button(ui, Ico::ChevronUp, "Previous").clicked() {
            next = Some(false);
        }
        if widgets::ico_button(ui, Ico::ChevronDown, "Next").clicked() {
            next = Some(true);
        }
        if ui.small_button("Replace all").clicked() {
            replace_all = true;
        }
        if widgets::ico_button(ui, Ico::Close, "Close").clicked() {
            close = true;
        }
    });
    let (query, replace, case) = (f.query.clone(), f.replace.clone(), f.case_sensitive);
    if let Some(forward) = next {
        let from = if forward { tab.selection.1.max(tab.selection.0) } else { tab.selection.0.min(tab.selection.1) };
        if let Some(r) = sqltext::find_next(&tab.text, &query, from, case, forward) {
            tab.set_selection(r);
        }
    }
    if replace_all && !query.is_empty() {
        tab.text = if case { tab.text.replace(&query, &replace) } else { replace_case_insensitive(&tab.text, &query, &replace) };
    }
    if close {
        tab.find = None;
    }
}

/// Replaces every case-insensitive occurrence.
fn replace_case_insensitive(text: &str, needle: &str, with: &str) -> String {
    let mut out = String::new();
    let mut from = 0;
    while let Some((s, e)) = sqltext::find_next(&text[from..], needle, 0, false, true).filter(|_| from <= text.len()) {
        out.push_str(&text[from..from + s]);
        out.push_str(with);
        from += e;
    }
    out.push_str(&text[from..]);
    out
}

/// The text editor with highlighting, line numbers and the current statement band.
fn editor(ui: &mut Ui, tab: &mut SqlTab, env: &SqlEnv, actions: &mut Vec<SqlAction>) {
    let font = FontId::monospace(env.font_size);
    let row_h = ui.fonts(|f| f.row_height(&font));
    let avail_h = ui.available_height();
    let rows_wanted = ((avail_h / row_h) as usize).max(8);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let (gutter_rect, _) = ui.allocate_exact_size(egui::vec2(GUTTER, row_h), Sense::hover());
            let band = ui.painter().add(Shape::Noop);
            let focused = ui.memory(|m| m.has_focus(tab.edit_id));
            let mut accept = false;
            if env.completion && focused {
                intercept_keys(ui, tab, env.complete_key, &mut accept);
            }
            let before = hl_key(&tab.text, 0.0);
            let SqlTab { text, hl, edit_id, .. } = tab;
            let mut layouter = |ui: &Ui, t: &str, wrap: f32| {
                let key = hl_key(t, env.font_size);
                if hl.as_ref().is_none_or(|(k, _)| *k != key) {
                    *hl = Some((key, sqlhl::layout_job(t, env.keywords, &font)));
                }
                let mut job = hl.as_ref().map(|(_, j)| j.clone()).unwrap_or_default();
                job.wrap.max_width = wrap;
                ui.fonts(|f| f.layout_job(job))
            };
            let mut out = TextEdit::multiline(text).id(*edit_id).font(font.clone()).frame(false).lock_focus(true).desired_width(f32::INFINITY).desired_rows(rows_wanted).layouter(&mut layouter).show(ui);
            if let Some(sel) = tab.pending_selection.take() {
                let (a, b) = (char_index(&tab.text, sel.0), char_index(&tab.text, sel.1));
                out.state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
                out.state.clone().store(ui.ctx(), tab.edit_id);
                out.response.request_focus();
            }
            if let Some(range) = out.cursor_range {
                tab.selection = (byte_index(&tab.text, range.secondary.ccursor.index), byte_index(&tab.text, range.primary.ccursor.index));
            }
            paint_band(ui, tab, env, &out, band);
            paint_gutter(ui, &out, env.font_size, gutter_rect.right());
            if hl_key(&tab.text, 0.0) != before {
                tab.last_edit = Some(Instant::now());
            }
            if env.completion {
                completion(ui, tab, env, &out, focused, accept, actions);
            }
        });
    });
}

/// Handles the navigation keys of the popup before the editor sees them.
fn intercept_keys(ui: &mut Ui, tab: &mut SqlTab, key: CompleteKey, accept: &mut bool) {
    let c = &mut tab.complete;
    let explicit_requested = ui.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Space));
    if explicit_requested {
        c.explicit = true;
        c.dismissed_key = None;
        tab.last_key = 0;
    }
    if !c.open || c.items.is_empty() {
        return;
    }
    let n = c.items.len();
    ui.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            c.selected = (c.selected + 1) % n;
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            c.selected = (c.selected + n - 1) % n;
        }
        if matches!(key, CompleteKey::Both | CompleteKey::EnterOnly) && i.consume_key(Modifiers::NONE, Key::Enter) {
            *accept = true;
        }
        if matches!(key, CompleteKey::Both | CompleteKey::TabOnly) && i.consume_key(Modifiers::NONE, Key::Tab) {
            *accept = true;
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            c.open = false;
            c.dismissed_key = Some(1);
        }
    });
}

/// Recomputes the suggestions, draws the popup and applies an accepted one.
fn completion(ui: &mut Ui, tab: &mut SqlTab, env: &SqlEnv, out: &egui::text_edit::TextEditOutput, focused: bool, accept: bool, actions: &mut Vec<SqlAction>) {
    let cursor = byte_index(&tab.text, out.cursor_range.map_or(0, |r| r.primary.ccursor.index));
    if accept {
        if let Some(item) = tab.complete.items.get(tab.complete.selected).cloned() {
            let start = tab.complete.start.min(cursor);
            tab.text.replace_range(start..cursor, &item.insert);
            tab.set_selection((start + item.insert.len(), start + item.insert.len()));
            tab.complete.open = false;
            tab.complete.dismissed_key = Some(1);
            ui.ctx().request_repaint();
            return;
        }
    }
    let key = hl_key(&tab.text, 1.0) ^ (cursor as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (env.schema_rev as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    if key != tab.last_key {
        tab.last_key = key;
        if tab.complete.dismissed_key != Some(1) || !tab.complete.open {
            tab.complete.dismissed_key = None;
        }
        let ctx = autocomplete::context(&tab.text, cursor, env.driver);
        let items = match (&ctx, focused) {
            (Some(c), true) => {
                for (t, _) in &c.tables {
                    if (env.columns)(t).is_none() && env.tables.iter().any(|(n, _)| n.eq_ignore_ascii_case(t)) {
                        actions.push(SqlAction::NeedStructure(t.clone()));
                    }
                }
                let schema = autocomplete::Schema { tables: env.tables.to_vec(), columns: env.columns, keywords: env.dialect_keywords, saved: env.saved };
                autocomplete::suggest(c, &schema, tab.complete.explicit, env.uppercase_keywords, env.quote)
            }
            _ => Vec::new(),
        };
        tab.complete.explicit = false;
        tab.complete.start = ctx.as_ref().map_or(cursor, |c| c.start);
        tab.complete.selected = tab.complete.selected.min(items.len().saturating_sub(1));
        tab.complete.open = !items.is_empty();
        tab.complete.items = items;
    }
    if !focused || !tab.complete.open || tab.complete.items.is_empty() {
        return;
    }
    let caret = out.galley.pos_from_ccursor(CCursor::new(out.cursor_range.map_or(0, |r| r.primary.ccursor.index)));
    let pos = out.galley_pos + egui::vec2(caret.left(), caret.bottom() + 4.0);
    let mut clicked = None;
    egui::Area::new(egui::Id::new((tab.edit_id, "complete"))).order(Order::Tooltip).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(theme::SURFACE).stroke(Stroke::new(1.0_f32, theme::FIELD_ROW_BORDER)).corner_radius(theme::RADIUS_CARD).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            ui.set_width(POPUP_WIDTH);
            for (i, it) in tab.complete.items.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::click());
                if i == tab.complete.selected || resp.hovered() {
                    ui.painter().rect_filled(rect, theme::RADIUS_CTRL, if i == tab.complete.selected { theme::INPUT_FILL_BLUE } else { theme::FIELD_ROW_BG });
                }
                let (badge, color) = match it.kind {
                    Kind::Table => ("T", theme::SECONDARY),
                    Kind::View => ("V", theme::SECONDARY),
                    Kind::Column => ("C", theme::OK),
                    Kind::Keyword => ("K", theme::LABEL),
                    Kind::Snippet => ("S", theme::TERTIARY),
                };
                ui.painter().text(rect.left_center() + egui::vec2(8.0, 0.0), Align2::LEFT_CENTER, badge, FontId::new(11.0, theme::semibold()), color);
                ui.painter().text(rect.left_center() + egui::vec2(24.0, 0.0), Align2::LEFT_CENTER, &it.label, FontId::monospace(12.5), theme::TEXT);
                ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), Align2::RIGHT_CENTER, &it.detail, FontId::new(11.0, egui::FontFamily::Proportional), theme::MUTED);
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    if let Some(i) = clicked {
        tab.complete.selected = i;
        if let Some(item) = tab.complete.items.get(i).cloned() {
            let start = tab.complete.start.min(cursor);
            tab.text.replace_range(start..cursor, &item.insert);
            tab.set_selection((start + item.insert.len(), start + item.insert.len()));
            tab.complete.open = false;
        }
    }
}

/// Highlights the statement under the cursor.
fn paint_band(ui: &Ui, tab: &SqlTab, env: &SqlEnv, out: &egui::text_edit::TextEditOutput, band: egui::layers::ShapeIdx) {
    let Some(stmt) = sqltext::statement_at(&tab.text, tab.selection.0.min(tab.selection.1), env.driver) else { return };
    let (a, b) = (char_index(&tab.text, stmt.start), char_index(&tab.text, stmt.end));
    let top = out.galley.pos_from_ccursor(CCursor::new(a)).top();
    let bottom = out.galley.pos_from_ccursor(CCursor::new(b)).bottom();
    let rect = egui::Rect::from_min_max(out.galley_pos + egui::vec2(-4.0, top), egui::pos2(out.response.rect.right(), out.galley_pos.y + bottom));
    ui.painter().set(band, Shape::rect_filled(rect, 0.0, theme::PRIMARY.gamma_multiply(0.07)));
}

/// Paints line numbers aligned with the galley rows.
fn paint_gutter(ui: &Ui, out: &egui::text_edit::TextEditOutput, size: f32, right: f32) {
    let clip = ui.clip_rect();
    let font = FontId::monospace(size * 0.9);
    let mut line = 1usize;
    let mut first_row_of_line = true;
    for row in &out.galley.rows {
        let y = out.galley_pos.y + row.rect.center().y;
        if first_row_of_line && y > clip.top() - 20.0 && y < clip.bottom() + 20.0 {
            ui.painter().text(egui::pos2(right - 10.0, y), Align2::RIGHT_CENTER, line.to_string(), font.clone(), theme::MUTED);
        }
        first_row_of_line = row.ends_with_newline;
        if row.ends_with_newline {
            line += 1;
        }
    }
}

/// Result tabs, message/data views and the grid.
fn results(ui: &mut Ui, tab: &mut SqlTab, env: &SqlEnv, actions: &mut Vec<SqlAction>) {
    if tab.results.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| widgets::muted(ui, "Run a statement to see its results here."));
        return;
    }
    tab.active_result = tab.active_result.min(tab.results.len() - 1);
    egui::Frame::new().fill(theme::NAVY_DARK).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (i, r) in tab.results.iter().enumerate() {
                let title = if r.pinned { format!("{} (pinned)", r.title) } else { r.title.clone() };
                ui.selectable_value(&mut tab.active_result, i, title);
            }
        });
    });
    let active = tab.active_result;
    let r = &mut tab.results[active];
    egui::Frame::new().fill(theme::DEEP_DARK).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut r.view, ResView::Data, "Data");
            ui.selectable_value(&mut r.view, ResView::Message, "Message");
            ui.checkbox(&mut r.pinned, "Pin");
            match &r.status {
                ResStatus::Running => {
                    ui.spinner();
                    widgets::muted(ui, &format!("{} rows so far", r.rows.len()));
                }
                ResStatus::Done { elapsed, .. } => {
                    widgets::muted(ui, &format!("{} rows, {} ms", r.rows.len(), elapsed.as_millis()));
                }
                ResStatus::Error(_) => {
                    ui.label(RichText::new("Error").color(theme::ERROR));
                }
                ResStatus::Cancelled => {
                    ui.label(RichText::new("Cancelled").color(theme::TERTIARY));
                }
            }
        });
    });
    let show_message = r.view == ResView::Message || r.columns.is_empty() || matches!(r.status, ResStatus::Error(_));
    if show_message {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            let color = if matches!(r.status, ResStatus::Error(_)) { theme::ERROR } else { theme::TEXT };
            ui.add(egui::Label::new(RichText::new(r.message()).monospace().color(color)).selectable(true));
        });
        return;
    }
    let id = ui.id().with(("result_grid", r.index));
    let input = GridInput { pending: &r.no_changes, editable: false, key_missing: true, columns: &r.columns, rows: &r.rows, sort: &[], alternating: env.alternating, row_height: env.row_height, id };
    for e in grid::show(ui, &mut r.grid, &input) {
        match e {
            GridEvent::Copy { format, cells_only } => {
                if let Some(text) = copy_text(&r.columns, &r.rows, &r.grid, format, cells_only) {
                    ui.ctx().copy_text(text);
                    actions.push(SqlAction::Toast("Copied".into()));
                }
            }
            GridEvent::CopyColumn(c) => {
                ui.ctx().copy_text(r.rows.iter().filter_map(|row| row.get(c)).map(dbzilla_core::value::Value::to_display).collect::<Vec<_>>().join("\n"));
                actions.push(SqlAction::Toast("Column values copied".into()));
            }
            GridEvent::HideColumn(c) => {
                r.grid.hidden.insert(c);
            }
            GridEvent::ShowAllColumns => r.grid.hidden.clear(),
            _ => {}
        }
    }
    let _ = (Sense::hover(), Stroke::NONE);
}

/// Copy text of a result selection.
fn copy_text(columns: &[ColumnMeta], rows: &[Row], g: &GridState, format: dbzilla_core::copyfmt::CopyFormat, cells_only: bool) -> Option<String> {
    use dbzilla_core::copyfmt::{format_rows, CopyContext, CsvOptions};
    let dialect = dbzilla_core::dialect::dialect_for(DriverId::Sqlite);
    let csv = CsvOptions::default();
    let picked_rows: Vec<Row>;
    let picked_cols: Vec<ColumnMeta>;
    match (cells_only, g.selection.range()) {
        (true, Some((r0, c0, r1, c1))) => {
            picked_cols = columns.get(c0..=c1.min(columns.len().checked_sub(1)?))?.to_vec();
            picked_rows = (r0..=r1).filter_map(|r| rows.get(r).map(|row| row.get(c0..=c1.min(row.len().saturating_sub(1))).unwrap_or(&[]).to_vec())).collect();
        }
        _ => {
            let sel = g.selection.selected_rows();
            if sel.is_empty() {
                return None;
            }
            picked_cols = columns.to_vec();
            picked_rows = sel.into_iter().filter_map(|r| rows.get(r).cloned()).collect();
        }
    }
    Some(format_rows(format, &picked_cols, &picked_rows, &CopyContext { dialect: dialect.as_ref(), table: None, csv: &csv }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Statement choice: all, selection, or the one under the cursor.
    #[test]
    fn statements_to_run() {
        let mut t = SqlTab::new(1, "SELECT 1;\nSELECT 2;\nSELECT 3".into());
        t.selection = (12, 12);
        assert_eq!(t.statements_to_run(false, DriverId::Sqlite).iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["SELECT 2"]);
        assert_eq!(t.statements_to_run(true, DriverId::Sqlite).len(), 3);
        t.selection = (0, 19);
        assert_eq!(t.statements_to_run(false, DriverId::Sqlite).len(), 2, "a selection runs every statement it covers");
    }

    /// Beautify replaces the statement under the cursor only.
    #[test]
    fn beautify_statement_under_cursor() {
        let mut t = SqlTab::new(1, "select 1;\nselect  a  from t".into());
        t.selection = (15, 15);
        t.beautify(DriverId::Sqlite, |s| s.to_uppercase());
        assert_eq!(t.text, "select 1;\nSELECT  A  FROM T");
    }

    /// Message text for the three kinds of outcome.
    #[test]
    fn result_messages() {
        let mut r = ResultTab::running(0, "Query 1".into(), "SELECT 1".into());
        r.columns = vec![ColumnMeta::default()];
        r.rows = vec![vec![]; 3];
        r.status = ResStatus::Done { elapsed: Duration::from_millis(12), affected: None, messages: vec!["NOTICE: hi".into()] };
        assert_eq!(r.message(), "3 rows returned in 12 ms\nNOTICE: hi");
        r.columns.clear();
        r.status = ResStatus::Done { elapsed: Duration::from_millis(1), affected: Some(5), messages: vec![] };
        assert_eq!(r.message(), "5 rows affected in 1 ms");
        r.status = ResStatus::Error("syntax error".into());
        assert_eq!(r.message(), "syntax error");
    }

    /// Byte and char indexes convert both ways on unicode text.
    #[test]
    fn index_conversion() {
        let s = "aè日b";
        assert_eq!(char_index(s, s.len()), 4);
        assert_eq!(byte_index(s, 2), 3);
        assert_eq!(byte_index(s, 99), s.len());
    }

    /// Case-insensitive replace-all.
    #[test]
    fn replace_all_insensitive() {
        assert_eq!(replace_case_insensitive("Select a, SELECT b", "select", "X"), "X a, X b");
        assert_eq!(replace_case_insensitive("abc", "zzz", "X"), "abc");
    }
}
