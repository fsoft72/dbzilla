//! Rope-backed text buffer with grouped undo/redo and marks.

use std::collections::VecDeque;
use std::ops::Range;

use ropey::Rope;

/// Most undo steps kept; older ones are dropped.
const MAX_UNDO_STEPS: usize = 1000;

/// One primitive text replacement, stored so it can be reverted.
#[derive(Clone, Debug, Default)]
struct Edit {
    /// Char index where the replacement happened.
    pos: usize,
    /// Text that was removed.
    removed: String,
    /// Text that was inserted.
    inserted: String,
}

/// A rope-backed text buffer. The rope holds LF only (CRLF and lone CR become LF).
pub struct Buffer {
    rope: Rope,
    revision: u64,
    history: VecDeque<Vec<Edit>>,
    /// Number of history steps currently applied.
    position: usize,
    /// History position of the last `mark_saved`, if still reachable.
    saved_position: Option<usize>,
    group_depth: usize,
    group_edits: Vec<Edit>,
    marks: Vec<(char, usize)>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

/// Converts CRLF and lone CR to LF.
pub fn normalize_newlines(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\r') {
        return text.into();
    }
    text.replace("\r\n", "\n").replace('\r', "\n").into()
}

impl Buffer {
    /// Creates a buffer from `text`.
    pub fn from_text(text: &str) -> Buffer {
        Buffer {
            rope: Rope::from_str(&normalize_newlines(text)),
            revision: 0,
            history: VecDeque::new(),
            position: 0,
            saved_position: Some(0),
            group_depth: 0,
            group_edits: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Returns the underlying rope.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Returns the whole content as a `String`.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Returns a counter incremented on every content change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns true when the content differs from the last `mark_saved` state.
    pub fn is_modified(&self) -> bool {
        self.saved_position != Some(self.position) || !self.group_edits.is_empty()
    }

    /// Remembers the current state as the saved one.
    pub fn mark_saved(&mut self) {
        self.saved_position = Some(self.position);
    }

    /// Replaces the char `range` with `text` and records an undo step.
    /// Insert uses an empty range, delete uses empty text.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let len = self.rope.len_chars();
        let start = range.start.min(len);
        let range = start..range.end.min(len).max(start);
        if range.is_empty() && text.is_empty() {
            return;
        }
        let edit = self.apply(range, &normalize_newlines(text));
        if self.group_depth > 0 {
            self.group_edits.push(edit);
            return;
        }
        self.push_step(vec![edit]);
    }

    /// Starts (or nests) an undo group; edits until the matching `end_group` form one step.
    pub fn begin_group(&mut self) {
        self.group_depth += 1;
    }

    /// Ends an undo group; the outermost end commits the merged step.
    pub fn end_group(&mut self) {
        if self.group_depth == 0 {
            return;
        }
        self.group_depth -= 1;
        if self.group_depth > 0 || self.group_edits.is_empty() {
            return;
        }
        let edits = std::mem::take(&mut self.group_edits);
        self.push_step(edits);
    }

    /// Replaces each char range of `edits` with its text, in the given order (pass them from
    /// the end of the text backwards), as one undo step or as part of the open group.
    pub fn replace_many(&mut self, edits: Vec<(Range<usize>, String)>) {
        self.begin_group();
        for (range, text) in edits {
            self.replace(range, &text);
        }
        self.end_group();
    }

    /// Undoes the last step; returns the char index for the cursor.
    pub fn undo(&mut self) -> Option<usize> {
        if self.position == 0 {
            return None;
        }
        self.position -= 1;
        let step = std::mem::take(&mut self.history[self.position]);
        let mut cursor = 0;
        for edit in step.iter().rev() {
            let end = edit.pos + edit.inserted.chars().count();
            self.apply(edit.pos..end, &edit.removed);
            cursor = edit.pos + edit.removed.chars().count();
        }
        self.history[self.position] = step;
        Some(cursor)
    }

    /// Redoes the next step; returns the char index for the cursor.
    pub fn redo(&mut self) -> Option<usize> {
        if self.position >= self.history.len() {
            return None;
        }
        let step = std::mem::take(&mut self.history[self.position]);
        let mut cursor = 0;
        for edit in &step {
            let end = edit.pos + edit.removed.chars().count();
            self.apply(edit.pos..end, &edit.inserted);
            cursor = edit.pos + edit.inserted.chars().count();
        }
        self.history[self.position] = step;
        self.position += 1;
        Some(cursor)
    }

    /// Sets mark `name` at char index `pos`.
    pub fn set_mark(&mut self, name: char, pos: usize) {
        match self.marks.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = pos,
            None => self.marks.push((name, pos)),
        }
    }

    /// Returns the char index of mark `name`.
    pub fn mark(&self, name: char) -> Option<usize> {
        self.marks.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
    }

    /// Pushes a finished step, dropping any redo tail and the oldest steps over the limit.
    fn push_step(&mut self, edits: Vec<Edit>) {
        self.history.truncate(self.position);
        if self.saved_position.is_some_and(|s| s > self.position) {
            self.saved_position = None;
        }
        self.history.push_back(edits);
        self.position += 1;
        while self.history.len() > MAX_UNDO_STEPS {
            self.history.pop_front();
            self.position -= 1;
            self.saved_position = self.saved_position.and_then(|s| s.checked_sub(1));
        }
    }

    /// Replaces `range` with `text` in the rope and updates revision and marks.
    fn apply(&mut self, range: Range<usize>, text: &str) -> Edit {
        let removed = self.rope.slice(range.clone()).to_string();
        let new_len = text.chars().count();
        let old_len = range.len();
        self.rope.remove(range.clone());
        self.rope.insert(range.start, text);
        for (_, pos) in &mut self.marks {
            if *pos >= range.end {
                *pos = *pos + new_len - old_len;
            } else if *pos > range.start {
                *pos = range.start;
            }
        }
        self.revision += 1;
        Edit { pos: range.start, removed, inserted: text.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_edits_undo_as_one_step() {
        let mut b = Buffer::from_text("abc");
        b.begin_group();
        b.replace(0..0, "x");
        b.replace(4..4, "y");
        b.end_group();
        assert_eq!(b.text(), "xabcy");
        assert_eq!(b.undo(), Some(0));
        assert_eq!(b.text(), "abc");
        assert!(b.redo().is_some());
        assert_eq!(b.text(), "xabcy");
    }

    #[test]
    fn crlf_is_normalised_and_modified_flag_follows_undo() {
        let mut b = Buffer::from_text("a\r\nb");
        assert_eq!(b.text(), "a\nb");
        assert!(!b.is_modified());
        b.replace(0..1, "z");
        assert!(b.is_modified());
        b.undo();
        assert!(!b.is_modified());
    }

    #[test]
    fn marks_follow_edits() {
        let mut b = Buffer::from_text("hello");
        b.set_mark('a', 4);
        b.replace(0..0, "xx");
        assert_eq!(b.mark('a'), Some(6));
    }
}
