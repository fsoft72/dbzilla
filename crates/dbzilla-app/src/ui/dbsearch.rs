//! Search in Database: look for a value in every table, one table at a time (SPEC 8.13).

use super::widgets::{self, muted};
use crate::theme;
use dbzilla_core::filter::{Filter, FilterColumn, FilterOp};
use dbzilla_core::model::TableRef;
use dbzilla_session::RequestId;
use eframe::egui::{self, RichText};
use std::collections::VecDeque;

/// Match modes offered by the dialog.
pub const MODES: [FilterOp; 5] = [FilterOp::Contains, FilterOp::NotContains, FilterOp::Eq, FilterOp::HasPrefix, FilterOp::HasSuffix];

/// State of a search.
pub struct SearchState {
    pub value: String,
    pub op: FilterOp,
    pub scope_all: bool,
    pub picked: Vec<(TableRef, bool)>,
    pub running: bool,
    pub paused: bool,
    queue: VecDeque<TableRef>,
    pub current: Option<TableRef>,
    pub awaiting_structure: Option<TableRef>,
    pub req: Option<RequestId>,
    /// Tables with matches and their counts.
    pub results: Vec<(TableRef, u64)>,
    pub scanned: usize,
    pub total: usize,
    pub skipped: Vec<(TableRef, String)>,
}

impl SearchState {
    /// A fresh dialog over the given tables.
    pub fn new(tables: &[TableRef]) -> Self {
        Self {
            value: String::new(),
            op: FilterOp::Contains,
            scope_all: true,
            picked: tables.iter().map(|t| (t.clone(), true)).collect(),
            running: false,
            paused: false,
            queue: VecDeque::new(),
            current: None,
            awaiting_structure: None,
            req: None,
            results: Vec::new(),
            scanned: 0,
            total: 0,
            skipped: Vec::new(),
        }
    }

    /// Starts scanning the chosen tables.
    pub fn start(&mut self) {
        let tables: Vec<TableRef> = if self.scope_all { self.picked.iter().map(|(t, _)| t.clone()).collect() } else { self.picked.iter().filter(|(_, on)| *on).map(|(t, _)| t.clone()).collect() };
        self.total = tables.len();
        self.queue = tables.into();
        self.results.clear();
        self.skipped.clear();
        self.scanned = 0;
        self.current = None;
        self.awaiting_structure = None;
        self.req = None;
        self.running = !self.value.is_empty() && self.total > 0;
        self.paused = false;
    }

    /// The next table to scan, when running and not paused.
    pub fn next_table(&mut self) -> Option<TableRef> {
        if !self.running || self.paused || self.current.is_some() {
            return None;
        }
        match self.queue.pop_front() {
            Some(t) => {
                self.current = Some(t.clone());
                Some(t)
            }
            None => {
                self.running = false;
                None
            }
        }
    }

    /// Records the match count of the table being scanned.
    pub fn record(&mut self, count: u64) {
        if let Some(t) = self.current.take() {
            if count > 0 {
                self.results.push((t, count));
            }
        }
        self.scanned += 1;
        self.req = None;
    }

    /// Records a table that could not be scanned.
    pub fn skip(&mut self, reason: String) {
        if let Some(t) = self.current.take() {
            self.skipped.push((t, reason));
        }
        self.scanned += 1;
        self.req = None;
    }

    /// The "any column" filter used for every table.
    pub fn filter(&self) -> Filter {
        Filter { enabled: true, column: FilterColumn::Any, op: self.op, value: self.value.clone() }
    }
}

/// What the dialog asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchAction {
    Start,
    Pause,
    Resume,
    Done,
    Open(TableRef),
}

