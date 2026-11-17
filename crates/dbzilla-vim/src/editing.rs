//! Selection and per-editor state used by the vim key handlers.

use std::ops::Range;

/// A selection with an anchor and a head (the caret); both are char indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    /// Fixed end of the selection.
    pub anchor: usize,
    /// Moving end of the selection, where the caret is drawn.
    pub head: usize,
}

impl Selection {
    /// Returns an empty selection (a caret) at `pos`.
    pub fn caret(pos: usize) -> Selection {
        Selection { anchor: pos, head: pos }
    }

    /// Returns the selected char range in ascending order.
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Returns true when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Per-editor state.
#[derive(Clone, Debug, Default)]
pub struct EditorState {
    /// Current selection (caret when empty).
    pub selection: Selection,
    /// Column to restore on vertical moves, `None` when the last command was not vertical.
    pub preferred_col: Option<usize>,
}

/// Indentation settings used by the indent operators.
#[derive(Clone, Copy, Debug)]
pub struct EditSettings {
    /// Width of a tab stop in columns.
    pub tab_width: usize,
    /// Insert spaces instead of a tab char.
    pub insert_spaces: bool,
}
