//! The console log: every statement sent to the server (SPEC 7.5).

use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use eframe::egui::{self, FontId, RichText, Ui};
use std::collections::VecDeque;

/// One logged statement.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleEntry {
    pub ts_ms: i64,
    pub sql: String,
}

/// Bounded log of statements.
#[derive(Debug, Clone)]
pub struct ConsoleLog {
    entries: VecDeque<ConsoleEntry>,
    max: usize,
}

impl ConsoleLog {
    /// Log keeping at most `max` entries.
    pub fn new(max: usize) -> Self {
        Self { entries: VecDeque::new(), max: max.max(1) }
    }

    /// Appends an entry, dropping the oldest ones above the limit.
    pub fn push(&mut self, ts_ms: i64, sql: String) {
        self.entries.push_back(ConsoleEntry { ts_ms, sql });
        while self.entries.len() > self.max {
            self.entries.pop_front();
        }
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is logged.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &ConsoleEntry> {
        self.entries.iter()
    }
}

/// `HH:MM:SS` of a Unix millisecond timestamp (UTC).
pub fn clock(ts_ms: i64) -> String {
    let secs = ts_ms.div_euclid(1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Draws the console panel; returns true when "Clear" was pressed.
pub fn show(ui: &mut Ui, log: &ConsoleLog) -> bool {
    let mut cleared = false;
    ui.horizontal(|ui| {
        ui.label(widgets::semibold("CONSOLE", 11.0).color(theme::LABEL));
        widgets::muted(ui, &format!("{} entries", log.len()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::ico_button(ui, Ico::Trash, "Clear the console").clicked() {
                cleared = true;
            }
        });
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for e in log.iter() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(clock(e.ts_ms)).font(FontId::monospace(12.0)).color(theme::MUTED));
                ui.label(RichText::new(e.sql.replace('\n', " ")).font(FontId::monospace(12.0)).color(theme::TEXT));
            });
        }
    });
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log keeps only the newest entries.
    #[test]
    fn bounded() {
        let mut l = ConsoleLog::new(3);
        for i in 0..5 {
            l.push(i, format!("SELECT {i}"));
        }
        assert_eq!(l.len(), 3);
        assert_eq!(l.iter().next().unwrap().sql, "SELECT 2");
        l.clear();
        assert!(l.is_empty());
    }

    /// Clock formatting wraps at midnight and handles old dates.
    #[test]
    fn clock_format() {
        assert_eq!(clock(0), "00:00:00");
        assert_eq!(clock(3_661_000), "01:01:01");
        assert_eq!(clock(86_400_000 + 5_000), "00:00:05");
    }
}
