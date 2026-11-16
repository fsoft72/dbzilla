//! Actions and keyboard shortcuts (SPEC 18.2). `Mod` is `Cmd` on macOS and `Ctrl` elsewhere.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};
use std::collections::HashMap;

/// Everything bindable to a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    OpenAnything,
    OpenDatabase,
    OpenConnection,
    NewSqlEditor,
    CloseTab,
    Commit,
    Preview,
    Discard,
    Undo,
    Redo,
    ReloadWorkspace,
    ReloadTab,
    Reconnect,
    Cancel,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleConsole,
    ToggleFilters,
    RunCurrent,
    RunAll,
    Beautify,
    ToggleComment,
    NextTab,
    PrevTab,
    Settings,
    ShowData,
    ShowStructure,
    AddRow,
    DuplicateRow,
    Export,
    FocusSidebarSearch,
}

impl Action {
    /// Every action, for the command palette and the keymap editor.
    pub const ALL: [Action; 31] = [
        Action::OpenAnything,
        Action::OpenDatabase,
        Action::OpenConnection,
        Action::NewSqlEditor,
        Action::CloseTab,
        Action::Commit,
        Action::Preview,
        Action::Discard,
        Action::Undo,
        Action::Redo,
        Action::ReloadWorkspace,
        Action::ReloadTab,
        Action::Reconnect,
        Action::Cancel,
        Action::ToggleLeftSidebar,
        Action::ToggleRightSidebar,
        Action::ToggleConsole,
        Action::ToggleFilters,
        Action::RunCurrent,
        Action::RunAll,
        Action::Beautify,
        Action::ToggleComment,
        Action::NextTab,
        Action::PrevTab,
        Action::Settings,
        Action::ShowData,
        Action::ShowStructure,
        Action::AddRow,
        Action::DuplicateRow,
        Action::Export,
        Action::FocusSidebarSearch,
    ];

    /// Name shown in menus, tooltips and the palette.
    pub fn label(self) -> &'static str {
        match self {
            Action::OpenAnything => "Open Anything",
            Action::OpenDatabase => "Open Database",
            Action::OpenConnection => "Open Connection",
            Action::NewSqlEditor => "New SQL Editor",
            Action::CloseTab => "Close Tab",
            Action::Commit => "Commit",
            Action::Preview => "Preview Changes",
            Action::Discard => "Discard Changes",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::ReloadWorkspace => "Reload Workspace",
            Action::ReloadTab => "Reload Tab",
            Action::Reconnect => "Reconnect",
            Action::Cancel => "Cancel Running Queries",
            Action::ToggleLeftSidebar => "Toggle Left Sidebar",
            Action::ToggleRightSidebar => "Toggle Right Sidebar",
            Action::ToggleConsole => "Toggle Console",
            Action::ToggleFilters => "Toggle Filters",
            Action::RunCurrent => "Run Current Statement",
            Action::RunAll => "Run All Statements",
            Action::Beautify => "Beautify SQL",
            Action::ToggleComment => "Toggle Comment",
            Action::NextTab => "Next Tab",
            Action::PrevTab => "Previous Tab",
            Action::Settings => "Settings",
            Action::ShowData => "Show Table Data",
            Action::ShowStructure => "Show Table Structure",
            Action::AddRow => "Add Row",
            Action::DuplicateRow => "Duplicate Rows",
            Action::Export => "Export Tables",
            Action::FocusSidebarSearch => "Find Items in Sidebar",
        }
    }
}

/// Maps actions to shortcuts.
#[derive(Debug, Clone)]
pub struct KeyMap {
    map: HashMap<Action, KeyboardShortcut>,
}

/// A shortcut with the platform command modifier.
fn cmd(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND, key)
}

/// A shortcut with command and shift.
fn cmd_shift(key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key)
}

impl Default for KeyMap {
    /// SPEC 18.2 defaults for the actions implemented in this version.
    fn default() -> Self {
        let mut m = HashMap::new();
        m.insert(Action::OpenAnything, cmd(Key::P));
        m.insert(Action::OpenDatabase, cmd(Key::K));
        m.insert(Action::OpenConnection, cmd_shift(Key::K));
        m.insert(Action::NewSqlEditor, cmd(Key::T));
        m.insert(Action::CloseTab, cmd(Key::W));
        m.insert(Action::Commit, cmd(Key::S));
        m.insert(Action::Preview, cmd_shift(Key::P));
        m.insert(Action::Discard, cmd_shift(Key::Delete));
        m.insert(Action::Undo, cmd(Key::Z));
        m.insert(Action::Redo, cmd_shift(Key::Z));
        m.insert(Action::ReloadWorkspace, cmd(Key::R));
        m.insert(Action::ReloadTab, KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::ALT, Key::R));
        m.insert(Action::Reconnect, cmd_shift(Key::R));
        m.insert(Action::Cancel, cmd(Key::Period));
        m.insert(Action::ToggleLeftSidebar, cmd(Key::Num0));
        m.insert(Action::ToggleConsole, KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::ALT, Key::C));
        m.insert(Action::ToggleFilters, cmd(Key::F));
        m.insert(Action::RunCurrent, cmd(Key::Enter));
        m.insert(Action::RunAll, cmd_shift(Key::Enter));
        m.insert(Action::Beautify, cmd(Key::I));
        m.insert(Action::ToggleComment, cmd(Key::Slash));
        m.insert(Action::NextTab, cmd(Key::CloseBracket));
        m.insert(Action::PrevTab, cmd(Key::OpenBracket));
        m.insert(Action::Settings, cmd(Key::Comma));
        m.insert(Action::AddRow, cmd(Key::I));
        m.insert(Action::DuplicateRow, cmd(Key::D));
        m.insert(Action::Export, cmd_shift(Key::E));
        m.insert(Action::FocusSidebarSearch, cmd_shift(Key::F));
        m.insert(Action::ShowData, KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::ALT, Key::OpenBracket));
        m.insert(Action::ShowStructure, KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::ALT, Key::CloseBracket));
        Self { map: m }
    }
}

