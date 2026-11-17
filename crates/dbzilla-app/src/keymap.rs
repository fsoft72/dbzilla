//! Actions and keyboard shortcuts (SPEC 18.2). `Mod` is `Cmd` on macOS and `Ctrl` elsewhere.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};
use std::collections::{BTreeMap, HashMap};

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
    SearchInDatabase,
    ShowDiagram,
    ShowMetrics,
    ProcessList,
    Backup,
    Restore,
    Users,
}

impl Action {
    /// Every action, for the command palette and the keymap editor.
    pub const ALL: [Action; 38] = [
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
        Action::SearchInDatabase,
        Action::ShowDiagram,
        Action::ShowMetrics,
        Action::ProcessList,
        Action::Backup,
        Action::Restore,
        Action::Users,
    ];

    /// Stable identifier used to store a custom shortcut in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Action::OpenAnything => "open_anything",
            Action::OpenDatabase => "open_database",
            Action::OpenConnection => "open_connection",
            Action::NewSqlEditor => "new_sql_editor",
            Action::CloseTab => "close_tab",
            Action::Commit => "commit",
            Action::Preview => "preview",
            Action::Discard => "discard",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::ReloadWorkspace => "reload_workspace",
            Action::ReloadTab => "reload_tab",
            Action::Reconnect => "reconnect",
            Action::Cancel => "cancel",
            Action::ToggleLeftSidebar => "toggle_left_sidebar",
            Action::ToggleRightSidebar => "toggle_right_sidebar",
            Action::ToggleConsole => "toggle_console",
            Action::ToggleFilters => "toggle_filters",
            Action::RunCurrent => "run_current",
            Action::RunAll => "run_all",
            Action::Beautify => "beautify",
            Action::ToggleComment => "toggle_comment",
            Action::NextTab => "next_tab",
            Action::PrevTab => "prev_tab",
            Action::Settings => "settings",
            Action::ShowData => "show_data",
            Action::ShowStructure => "show_structure",
            Action::AddRow => "add_row",
            Action::DuplicateRow => "duplicate_row",
            Action::Export => "export",
            Action::FocusSidebarSearch => "focus_sidebar_search",
            Action::SearchInDatabase => "search_in_database",
            Action::ShowDiagram => "show_diagram",
            Action::ShowMetrics => "show_metrics",
            Action::ProcessList => "process_list",
            Action::Backup => "backup",
            Action::Restore => "restore",
            Action::Users => "users",
        }
    }

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
            Action::SearchInDatabase => "Search in Database",
            Action::ShowDiagram => "Show Diagram",
            Action::ShowMetrics => "Show Metrics Board",
            Action::ProcessList => "Process List",
            Action::Backup => "Backup Database",
            Action::Restore => "Restore Database",
            Action::Users => "User Management",
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

    /// Rebinds an action; refuses an invalid shortcut or one that another action already uses in an overlapping context.
    pub fn rebind(&mut self, a: Action, s: KeyboardShortcut) -> Result<(), String> {
        validate(&s)?;
        if let Some(other) = self.conflict(a, &s) {
            return Err(format!("{} already uses this shortcut", other.label()));
        }
        self.map.insert(a, s);
        Ok(())
    }

    /// Another action using `s` in an overlapping context, if any.
    pub fn conflict(&self, a: Action, s: &KeyboardShortcut) -> Option<Action> {
        self.map.iter().find(|(other, sc)| **other != a && *sc == s && contexts_overlap(**other, a)).map(|(o, _)| *o)
    }

    /// Removes the shortcut of an action (`Not Set`).
    pub fn clear(&mut self, a: Action) {
        self.map.remove(&a);
    }

    /// Default shortcut of an action, if it has one.
    pub fn default_shortcut(a: Action) -> Option<KeyboardShortcut> {
        KeyMap::default().shortcut(a)
    }

    /// True when the action uses its default shortcut (or none, when it has no default).
    pub fn is_default(&self, a: Action) -> bool {
        self.shortcut(a) == Self::default_shortcut(a)
    }

    /// Puts the default shortcut of one action back.
    pub fn reset(&mut self, a: Action) {
        match Self::default_shortcut(a) {
            Some(s) => self.map.insert(a, s),
            None => self.map.remove(&a),
        };
    }

    /// Puts every default shortcut back.
    pub fn restore_defaults(&mut self) {
        *self = KeyMap::default();
    }

    /// Builds the map from the defaults plus the custom shortcuts stored in the settings.
    /// Unknown actions, unparsable shortcuts and conflicting ones are skipped, so a hand edited file never breaks the keys.
    pub fn from_overrides(overrides: &BTreeMap<String, String>) -> Self {
        let mut km = KeyMap::default();
        for a in Action::ALL {
            let Some(text) = overrides.get(a.id()) else { continue };
            if text.is_empty() {
                km.clear(a);
            } else if let Some(s) = decode_shortcut(text) {
                // a conflicting stored shortcut is ignored: the default stays
                let _ = km.rebind(a, s);
            }
        }
        km
    }

    /// The custom shortcuts to store: only what differs from the defaults (an empty text means `Not Set`).
    pub fn overrides(&self) -> BTreeMap<String, String> {
        Action::ALL.into_iter().filter(|a| !self.is_default(*a)).map(|a| (a.id().to_owned(), self.shortcut(a).map(|s| encode_shortcut(&s)).unwrap_or_default())).collect()
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

/// True when two actions can be active at the same time: a global action overlaps every context.
fn contexts_overlap(a: Action, b: Action) -> bool {
    let (ca, cb) = (context(a), context(b));
    ca == cb || ca == 0 || cb == 0
}

/// Checks the SPEC 14.5 rule: a shortcut needs `Mod` or `Ctrl`, unless it is a function key.
pub fn validate(s: &KeyboardShortcut) -> Result<(), String> {
    if s.modifiers.command || s.modifiers.ctrl || is_function_key(s.logical_key) {
        return Ok(());
    }
    Err("A shortcut must include Cmd/Ctrl, unless it is a function key".into())
}

/// True for F1 to F35.
fn is_function_key(k: Key) -> bool {
    let n = k.name();
    n.len() > 1 && n.starts_with('F') && n[1..].chars().all(|c| c.is_ascii_digit())
}

/// Reduces the modifiers of a key press to the form used in the keymap: `command` is the platform command key.
pub fn normalize_modifiers(m: Modifiers) -> Modifiers {
    let mac = cfg!(target_os = "macos");
    Modifiers { alt: m.alt, shift: m.shift, command: m.command || (mac && m.mac_cmd) || (!mac && m.ctrl), ctrl: mac && m.ctrl, mac_cmd: false }
}

/// Text stored in the settings file, like `Mod+Shift+P`.
pub fn encode_shortcut(s: &KeyboardShortcut) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if s.modifiers.command {
        parts.push("Mod");
    }
    if s.modifiers.ctrl {
        parts.push("Ctrl");
    }
    if s.modifiers.alt {
        parts.push("Alt");
    }
    if s.modifiers.shift {
        parts.push("Shift");
    }
    parts.push(s.logical_key.name());
    parts.join("+")
}

