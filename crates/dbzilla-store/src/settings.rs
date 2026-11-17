//! Application preferences (SPEC 14, defaults from 14.7).

use crate::atomic::{load_json, save_json, LoadResult};
use crate::paths::AppPaths;
use crate::StoreResult;
use dbzilla_core::safemode::SafeLevel;
use serde::{Deserialize, Serialize};

/// Current settings schema version.
pub const SETTINGS_VERSION: u32 = 1;
/// Default page size of table views.
pub const DEFAULT_PAGE_SIZE: u64 = 300;
/// Default keep-alive interval.
pub const DEFAULT_KEEP_ALIVE_SECS: u64 = 30;
/// Default query timeout.
pub const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 300;
/// Default number of console entries kept.
pub const DEFAULT_CONSOLE_ENTRIES: usize = 1000;
/// Default MCP port.
pub const DEFAULT_MCP_PORT: u16 = 51003;
/// Default editor font size.
pub const DEFAULT_FONT_SIZE: f32 = 12.0;
/// Default estimate threshold (rows).
pub const DEFAULT_ESTIMATE_THRESHOLD: u64 = 100_000;

/// What `Mod+T` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NewTabKind {
    #[default]
    SqlEditor,
    Empty,
}

/// Which keys accept an autocomplete suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompleteKey {
    #[default]
    Both,
    EnterOnly,
    TabOnly,
}

/// Key bindings of the SQL editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum KeyBinding {
    #[default]
    Default,
    Vim,
}

/// Indentation character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum IndentWith {
    #[default]
    Spaces,
    Tab,
}

/// Beautifier indent type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum IndentType {
    #[default]
    Standard,
    TabularLeft,
    TabularRight,
}

pub use dbzilla_core::params::ParamFormat;

/// How results of several statements are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ResultViewMode {
    #[default]
    Tabs,
    AppendInOneTab,
}

/// CSV defaults (SPEC 14.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CsvSettings {
    pub delimiter: char,
    pub quote: char,
    pub quote_always: bool,
    pub line_break: String,
    pub decimal: char,
}

impl Default for CsvSettings {
    fn default() -> Self {
        Self { delimiter: ',', quote: '"', quote_always: false, line_break: "\n".into(), decimal: '.' }
    }
}

/// Query parameter options (SPEC 10.11).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryParamSettings {
    pub enabled: bool,
    pub allow_in_quotes: bool,
    pub format: ParamFormat,
}

/// MCP server options (SPEC 16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
    pub approval_needed: bool,
    pub auto_token_stdio: bool,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self { enabled: false, port: DEFAULT_MCP_PORT, approval_needed: true, auto_token_stdio: true }
    }
}

/// All preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub reopen_workspaces: bool,
    pub new_tab: NewTabKind,
    pub csv: CsvSettings,
    pub query_timeout_secs: u64,
    pub keep_alive: bool,
    pub keep_alive_secs: u64,
    pub page_size: u64,
    pub auto_hide_scrollers: bool,
    pub alternating_rows: bool,
    pub estimate_count: bool,
    pub estimate_threshold: u64,
    pub auto_save_queries: bool,
    pub uppercase_keywords: bool,
    pub auto_close_brackets: bool,
    pub complete_key: CompleteKey,
    pub indent_with: IndentWith,
    pub indent_width: u8,
    pub indent_type: IndentType,
    pub key_binding: KeyBinding,
    /// Custom shortcuts: action id to shortcut text (empty means Not Set). Only differences from the defaults.
    pub keymap: std::collections::BTreeMap<String, String>,
    pub query_params: QueryParamSettings,
    pub editor_font_size: f32,
    pub table_row_padding: f32,
    pub default_safe_level: SafeLevel,
    pub console_max_entries: usize,
    pub history_enabled: bool,
    pub log_original_values: bool,
    pub result_view_mode: ResultViewMode,
    pub mcp: McpSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_VERSION,
            reopen_workspaces: false,
            new_tab: NewTabKind::default(),
            csv: CsvSettings::default(),
            query_timeout_secs: DEFAULT_QUERY_TIMEOUT_SECS,
            keep_alive: true,
            keep_alive_secs: DEFAULT_KEEP_ALIVE_SECS,
            page_size: DEFAULT_PAGE_SIZE,
            auto_hide_scrollers: false,
            alternating_rows: true,
            estimate_count: true,
            estimate_threshold: DEFAULT_ESTIMATE_THRESHOLD,
            auto_save_queries: true,
            uppercase_keywords: true,
            auto_close_brackets: true,
            complete_key: CompleteKey::default(),
            indent_with: IndentWith::default(),
            indent_width: 4,
            indent_type: IndentType::default(),
            key_binding: KeyBinding::default(),
            keymap: std::collections::BTreeMap::new(),
            query_params: QueryParamSettings::default(),
            editor_font_size: DEFAULT_FONT_SIZE,
            table_row_padding: 12.0,
            default_safe_level: SafeLevel::Silent,
            console_max_entries: DEFAULT_CONSOLE_ENTRIES,
            history_enabled: true,
            log_original_values: false,
            result_view_mode: ResultViewMode::default(),
            mcp: McpSettings::default(),
        }
    }
}

impl Settings {
    /// Loads the settings, falling back to defaults with a warning when the file is unusable.
    pub fn load(paths: &AppPaths) -> LoadResult<Settings> {
        load_json(&paths.settings_file(), u64::from(SETTINGS_VERSION))
    }

    /// Saves the settings atomically.
    pub fn save(&self, paths: &AppPaths) -> StoreResult<()> {
        save_json(&paths.settings_file(), self)
    }
}