impl KeyMap {
    /// Shortcut of an action.
    pub fn shortcut(&self, a: Action) -> Option<KeyboardShortcut> {
        self.map.get(&a).copied()
    }

    /// Human readable shortcut text for tooltips, or an empty string.
    pub fn hint(&self, a: Action) -> String {
        self.shortcut(a).map(|s| format_shortcut(&s)).unwrap_or_default()
    }

    /// Rebinds an action; refuses a shortcut that another action already uses in the same context.
    pub fn rebind(&mut self, a: Action, s: KeyboardShortcut) -> Result<(), String> {
        if let Some(other) = self.conflict(a, &s) {
            return Err(format!("{} already uses this shortcut", other.label()));
        }
        self.map.insert(a, s);
        Ok(())
    }

    /// Another action using `s` in an overlapping context, if any.
    pub fn conflict(&self, a: Action, s: &KeyboardShortcut) -> Option<Action> {
        self.map.iter().find(|(other, sc)| **other != a && *sc == s && context(**other) == context(a)).map(|(o, _)| *o)
    }

    /// Actions triggered this frame (consuming the key presses).
    pub fn triggered(&self, ctx: &eframe::egui::Context) -> Vec<Action> {
        // longer modifier combinations first, so Cmd+Shift+P wins over Cmd+P
        let mut sorted: Vec<(&Action, &KeyboardShortcut)> = self.map.iter().collect();
        sorted.sort_by_key(|(_, s)| std::cmp::Reverse(modifier_count(&s.modifiers)));
        ctx.input_mut(|i| sorted.into_iter().filter(|(_, s)| i.consume_shortcut(s)).map(|(a, _)| *a).collect())
    }
}

/// Shortcut context: bindings in different contexts may share keys.
fn context(a: Action) -> u8 {
    match a {
        Action::Beautify | Action::ToggleComment | Action::RunCurrent | Action::RunAll => 1,
        Action::AddRow | Action::DuplicateRow => 2,
        _ => 0,
    }
}

/// Number of modifier keys held in a combination.
fn modifier_count(m: &Modifiers) -> usize {
    [m.ctrl, m.alt, m.shift, m.command, m.mac_cmd].iter().filter(|b| **b).count()
}

/// Formats a shortcut like `Ctrl+Shift+P`.
pub fn format_shortcut(s: &KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if s.modifiers.command {
        parts.push(if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }.to_owned());
    }
    if s.modifiers.alt {
        parts.push("Alt".into());
    }
    if s.modifiers.shift {
        parts.push("Shift".into());
    }
    parts.push(s.logical_key.name().to_owned());
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No two actions of the same context share a default shortcut.
    #[test]
    fn defaults_have_no_conflicts() {
        let km = KeyMap::default();
        for a in Action::ALL {
            if let Some(s) = km.shortcut(a) {
                assert_eq!(km.conflict(a, &s), None, "{a:?}");
            }
        }
    }

    /// Rebinding onto a used shortcut is refused and leaves the map unchanged.
    #[test]
    fn rebind_refuses_conflicts() {
        let mut km = KeyMap::default();
        let taken = km.shortcut(Action::Commit).unwrap();
        assert!(km.rebind(Action::Preview, taken).is_err());
        assert_ne!(km.shortcut(Action::Preview), Some(taken));
        assert!(km.rebind(Action::Preview, cmd(Key::J)).is_ok());
    }

    /// Editor and grid shortcuts may reuse a key of another context (Mod+I).
    #[test]
    fn contexts_may_share_keys() {
        let km = KeyMap::default();
        assert_eq!(km.shortcut(Action::Beautify), km.shortcut(Action::AddRow));
    }

    /// Shortcut text uses the platform command name.
    #[test]
    fn formatting() {
        let s = cmd_shift(Key::P);
        let text = format_shortcut(&s);
        assert!(text.ends_with("Shift+P"), "{text}");
    }
}
