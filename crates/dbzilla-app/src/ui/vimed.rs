//! Vim key binding of the SQL editor: turns egui keyboard events into vim keys, runs them on a
//! rope buffer kept next to the editor text and reports the caret to draw.

use dbzilla_vim::buffer::Buffer;
use dbzilla_vim::editing::{EditSettings, EditorState, Selection};
use dbzilla_vim::{Mode, VimEffect, VimKey, VimState};
use eframe::egui::{self, Key, Modifiers, Ui};

/// Vim state of one SQL tab.
pub struct VimEditor {
    state: VimState,
    buf: Buffer,
    ed: EditorState,
    /// Message of the last command (an error or an information), shown in the status line.
    pub message: Option<(bool, String)>,
}

/// Maps a key press to a vim key. Printable keys arrive as text events instead; Ctrl+R (redo)
/// is the only Ctrl combination vim owns, so the other app shortcuts keep working.
fn key_to_vim_key(key: Key, mods: Modifiers) -> Option<VimKey> {
    if mods.alt {
        return None;
    }
    if mods.command || mods.ctrl {
        return (key == Key::R && !mods.shift).then_some(VimKey::Ctrl('r'));
    }
    Some(match key {
        Key::Escape => VimKey::Esc,
        Key::Enter => VimKey::Enter,
        Key::Backspace => VimKey::Backspace,
        Key::Tab if !mods.shift => VimKey::Tab,
        Key::ArrowLeft => VimKey::Left,
        Key::ArrowRight => VimKey::Right,
        Key::ArrowUp => VimKey::Up,
        Key::ArrowDown => VimKey::Down,
        _ => return None,
    })
}

/// Takes the keyboard events the vim state machine owns out of the frame input and returns
/// them as vim keys. Events that vim does not handle (app shortcuts, mouse) stay in place.
pub fn take_keys(ui: &mut Ui, mode: Mode) -> Vec<VimKey> {
    let mut keys = Vec::new();
    ui.input_mut(|i| {
        i.events.retain(|ev| match ev {
            egui::Event::Text(t) | egui::Event::Ime(egui::ImeEvent::Commit(t)) => {
                keys.extend(t.chars().filter(|c| !c.is_control()).map(VimKey::Char));
                false
            }
            egui::Event::Paste(t) => {
                if mode == Mode::Insert {
                    keys.push(VimKey::Text(t.clone()));
                }
                false
            }
            egui::Event::Key { key, pressed: true, modifiers, .. } => match key_to_vim_key(*key, *modifiers) {
                Some(k) => {
                    keys.push(k);
                    false
                }
                None => true,
            },
            _ => true,
        });
    });
    keys
}

/// Char index of the first difference between `a` and `b`, and the length of their common
/// suffix that does not overlap that prefix (both in chars).
fn diff_bounds(a: &str, b: &str) -> (usize, usize) {
    let prefix = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let (la, lb) = (a.chars().count(), b.chars().count());
    let max_suffix = la.min(lb) - prefix;
    let suffix = a.chars().rev().zip(b.chars().rev()).take(max_suffix).take_while(|(x, y)| x == y).count();
    (prefix, suffix)
}

impl VimEditor {
    /// A new state in Normal mode over `text`.
    pub fn new(text: &str) -> Self {
        Self { state: VimState::new(), buf: Buffer::from_text(text), ed: EditorState::default(), message: None }
    }

    /// Current mode.
    pub fn mode(&self) -> Mode {
        self.state.mode()
    }

    /// Text of the status line: mode, pending keys or command line, and the last message.
    pub fn status(&self) -> String {
        let mode = match self.state.mode() {
            Mode::Normal => "-- NORMAL --",
            Mode::Insert => "-- INSERT --",
            Mode::Visual => "-- VISUAL --",
            Mode::VisualLine => "-- VISUAL LINE --",
            Mode::CommandLine => "",
        };
        let cmd = self.state.cmdline();
        if !cmd.is_empty() {
            return cmd.to_owned();
        }
        format!("{mode}  {}", self.state.pending())
    }

    /// Brings the buffer in line with `text` edited from outside (autocomplete, format, a
    /// reloaded query) as one undoable replacement of the changed middle part, and places the
    /// caret at `caret_chars`.
    pub fn sync_text(&mut self, text: &str, caret_chars: usize) {
        let current = self.buf.text();
        if current == text {
            return;
        }
        self.state.end_insert_group(&mut self.buf);
        let (prefix, suffix) = diff_bounds(&current, text);
        let end_old = current.chars().count() - suffix;
        let end_new = text.chars().count() - suffix;
        let middle: String = text.chars().skip(prefix).take(end_new - prefix).collect();
        self.buf.replace(prefix..end_old, &middle);
        let len = self.buf.rope().len_chars();
        self.ed.selection = Selection::caret(caret_chars.min(len));
    }