/// Draws the dialog.
pub fn show(ctx: &egui::Context, st: &mut SearchState) -> Option<SearchAction> {
    let mut action = None;
    egui::Window::new(widgets::semibold("Search in database", 15.0)).collapsible(false).resizable(true).default_width(460.0).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        widgets::field_label(ui, "Value");
        let r = widgets::input(ui, &mut st.value, "text to look for");
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.running {
            action = Some(SearchAction::Start);
        }
        ui.horizontal(|ui| {
            for m in MODES {
                ui.selectable_value(&mut st.op, m, m.label());
            }
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut st.scope_all, true, "Search all items");
            ui.selectable_value(&mut st.scope_all, false, "Select items");
        });
        if !st.scope_all {
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                for (t, on) in &mut st.picked {
                    ui.checkbox(on, &t.name);
                }
            });
        }
        muted(ui, "Every column of every table is read, which adds load on the server.");
        ui.horizontal(|ui| {
            if !st.running {
                if widgets::primary_button(ui, "Start search").clicked() {
                    action = Some(SearchAction::Start);
                }
            } else if st.paused {
                if widgets::primary_button(ui, "Resume").clicked() {
                    action = Some(SearchAction::Resume);
                }
            } else if widgets::secondary_button(ui, "Pause").clicked() {
                action = Some(SearchAction::Pause);
            }
            if widgets::secondary_button(ui, "Done").clicked() {
                action = Some(SearchAction::Done);
            }
            if st.running {
                ui.spinner();
            }
            if st.total > 0 {
                muted(ui, &format!("{} of {} tables", st.scanned, st.total));
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
            for (t, n) in &st.results {
                let resp = ui.add(egui::Button::new(RichText::new(format!("{}   {} matches", t.name, n)).color(theme::TEXT)).frame(false));
                if resp.clicked() {
                    action = Some(SearchAction::Open(t.clone()));
                }
            }
            if st.results.is_empty() && st.scanned > 0 && !st.running {
                muted(ui, "No matches.");
            }
            for (t, why) in &st.skipped {
                ui.label(RichText::new(format!("{} skipped: {why}", t.name)).size(12.0).color(theme::TERTIARY));
            }
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables() -> Vec<TableRef> {
        ["a", "b", "c"].iter().map(|n| TableRef::simple(n)).collect()
    }

    /// Tables are scanned one at a time; pausing stops the queue; results keep only matches.
    #[test]
    fn scanning_flow() {
        let mut s = SearchState::new(&tables());
        s.value = "x".into();
        s.start();
        assert!(s.running && s.total == 3);
        assert_eq!(s.next_table().map(|t| t.name), Some("a".into()));
        assert_eq!(s.next_table(), None, "one table at a time");
        s.record(0);
        s.paused = true;
        assert_eq!(s.next_table(), None);
        s.paused = false;
        assert_eq!(s.next_table().map(|t| t.name), Some("b".into()));
        s.record(4);
        s.next_table();
        s.skip("permission denied".into());
        assert_eq!(s.next_table(), None);
        assert!(!s.running);
        assert_eq!(s.results.len(), 1);
        assert_eq!((s.results[0].0.name.as_str(), s.results[0].1), ("b", 4));
        assert_eq!((s.scanned, s.skipped.len()), (3, 1));
    }

    /// An empty value or an empty selection does not start anything.
    #[test]
    fn nothing_to_search() {
        let mut s = SearchState::new(&tables());
        s.start();
        assert!(!s.running, "empty value");
        s.value = "x".into();
        s.scope_all = false;
        for (_, on) in &mut s.picked {
            *on = false;
        }
        s.start();
        assert!(!s.running, "no table selected");
        s.picked[1].1 = true;
        s.start();
        assert_eq!(s.total, 1);
    }

    /// The filter searches every column with the chosen mode.
    #[test]
    fn filter_uses_any_column() {
        let mut s = SearchState::new(&[]);
        s.value = "ship".into();
        s.op = FilterOp::HasPrefix;
        let f = s.filter();
        assert_eq!((f.column, f.op, f.value.as_str()), (FilterColumn::Any, FilterOp::HasPrefix, "ship"));
    }
}