/// Reads the text written by [`encode_shortcut`]; `None` when it is not a valid shortcut.
pub fn decode_shortcut(text: &str) -> Option<KeyboardShortcut> {
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = Key::from_name(parts.pop()?)?;
    let mut m = Modifiers::NONE;
    for p in parts {
        match p {
            "Mod" => m.command = true,
            "Ctrl" if cfg!(target_os = "macos") => m.ctrl = true,
            "Ctrl" => m.command = true,
            "Alt" => m.alt = true,
            "Shift" => m.shift = true,
            _ => return None,
        }
    }
    Some(KeyboardShortcut::new(m, key))
}

/// Number of modifier keys held in a combination.
fn modifier_count(m: &Modifiers) -> usize {
    [m.ctrl, m.alt, m.shift, m.command, m.mac_cmd].iter().filter(|b| **b).count()
}

/// Formats a shortcut like `Ctrl+Shift+P`.
pub fn format_shortcut(s: &KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if s.modifiers.ctrl {
        parts.push("Ctrl".to_owned());
    }
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

    /// Every default shortcut survives the settings text form.
    #[test]
    fn shortcut_text_round_trips() {
        let km = KeyMap::default();
        for a in Action::ALL {
            if let Some(s) = km.shortcut(a) {
                assert_eq!(decode_shortcut(&encode_shortcut(&s)), Some(s), "{a:?}");
            }
        }
        assert_eq!(decode_shortcut("Nope+P"), None);
        assert_eq!(decode_shortcut("Mod+NotAKey"), None);
    }

    /// Action ids are unique, because they are the keys of the stored map.
    #[test]
    fn action_ids_are_unique() {
        let ids: std::collections::HashSet<_> = Action::ALL.iter().map(|a| a.id()).collect();
        assert_eq!(ids.len(), Action::ALL.len());
    }

    /// Without changes nothing is stored; custom and cleared shortcuts come back after a reload.
    #[test]
    fn overrides_round_trip() {
        let mut km = KeyMap::default();
        assert!(km.overrides().is_empty());
        km.rebind(Action::Preview, cmd(Key::J)).unwrap();
        km.clear(Action::Cancel);
        let stored = km.overrides();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored["cancel"], "");
        let back = KeyMap::from_overrides(&stored);
        assert_eq!(back.shortcut(Action::Preview), Some(cmd(Key::J)));
        assert_eq!(back.shortcut(Action::Cancel), None);
    }

    /// A damaged or conflicting stored shortcut leaves the default in place.
    #[test]
    fn bad_overrides_are_ignored() {
        let mut o = BTreeMap::new();
        o.insert("preview".to_owned(), "garbage".to_owned());
        o.insert("commit".to_owned(), encode_shortcut(&cmd(Key::P))); // taken by Open Anything
        o.insert("no_such_action".to_owned(), "Mod+J".to_owned());
        let km = KeyMap::from_overrides(&o);
        assert_eq!(km.shortcut(Action::Preview), KeyMap::default_shortcut(Action::Preview));
        assert_eq!(km.shortcut(Action::Commit), KeyMap::default_shortcut(Action::Commit));
    }

    /// A shortcut needs Mod or Ctrl, except function keys.
    #[test]
    fn validation_rule() {
        assert!(validate(&KeyboardShortcut::new(Modifiers::NONE, Key::A)).is_err());
        assert!(validate(&KeyboardShortcut::new(Modifiers::SHIFT, Key::A)).is_err());
        assert!(validate(&KeyboardShortcut::new(Modifiers::NONE, Key::F5)).is_ok());
        assert!(validate(&cmd(Key::A)).is_ok());
        let mut km = KeyMap::default();
        assert!(km.rebind(Action::Preview, KeyboardShortcut::new(Modifiers::NONE, Key::A)).is_err());
    }

    /// Reset and Restore Defaults put the defaults back, also for a cleared shortcut.
    #[test]
    fn reset_and_restore() {
        let mut km = KeyMap::default();
        km.clear(Action::Commit);
        assert!(!km.is_default(Action::Commit));
        km.reset(Action::Commit);
        assert!(km.is_default(Action::Commit));
        km.rebind(Action::Preview, cmd(Key::J)).unwrap();
        km.restore_defaults();
        assert!(km.overrides().is_empty());
    }

    /// A global action cannot reuse the key of an editor or grid action.
    #[test]
    fn global_overlaps_contexts() {
        let mut km = KeyMap::default();
        assert!(km.rebind(Action::Preview, cmd(Key::D)).is_err()); // Duplicate Rows
        assert!(km.rebind(Action::Beautify, cmd(Key::F)).is_err()); // Toggle Filters is global
    }
}