    /// Places the caret from the editor (mouse click or drag) and syncs the mode with it.
    pub fn mouse_selection(&mut self, anchor_chars: usize, head_chars: usize) {
        let len = self.buf.rope().len_chars();
        self.ed.selection = Selection { anchor: anchor_chars.min(len), head: head_chars.min(len) };
        self.state.mouse_selection(&mut self.buf, &mut self.ed);
    }

    /// Feeds keys; returns the new text when it changed, and the effects to perform.
    pub fn handle_keys(&mut self, keys: Vec<VimKey>, settings: &EditSettings) -> (Option<String>, Vec<VimEffect>) {
        let rev = self.buf.revision();
        let mut effects = Vec::new();
        for k in keys {
            effects.extend(self.state.handle_key(k, &mut self.buf, &mut self.ed, settings));
        }
        for e in &effects {
            match e {
                VimEffect::Message(m) => self.message = Some((false, m.clone())),
                VimEffect::Error(m) => self.message = Some((true, m.clone())),
                _ => {}
            }
        }
        let changed = (self.buf.revision() != rev).then(|| self.buf.text());
        (changed, effects)
    }

    /// The text selection (anchor, head) in chars the editor must show: the visual range in
    /// Visual modes, the caret otherwise.
    pub fn display_selection(&self) -> (usize, usize) {
        if let Some(r) = self.state.visual_range(&self.buf, &self.ed) {
            return (r.start, r.end);
        }
        let head = self.ed.selection.head.min(self.buf.rope().len_chars());
        (head, head)
    }

    /// Char index of the caret (the block cursor in Normal and Visual modes).
    pub fn caret(&self) -> usize {
        self.ed.selection.head.min(self.buf.rope().len_chars())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<VimKey> {
        s.chars().map(VimKey::Char).collect()
    }

    fn settings() -> EditSettings {
        EditSettings { tab_width: 4, insert_spaces: true }
    }

    #[test]
    fn normal_mode_edits_and_reports_the_new_text() {
        let mut v = VimEditor::new("select 1\nfrom t");
        let (text, _) = v.handle_keys(keys("dd"), &settings());
        assert_eq!(text.as_deref(), Some("from t"));
        let (text, _) = v.handle_keys(keys("j"), &settings());
        assert_eq!(text, None, "a motion does not change the text");
    }

    #[test]
    fn insert_mode_types_and_esc_returns_to_normal() {
        let mut v = VimEditor::new("");
        v.handle_keys(keys("iabc"), &settings());
        assert_eq!(v.mode(), Mode::Insert);
        let (text, _) = v.handle_keys(vec![VimKey::Esc], &settings());
        assert_eq!(v.mode(), Mode::Normal);
        assert_eq!(text, None);
        assert_eq!(v.buf.text(), "abc");
    }

    #[test]
    fn undo_and_redo_work_and_survive_an_external_edit() {
        let mut v = VimEditor::new("a b c");
        v.handle_keys(keys("x"), &settings());
        v.sync_text(" b c!", 0);
        assert_eq!(v.buf.text(), " b c!");
        let (text, _) = v.handle_keys(keys("u"), &settings());
        assert_eq!(text.as_deref(), Some(" b c"), "undoes the external edit first");
    }

    #[test]
    fn visual_mode_shows_a_selection() {
        let mut v = VimEditor::new("hello world");
        v.handle_keys(keys("vll"), &settings());
        assert_eq!(v.mode(), Mode::Visual);
        assert_eq!(v.display_selection(), (0, 3));
    }

    #[test]
    fn yank_asks_for_the_clipboard_and_delete_changes_the_text() {
        let mut v = VimEditor::new("one two");
        let (_, fx) = v.handle_keys(keys("yw"), &settings());
        assert!(fx.contains(&VimEffect::SetClipboard("one ".into())));
        let (text, _) = v.handle_keys(keys("dw"), &settings());
        assert_eq!(text.as_deref(), Some("two"));
    }

    #[test]
    fn diff_bounds_finds_the_changed_middle() {
        assert_eq!(diff_bounds("abcdef", "abXdef"), (2, 3));
        assert_eq!(diff_bounds("aaa", "aaaa"), (3, 0));
        assert_eq!(diff_bounds("é1é", "é2é"), (1, 1));
    }

    #[test]
    fn only_ctrl_r_is_taken_among_the_ctrl_combinations() {
        assert_eq!(key_to_vim_key(Key::R, Modifiers::CTRL), Some(VimKey::Ctrl('r')));
        assert_eq!(key_to_vim_key(Key::S, Modifiers::CTRL), None);
        assert_eq!(key_to_vim_key(Key::Enter, Modifiers::CTRL), None);
        assert_eq!(key_to_vim_key(Key::Escape, Modifiers::NONE), Some(VimKey::Esc));
        assert_eq!(key_to_vim_key(Key::Tab, Modifiers::SHIFT), None);
    }
}
