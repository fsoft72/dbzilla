//! An open connection: session, sidebar, tabs, console and the workspace screen (SPEC 7).

use super::console::{self, ConsoleLog};
use super::datatab::{self, DataAction, DataEnv, DataState, Total};
use super::details::{self, DetailsAction, DetailsState};
use super::pending::default_key_columns;
use super::palette::{self, PaletteChoice, PaletteItem, PaletteState};
use super::sidebar::{self, SidebarAction, SidebarState, SidebarTab};
use super::sqltab::{self, ResStatus, ResultTab, SqlAction, SqlEnv, SqlTab};
use super::tabs::{decide_open, OpenDecision, OpenMode, TabId, TabMeta, TabTarget};
use super::widgets;
use super::Toasts;
use crate::glyphs::Ico;
use crate::keymap::{Action, KeyMap};
use crate::sqlhl;
use crate::sqltext;
use crate::theme;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::changes::generate_sql;
use dbzilla_core::copyfmt::CsvOptions;
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::filter::Filter;
use dbzilla_core::rowlimit::apply_row_limit;
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef, TableStructure};
use dbzilla_drivers::DatabaseInfo;
use dbzilla_session::gate::{gate_check, verify_password, GateDecision};
use dbzilla_session::{Command, Event, PageRequest, RequestId, SessionHandle};
use dbzilla_store::connections::StoredConnection;
use dbzilla_store::settings::Settings;
use dbzilla_store::tags::TagStore;
use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Stroke};
use egui_dock::{DockArea, DockState, Style, TabViewer};
use std::collections::{HashMap, HashSet};

/// Connection state shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum WsStatus {
    Connecting,
    Connected(String),
    Failed(String),
    Disconnected(String),
}

/// Content of a table tab.
pub struct TableTab {
    pub table: TableRef,
    pub structure: Option<TableStructure>,
    pub error: Option<String>,
    pub data: DataState,
    pub is_view: bool,
}

/// What a request of a tab is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReqKind {
    Page,
    Count,
    Commit,
    Sql,
}

/// What a confirmed gate does.
enum GateAction {
    Commit,
    Run { labels: Vec<Option<String>> },
}

/// A statement list waiting for the safe-mode answer.
struct GateModal {
    action: GateAction,
    statements: Vec<String>,
    needs_password: bool,
    input: String,
    reveal: bool,
    tab: TabId,
    error: Option<String>,
}

/// Dialog choosing the key columns of a table without a primary key.
struct KeyDialog {
    tab: TabId,
    names: Vec<String>,
    selected: Vec<bool>,
}

/// What a tab holds.
pub enum TabKind {
    Table(Box<TableTab>),
    Sql(Box<SqlTab>),
}

/// A tab of the dock.
pub struct Tab {
    pub id: TabId,
    pub kind: TabKind,
    pub pinned: bool,
}

impl PartialEq for Tab {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Tab {
    /// Title shown on the tab.
    fn title(&self) -> String {
        match &self.kind {
            TabKind::Table(t) => t.table.name.clone(),
            TabKind::Sql(s) => format!("SQL {}", s.no),
        }
    }
}

/// Requests raised while drawing tabs, applied afterwards.
enum TabRequest {
    Close(TabId),
    CloseOthers(TabId),
    CloseAll,
    TogglePin(TabId),
}

/// Things the workspace asks the application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum WsOutput {
    Close,
    Run(Action),
}

/// Application data the workspace screen reads.
pub struct WsEnv<'a> {
    pub settings: &'a Settings,
    pub keymap: &'a KeyMap,
    pub tags: &'a TagStore,
    pub toasts: &'a mut Toasts,
}

/// One open database.
pub struct Workspace {
    pub conn: StoredConnection,
    pub session: SessionHandle,
    pub status: WsStatus,
    pub caps: Capabilities,
    pub databases: Vec<DatabaseInfo>,
    pub current_db: String,
    pub schemas: Vec<String>,
    pub current_schema: Option<String>,
    pub objects: Vec<ObjectInfo>,
    pub structures: HashMap<TableRef, TableStructure>,
    pub sidebar: SidebarState,
    pub dock: DockState<Tab>,
    next_tab_id: TabId,
    pub console: ConsoleLog,
    pub console_open: bool,
    pub left_open: bool,
    pub right_open: bool,
    pub palette: Option<PaletteState>,
    pub dialect: Box<dyn Dialect>,
    pub page_size: u64,
    req_map: HashMap<RequestId, (TabId, ReqKind)>,
    /// Password of the connection, used to unlock the safe modes.
    pub password: Option<String>,
    pub keywords: HashSet<String>,
    sql_counter: usize,
    fmt_width: u8,
    fmt_with: dbzilla_store::settings::IndentWith,
    fmt_kind: dbzilla_store::settings::IndentType,
    fmt_upper: bool,
    pub details: DetailsState,
    gate: Option<GateModal>,
    preview: Option<Vec<String>>,
    key_dialog: Option<KeyDialog>,
    /// Messages for the toast area: (is_error, text).
    notices: Vec<(bool, String)>,
}

impl Workspace {
    /// Creates the workspace of a connection whose session is already connecting.
    pub fn new(conn: StoredConnection, session: SessionHandle, console_entries: usize, page_size: u64, password: Option<String>) -> Self {
        let caps = Capabilities::for_driver(conn.driver);
        let conn_driver = conn.driver;
        Self {
            current_db: if conn.driver == DriverId::Sqlite { "main".into() } else { conn.database.clone() },
            conn,
            session,
            status: WsStatus::Connecting,
            caps,
            databases: Vec::new(),
            schemas: Vec::new(),
            current_schema: None,
            objects: Vec::new(),
            structures: HashMap::new(),
            sidebar: SidebarState::default(),
            dock: DockState::new(Vec::new()),
            next_tab_id: 1,
            console: ConsoleLog::new(console_entries),
            console_open: false,
            left_open: true,
            right_open: false,
            palette: None,
            dialect: dialect_for(conn_driver),
            page_size,
            req_map: HashMap::new(),
            password,
            keywords: sqlhl::keyword_set(dialect_for(conn_driver).keywords()),
            sql_counter: 0,
            fmt_width: 4,
            fmt_with: dbzilla_store::settings::IndentWith::Spaces,
            fmt_kind: dbzilla_store::settings::IndentType::Standard,
            fmt_upper: true,
            details: DetailsState::default(),
            gate: None,
            preview: None,
            key_dialog: None,
            notices: Vec::new(),
        }
    }

    /// Drains the events that arrived since the last frame.
    pub fn poll(&mut self) {
        while let Some(ev) = self.session.try_recv() {
            self.apply(ev);
        }
    }

    /// Applies one event to the state.
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Connected { server_version } => {
                self.status = WsStatus::Connected(server_version);
                self.load_schema();
            }
            Event::ConnectFailed { error } => self.status = WsStatus::Failed(error),
            Event::Disconnected { reason } => self.status = WsStatus::Disconnected(reason),
            Event::Databases(d) => self.databases = d,
            Event::Schema { schema, schemas, objects, .. } => {
                self.current_schema = schema;
                self.schemas = schemas;
                self.objects = objects;
            }
            Event::Structure(st) => self.apply_structure(*st),
            Event::StatementStarted { req, index, sql } => self.on_statement_started(req, index, sql),
            Event::RowBatch { req, index, columns, rows } => match self.req_map.get(&req).copied() {
                Some((id, ReqKind::Sql)) => self.on_sql_rows(id, index, columns, rows),
                _ => self.on_rows(req, columns, rows),
            },
            Event::QueryDone { req, index, summary } => match self.req_map.get(&req).copied() {
                Some((id, ReqKind::Sql)) => self.on_sql_done(id, index, ResStatus::Done { elapsed: summary.elapsed, affected: summary.affected, messages: summary.messages }),
                _ => self.on_page_done(req, summary.elapsed),
            },
            Event::QueryError { req, index, error, cancelled } => match self.req_map.get(&req).copied() {
                Some((id, ReqKind::Sql)) => self.on_sql_done(id, index, if cancelled { ResStatus::Cancelled } else { ResStatus::Error(error) }),
                _ => self.on_page_error(req, error),
            },
            Event::RunFinished { req } => self.on_run_finished(req),
            Event::Timeout { .. } => self.notices.push((true, "The server is taking too long to respond".into())),
            Event::Count { req, count } => self.on_total(req, Total::Exact(count)),
            Event::Estimate { req, count } => self.on_total(req, count.map_or(Total::Unknown, Total::Estimated)),
            Event::CommitDone { req, counts } => self.on_commit_done(req, counts.len()),
            Event::CommitError { req, error } => self.on_commit_error(req, error),
            Event::Console { ts_ms, sql } => self.console.push(ts_ms, sql),
            Event::Error { error } => {
                self.console.push(0, format!("-- error: {error}"));
            }
            _ => {}
        }
    }

    /// Requests the database list and the objects of the current schema.
    pub fn load_schema(&mut self) {
        self.session.send(Command::LoadDatabases);
        if self.conn.driver != DriverId::Sqlite && self.current_db.is_empty() {
            return;
        }
        let schema = if self.caps.schemas { Some(self.current_schema.clone().unwrap_or_else(|| "public".into())) } else { None };
        self.session.send(Command::LoadSchema { db: self.current_db.clone(), schema });
    }

    /// Table tab with the given id.
    fn table_tab_mut(&mut self, id: TabId) -> Option<&mut TableTab> {
        self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.id == id).and_then(|t| match &mut t.kind {
            TabKind::Table(tt) => Some(tt.as_mut()),
            TabKind::Sql(_) => None,
        })
    }

    /// Rows of a page arrived.
    fn on_rows(&mut self, req: RequestId, columns: Option<Vec<dbzilla_core::model::ColumnMeta>>, rows: Vec<dbzilla_core::model::Row>) {
        let Some((id, ReqKind::Page)) = self.req_map.get(&req).copied() else { return };
        if let Some(t) = self.table_tab_mut(id) {
            if let Some(cols) = columns {
                t.data.columns = cols;
                t.data.rows.clear();
                t.data.grid.selection.clear();
            }
            t.data.rows.extend(rows);
        }
    }

    /// A page finished loading.
    fn on_page_done(&mut self, req: RequestId, elapsed: std::time::Duration) {
        let Some((id, ReqKind::Page)) = self.req_map.remove(&req) else { return };
        let Some(t) = self.table_tab_mut(id) else { return };
        t.data.loading = false;
        t.data.elapsed = Some(elapsed);
        if t.data.total == Total::Unknown {
            self.request_total(id);
        }
    }

    /// A page failed.
    fn on_page_error(&mut self, req: RequestId, error: String) {
        let Some((id, _)) = self.req_map.remove(&req) else { return };
        if let Some(t) = self.table_tab_mut(id) {
            t.data.loading = false;
            t.data.error = Some(error);
        }
    }

    /// A count or estimate arrived.
    fn on_total(&mut self, req: RequestId, total: Total) {
        let Some((id, _)) = self.req_map.remove(&req) else { return };
        if let Some(t) = self.table_tab_mut(id) {
            t.data.total = total;
        }
    }

    /// Asks for the size of the table: exact when it is cheap or known, an estimate otherwise.
    fn request_total(&mut self, id: TabId) {
        let estimated = self.caps.estimated_count;
        let Some(t) = self.table_tab_mut(id) else { return };
        let (table, loaded, offset, limit, has_filters) = (t.table.clone(), t.data.rows.len() as u64, t.data.offset, t.data.limit, t.data.filters.iter().any(|f| f.enabled));
        if loaded < limit {
            t.data.total = Total::Exact(offset + loaded);
            return;
        }
        let req = RequestId::next();
        t.data.count_req = Some(req);
        if estimated && !has_filters {
            self.req_map.insert(req, (id, ReqKind::Count));
            self.session.send(Command::EstimateCount { req, table });
        } else {
            self.count_exact(id);
        }
    }

    /// Runs `SELECT COUNT(*)` for a tab.
    pub fn count_exact(&mut self, id: TabId) {
        let table = self.tab_table(id);
        let columns = self.structures.get(&table).map(|s| s.columns.clone()).unwrap_or_default();
        let Some(t) = self.table_tab_mut(id) else { return };
        let req = RequestId::next();
        t.data.total = Total::Counting;
        t.data.count_req = Some(req);
        let (table, filters, or) = (t.table.clone(), t.data.filters.clone(), t.data.filters_or);
        self.req_map.insert(req, (id, ReqKind::Count));
        self.session.send(Command::CountExact { req, table, columns, filters, filters_or: or });
    }

    /// Table shown by a tab.
    fn tab_table(&mut self, id: TabId) -> TableRef {
        self.table_tab_mut(id).map(|t| t.table.clone()).unwrap_or_default()
    }

    /// Loads (or reloads) the page of a tab; refused while changes are pending.
    pub fn fetch_tab(&mut self, id: TabId) {
        if self.table_tab_mut(id).is_some_and(|t| t.data.pending.has_changes()) {
            self.notices.push((true, "Commit or discard the pending changes first".into()));
            return;
        }
        self.fetch_tab_forced(id);
    }

    /// Loads the page of a tab, dropping any pending changes.
    fn fetch_tab_forced(&mut self, id: TabId) {
        if let Some(t) = self.table_tab_mut(id) {
            t.data.pending.reset();
        }
        let table = self.tab_table(id);
        let columns = self.structures.get(&table).map(|s| s.columns.clone()).unwrap_or_default();
        let Some(t) = self.table_tab_mut(id) else { return };
        if let Some(old) = t.data.fetch_req.take() {
            self.req_map.remove(&old);
        }
        let Some(t) = self.table_tab_mut(id) else { return };
        let req = RequestId::next();
        t.data.fetch_req = Some(req);
        t.data.loading = true;
        t.data.error = None;
        let page = PageRequest { req, table: t.table.clone(), columns, filters: t.data.filters.clone(), filters_or: t.data.filters_or, sort: t.data.sort.clone(), limit: t.data.limit, offset: t.data.offset };
        self.req_map.insert(req, (id, ReqKind::Page));
        self.session.send(Command::FetchPage(Box::new(page)));
    }

    /// Stores a structure and hands it to the tabs showing that table.
    fn apply_structure(&mut self, st: TableStructure) {
        for (_, tab) in self.dock.iter_all_tabs_mut() {
            let TabKind::Table(t) = &mut tab.kind else { continue };
            if t.table == st.table {
                t.structure = Some(st.clone());
                t.error = None;
                if t.data.key_cols.is_empty() {
                    t.data.key_cols = default_key_columns(Some(&st));
                }
            }
        }
        self.structures.insert(st.table.clone(), st);
    }

    /// Facts about the open tabs for the opening rules.
    fn tab_metas(&self) -> Vec<TabMeta> {
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| match &t.kind {
                TabKind::Table(tt) => TabMeta { id: t.id, target: TabTarget::Table(tt.table.clone()), pinned: t.pinned, dirty: false },
                TabKind::Sql(sq) => TabMeta { id: t.id, target: TabTarget::Sql(t.id), pinned: true, dirty: !sq.text.is_empty() },
            })
            .collect()
    }

    /// Id of the focused tab, if any.
    fn focused_tab(&mut self) -> Option<TabId> {
        self.dock.find_active_focused().map(|(_, t)| t.id)
    }

    /// Table of the focused tab.
    fn active_table(&mut self) -> Option<TableRef> {
        self.dock.find_active_focused().and_then(|(_, t)| match &t.kind {
            TabKind::Table(tt) => Some(tt.table.clone()),
            TabKind::Sql(_) => None,
        })
    }

    /// Opens a table or view following the preview-tab rules.
    pub fn open_table(&mut self, table: TableRef, mode: OpenMode) {
        self.open_table_with(table, mode, Vec::new());
    }

    /// Opens a table with initial filters (used when following a foreign key).
    pub fn open_table_with(&mut self, table: TableRef, mode: OpenMode, filters: Vec<Filter>) {
        let metas = self.tab_metas();
        let focused = self.focused_tab();
        let decision = decide_open(&metas, focused, &TabTarget::Table(table.clone()), mode);
        let new_tab = |ws: &mut Workspace, pinned: bool| {
            let id = ws.next_tab_id;
            ws.next_tab_id += 1;
            let mut data = DataState::new(ws.page_size);
            data.filters = filters.clone();
            let is_view = ws.objects.iter().any(|o| o.table == table && matches!(o.kind, ObjectKind::View | ObjectKind::MaterializedView));
            if let Some(st) = ws.structures.get(&table) {
                data.key_cols = default_key_columns(Some(st));
            }
            Tab { id, pinned, kind: TabKind::Table(Box::new(TableTab { structure: ws.structures.get(&table).cloned(), table: table.clone(), error: None, data, is_view })) }
        };
        let mut touched: Option<TabId> = None;
        match decision {
            OpenDecision::Focus(id) => {
                if let Some(loc) = self.tab_location(id) {
                    self.dock.set_active_tab(loc);
                }
                if mode == OpenMode::Pin {
                    self.set_pinned(id, true);
                }
                if !filters.is_empty() {
                    if let Some(t) = self.table_tab_mut(id) {
                        t.data.filters = filters.clone();
                        t.data.offset = 0;
                        t.data.total = Total::Unknown;
                    }
                    touched = Some(id);
                }
            }
            OpenDecision::Replace(id) => {
                let fresh = new_tab(self, mode == OpenMode::Pin);
                let new_id = fresh.id;
                if let Some((_, slot)) = self.dock.iter_all_tabs_mut().find(|(_, t)| t.id == id) {
                    *slot = fresh;
                }
                touched = Some(new_id);
            }
            OpenDecision::New => {
                let fresh = new_tab(self, mode != OpenMode::Preview);
                touched = Some(fresh.id);
                self.dock.push_to_focused_leaf(fresh);
            }
        }
        if let Some(id) = touched {
            self.fetch_tab(id);
        }
        if !self.structures.contains_key(&table) {
            self.session.send(Command::LoadStructure(table));
        }
    }

    /// Dock location of a tab.
    fn tab_location(&self, id: TabId) -> Option<(egui_dock::SurfaceIndex, egui_dock::NodeIndex, egui_dock::TabIndex)> {
        let tab = self.dock.iter_all_tabs().map(|(_, t)| t).find(|t| t.id == id)?;
        self.dock.find_tab(tab)
    }

    /// Pins or unpins a tab.
    fn set_pinned(&mut self, id: TabId, pinned: bool) {
        if let Some((_, t)) = self.dock.iter_all_tabs_mut().find(|(_, t)| t.id == id) {
            t.pinned = pinned;
        }
    }

    /// Removes tabs by predicate.
    fn retain_tabs(&mut self, keep: impl Fn(&Tab) -> bool) {
        let doomed: Vec<TabId> = self.dock.iter_all_tabs().filter(|(_, t)| !keep(t)).map(|(_, t)| t.id).collect();
        for id in doomed {
            if let Some(loc) = self.tab_location(id) {
                self.dock.remove_tab(loc);
            }
        }
    }

    /// Applies a keyboard action; returns what the application must do.
    pub fn handle_action(&mut self, a: Action) -> Option<WsOutput> {
        match a {
            Action::ToggleLeftSidebar => self.left_open = !self.left_open,
            Action::ToggleRightSidebar => self.right_open = !self.right_open,
            Action::ToggleConsole => self.console_open = !self.console_open,
            Action::OpenAnything => self.palette = Some(PaletteState::default()),
            Action::FocusSidebarSearch => {
                self.left_open = true;
                self.sidebar.tab = SidebarTab::Items;
                self.sidebar.focus_search = true;
            }
            Action::CloseTab => {
                if let Some(id) = self.focused_tab() {
                    self.retain_tabs(|t| t.id != id);
                }
            }
            Action::ReloadWorkspace => self.load_schema(),
            Action::NewSqlEditor => self.open_sql(String::new()),
            Action::RunCurrent => self.run_sql(false),
            Action::RunAll => self.run_sql(true),
            Action::Cancel => self.cancel_sql(),
            Action::Beautify => self.sql_action(SqlAction::Beautify),
            Action::ToggleComment => self.sql_action(SqlAction::Comment),
            Action::Commit => self.commit_active(),
            Action::Preview => self.preview_active(),
            Action::Discard => self.discard_active(),
            Action::Undo => self.row_op(|t| {
                t.data.pending.undo(&mut t.data.rows);
            }),
            Action::Redo => self.row_op(|t| {
                t.data.pending.redo(&mut t.data.rows);
            }),
            Action::AddRow => self.row_op(|t| {
                let cols = super::datatab::merge_columns(&t.data.columns, t.structure.as_ref());
                if !cols.is_empty() {
                    datatab::insert_row(&mut t.data, &cols);
                }
            }),
            Action::DuplicateRow => self.row_op(|t| {
                let cols = super::datatab::merge_columns(&t.data.columns, t.structure.as_ref());
                let sel = t.data.grid.selection.selected_rows();
                t.data.pending.duplicate(&mut t.data.rows, &cols, &sel);
            }),
            Action::Reconnect => {
                self.status = WsStatus::Connecting;
                self.session.send(Command::Reconnect);
            }
            other => return Some(WsOutput::Run(other)),
        }
        None
    }

    /// The focused table tab.
    fn active_tab_mut(&mut self) -> Option<(TabId, &mut TableTab)> {
        self.dock.find_active_focused().and_then(|(_, t)| {
            let id = t.id;
            match &mut t.kind {
                TabKind::Table(tt) => Some((id, tt.as_mut())),
                TabKind::Sql(_) => None,
            }
        })
    }

    /// The focused SQL tab.
    fn active_sql_mut(&mut self) -> Option<(TabId, &mut SqlTab)> {
        self.dock.find_active_focused().and_then(|(_, t)| {
            let id = t.id;
            match &mut t.kind {
                TabKind::Sql(sq) => Some((id, sq.as_mut())),
                TabKind::Table(_) => None,
            }
        })
    }

    /// SQL tab with the given id.
    fn sql_tab_mut(&mut self, id: TabId) -> Option<&mut SqlTab> {
        self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.id == id).and_then(|t| match &mut t.kind {
            TabKind::Sql(sq) => Some(sq.as_mut()),
            TabKind::Table(_) => None,
        })
    }

    /// Opens a new SQL editor tab.
    pub fn open_sql(&mut self, text: String) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.sql_counter += 1;
        self.dock.push_to_focused_leaf(Tab { id, pinned: true, kind: TabKind::Sql(Box::new(SqlTab::new(self.sql_counter, text))) });
        if let Some(loc) = self.tab_location(id) {
            self.dock.set_active_tab(loc);
        }
    }

    /// Runs the statements of the focused SQL tab through the safe-mode gate.
    pub fn run_sql(&mut self, all: bool) {
        let driver = self.conn.driver;
        let Some((id, tab)) = self.active_sql_mut() else { return };
        if tab.running {
            return;
        }
        let stmts = tab.statements_to_run(all, driver);
        if stmts.is_empty() {
            return self.notices.push((false, "There is nothing to run".into()));
        }
        let limit = tab.row_limit;
        let texts: Vec<String> = stmts.iter().map(|s| apply_row_limit(&s.text, limit)).collect();
        let labels: Vec<Option<String>> = stmts.iter().map(|s| s.label.clone()).collect();
        match gate_check(self.conn.safe_level, driver, &texts) {
            GateDecision::Allow => self.send_run(id, texts, labels),
            GateDecision::Confirm(t) => self.gate = Some(GateModal { action: GateAction::Run { labels }, statements: t, needs_password: false, input: String::new(), reveal: false, tab: id, error: None }),
            GateDecision::Password(t) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::Run { labels }, statements: t, needs_password: known, input: String::new(), reveal: false, tab: id, error: None });
            }
        }
    }

    /// Sends the statements of a SQL tab to the session.
    fn send_run(&mut self, id: TabId, statements: Vec<String>, labels: Vec<Option<String>>) {
        let req = RequestId::next();
        let Some(tab) = self.sql_tab_mut(id) else { return };
        tab.results.retain(|r| r.pinned);
        tab.active_result = 0;
        tab.running = true;
        tab.run_req = Some(req);
        tab.labels = labels;
        tab.total = statements.len();
        self.req_map.insert(req, (id, ReqKind::Sql));
        self.session.send(Command::RunSql { req, statements, opts: dbzilla_drivers::QueryOpts::default(), stop_on_error: false });
    }

    /// Cancels the running statement of the focused SQL tab.
    fn cancel_sql(&mut self) {
        if let Some((_, tab)) = self.active_sql_mut() {
            if let Some(req) = tab.run_req {
                self.session.send(Command::Cancel(req));
            }
        }
    }

    /// A statement of a SQL tab started: create its result tab.
    fn on_statement_started(&mut self, req: RequestId, index: usize, sql: String) {
        let Some((id, ReqKind::Sql)) = self.req_map.get(&req).copied() else { return };
        let Some(tab) = self.sql_tab_mut(id) else { return };
        let single = tab.total > sqltab::MAX_RESULT_TABS;
        let title = if single { "Query".to_owned() } else { tab.labels.get(index).cloned().flatten().unwrap_or_else(|| format!("Query {}", index + 1)) };
        if single {
            tab.results.retain(|r| r.pinned);
        }
        tab.results.push(ResultTab::running(index, title, sql));
        tab.active_result = tab.results.len() - 1;
    }

    /// Rows of a result arrived.
    fn on_sql_rows(&mut self, id: TabId, index: usize, columns: Option<Vec<dbzilla_core::model::ColumnMeta>>, rows: Vec<dbzilla_core::model::Row>) {
        let Some(tab) = self.sql_tab_mut(id) else { return };
        let Some(r) = tab.results.iter_mut().rev().find(|r| r.index == index && r.status == ResStatus::Running) else { return };
        if let Some(c) = columns {
            r.columns = c;
        }
        r.rows.extend(rows);
    }

    /// A statement finished.
    fn on_sql_done(&mut self, id: TabId, index: usize, status: ResStatus) {
        let Some(tab) = self.sql_tab_mut(id) else { return };
        if let Some(r) = tab.results.iter_mut().rev().find(|r| r.index == index && r.status == ResStatus::Running) {
            r.status = status;
        }
    }

    /// The whole run finished.
    fn on_run_finished(&mut self, req: RequestId) {
        let Some((id, ReqKind::Sql)) = self.req_map.remove(&req) else { return };
        let Some(tab) = self.sql_tab_mut(id) else { return };
        tab.running = false;
        tab.run_req = None;
        if let Some(i) = tab.results.iter().position(|r| !r.columns.is_empty()) {
            tab.active_result = i;
        } else {
            tab.active_result = tab.results.len().saturating_sub(1);
        }
    }

    /// Editor shortcuts on the focused SQL tab.
    fn sql_action(&mut self, a: SqlAction) {
        let driver = self.conn.driver;
        let (width, with, kind, upper) = (self.fmt_width, self.fmt_with, self.fmt_kind, self.fmt_upper);
        match a {
            SqlAction::RunCurrent => self.run_sql(false),
            SqlAction::RunAll => self.run_sql(true),
            SqlAction::Cancel => self.cancel_sql(),
            SqlAction::Beautify => {
                if let Some((_, t)) = self.active_sql_mut() {
                    t.beautify(driver, |s| sqltext::beautify(s, width, with, kind, upper));
                }
            }
            SqlAction::Minify => {
                if let Some((_, t)) = self.active_sql_mut() {
                    t.beautify(driver, sqltext::minify);
                }
            }
            SqlAction::Comment => {
                if let Some((_, t)) = self.active_sql_mut() {
                    t.toggle_comment();
                }
            }
            SqlAction::Toast(m) => self.notices.push((false, m)),
        }
    }

    /// True when the focused tab has pending changes.
    pub fn has_pending(&mut self) -> bool {
        self.active_tab_mut().is_some_and(|(_, t)| t.data.pending.has_changes())
    }

    /// Statements for the pending changes of a tab.
    fn statements_of(&mut self, id: TabId) -> Result<Vec<String>, String> {
        let columns = {
            let table = self.tab_table(id);
            let st = self.structures.get(&table).cloned();
            let Some(t) = self.table_tab_mut(id) else { return Err("tab closed".into()) };
            super::datatab::merge_columns(&t.data.columns, st.as_ref().or(t.structure.as_ref()))
        };
        let dialect = dialect_for(self.conn.driver);
        let Some(t) = self.table_tab_mut(id) else { return Err("tab closed".into()) };
        let cs = t.data.pending.to_changeset(&t.table, &columns, &t.data.key_cols, &t.data.rows).map_err(|e| e.to_string())?;
        generate_sql(&cs, dialect.as_ref()).map_err(|e| e.to_string())
    }

    /// Shows the generated SQL of the focused tab.
    pub fn preview_active(&mut self) {
        let Some((id, _)) = self.active_tab_mut() else { return };
        match self.statements_of(id) {
            Ok(s) if s.is_empty() => self.notices.push((false, "There are no pending changes".into())),
            Ok(s) => self.preview = Some(s),
            Err(e) => self.notices.push((true, e)),
        }
    }

    /// Throws away the pending changes of the focused tab and reloads it.
    pub fn discard_active(&mut self) {
        if let Some((id, _)) = self.active_tab_mut() {
            self.fetch_tab_forced(id);
        }
    }

    /// Starts the commit of the focused tab, going through the safe-mode gate.
    pub fn commit_active(&mut self) {
        let Some((id, _)) = self.active_tab_mut() else { return };
        let statements = match self.statements_of(id) {
            Ok(s) if s.is_empty() => return self.notices.push((false, "There are no pending changes".into())),
            Ok(s) => s,
            Err(e) => return self.notices.push((true, e)),
        };
        match gate_check(self.conn.safe_level, self.conn.driver, &statements) {
            GateDecision::Allow => self.send_commit(id, statements),
            GateDecision::Confirm(s) => self.gate = Some(GateModal { action: GateAction::Commit, statements: s, needs_password: false, input: String::new(), reveal: false, tab: id, error: None }),
            GateDecision::Password(s) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::Commit, statements: s, needs_password: known, input: String::new(), reveal: false, tab: id, error: None });
            }
        }
    }

    /// Sends the statements to the server.
    fn send_commit(&mut self, id: TabId, statements: Vec<String>) {
        let req = RequestId::next();
        if let Some(t) = self.table_tab_mut(id) {
            t.data.committing = true;
        }
        self.req_map.insert(req, (id, ReqKind::Commit));
        self.session.send(Command::Commit { req, statements });
    }

    /// A commit succeeded: reload the tab.
    fn on_commit_done(&mut self, req: RequestId, statements: usize) {
        let Some((id, ReqKind::Commit)) = self.req_map.remove(&req) else { return };
        if let Some(t) = self.table_tab_mut(id) {
            t.data.committing = false;
        }
        self.notices.push((false, format!("{statements} {} committed", if statements == 1 { "statement" } else { "statements" })));
        self.fetch_tab_forced(id);
    }

    /// A commit failed: nothing is lost, the changes stay pending.
    fn on_commit_error(&mut self, req: RequestId, error: String) {
        let Some((id, ReqKind::Commit)) = self.req_map.remove(&req) else { return };
        if let Some(t) = self.table_tab_mut(id) {
            t.data.committing = false;
        }
        self.notices.push((true, format!("Commit failed: {error}")));
    }

    /// Applies edits made in the Details panel to every selected row.
    fn apply_detail_edits(&mut self, edits: Vec<DetailsAction>) {
        let Some((_, t)) = self.active_tab_mut() else { return };
        let selected = t.data.grid.selection.selected_rows();
        let mut errors = Vec::new();
        for e in edits {
            match e {
                DetailsAction::Edit { col, value } => {
                    for &r in &selected {
                        t.data.pending.edit_cell(&mut t.data.rows, r, col, value.clone());
                    }
                }
                DetailsAction::Error(m) => errors.push(m),
            }
        }
        for m in errors {
            self.notices.push((true, m));
        }
    }

    /// Row operations on the focused tab.
    fn row_op(&mut self, f: impl FnOnce(&mut TableTab)) {
        if let Some((_, t)) = self.active_tab_mut() {
            if !t.is_view {
                f(t);
            }
        }
    }

    /// Asks the session to reconnect.
    pub fn reconnect(&mut self) {
        self.status = WsStatus::Connecting;
        self.session.send(Command::Reconnect);
    }

    /// Items of Open Anything: objects and actions.
    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut v: Vec<PaletteItem> = self
            .objects
            .iter()
            .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView))
            .map(|o| PaletteItem::Object { label: o.table.name.clone(), table: o.table.clone() })
            .collect();
        v.extend(Action::ALL.iter().map(|a| PaletteItem::Action(*a)));
        v
    }

    /// Draws the whole workspace screen.
    pub fn show(&mut self, ctx: &egui::Context, env: &mut WsEnv) -> Vec<WsOutput> {
        let mut out = Vec::new();
        let pending = self.has_pending();
        self.toolbar(ctx, env, pending, &mut out);
        self.status_bar(ctx, env, &mut out);
        if self.console_open {
            egui::TopBottomPanel::bottom("ws_console").resizable(true).default_height(160.0).height_range(80.0..=500.0).frame(Frame::new().fill(theme::NAVY_DARK).inner_margin(Margin::symmetric(12, 8))).show(ctx, |ui| {
                if console::show(ui, &self.console) {
                    self.console.clear();
                }
            });
        }
        let mut sidebar_action = None;
        if self.left_open {
            let active = self.active_table();
            egui::SidePanel::left("ws_sidebar").default_width(260.0).width_range(200.0..=480.0).resizable(true).frame(Frame::new().fill(theme::DEEP_DARK).inner_margin(Margin::same(10))).show(ctx, |ui| {
                let view = sidebar::SidebarView { objects: &self.objects, schemas: &self.schemas, current_schema: self.current_schema.as_deref(), caps: &self.caps, active: active.as_ref() };
                sidebar_action = sidebar::show(ui, &mut self.sidebar, &view, &mut |ui, tab| {
                    ui.add_space(16.0);
                    widgets::muted(ui, if tab == SidebarTab::Queries { "Saved queries will appear here." } else { "The query history will appear here." });
                });
            });
        }
        let mut detail_actions = Vec::new();
        if self.right_open {
            egui::SidePanel::right("ws_details").default_width(300.0).width_range(240.0..=520.0).resizable(true).frame(Frame::new().fill(theme::DEEP_DARK).inner_margin(Margin::same(10))).show(ctx, |ui| {
                let Workspace { dock, details, .. } = &mut *self;
                match dock.find_active_focused() {
                    Some((_, t)) if matches!(t.kind, TabKind::Table(_)) => {
                        let id = t.id;
                        let TabKind::Table(tt) = &mut t.kind else { return };
                        let cols = datatab::merge_columns(&tt.data.columns, tt.structure.as_ref());
                        let selected = tt.data.grid.selection.selected_rows();
                        detail_actions = details::show(ui, details, id, &cols, &tt.data.rows, &selected, !tt.is_view);
                    }
                    _ => {
                        ui.label(widgets::semibold("Details", 15.0).color(theme::TEXT));
                        widgets::muted(ui, "Open a table to see the fields of its rows.");
                    }
                }
            });
        }
        if !detail_actions.is_empty() {
            self.apply_detail_edits(detail_actions);
        }
        let mut requests = Vec::new();
        let mut data_actions: Vec<(TabId, DataAction)> = Vec::new();
        let mut sql_actions: Vec<(TabId, SqlAction)> = Vec::new();
        self.fmt_width = env.settings.indent_width;
        self.fmt_with = env.settings.indent_with;
        self.fmt_kind = env.settings.indent_type;
        self.fmt_upper = env.settings.uppercase_keywords;
        self.page_size = env.settings.page_size;
        let csv = CsvOptions { delimiter: env.settings.csv.delimiter, quote: env.settings.csv.quote, quote_always: env.settings.csv.quote_always, line_break: env.settings.csv.line_break.clone(), decimal: env.settings.csv.decimal, null_as_empty: false };
        let row_height = (env.settings.table_row_padding + 12.0).clamp(20.0, 60.0);
        let alternating = env.settings.alternating_rows;
        egui::CentralPanel::default().frame(Frame::new().fill(theme::DEEP_DARK)).show(ctx, |ui| {
            if self.dock.iter_all_tabs().next().is_none() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    widgets::heading(ui, &self.conn.name);
                    widgets::muted(ui, &format!("Open a table from the sidebar, or press {} to search.", env.keymap.hint(Action::OpenAnything)));
                });
                return;
            }
            let mut style = Style::from_egui(ui.style().as_ref());
            style.tab_bar.bg_fill = theme::NAVY_DARK;
            style.tab_bar.hline_color = theme::FIELD_ROW_BORDER;
            style.tab.tab_body.bg_fill = theme::DEEP_DARK;
            style.tab.tab_body.stroke = Stroke::NONE;
            style.main_surface_border_stroke = Stroke::NONE;
            for s in [&mut style.tab.active, &mut style.tab.focused] {
                s.bg_fill = theme::DEEP_DARK;
                s.text_color = theme::TEXT;
                s.outline_color = theme::FIELD_ROW_BORDER;
            }
            style.tab.inactive.bg_fill = theme::NAVY_DARK;
            style.tab.inactive.text_color = theme::MUTED;
            style.tab.inactive.outline_color = theme::FIELD_ROW_BORDER;
            style.tab.hovered.bg_fill = theme::FIELD_ROW_BG;
            style.tab.hovered.text_color = theme::TEXT;
            style.tab.hovered.outline_color = theme::FIELD_ROW_BORDER;
            let mut viewer = Viewer { requests: &mut requests, data_actions: &mut data_actions, dialect: self.dialect.as_ref(), csv: &csv, alternating, row_height, structures: &self.structures, sql_actions: &mut sql_actions, keywords: &self.keywords, driver: self.conn.driver, font_size: env.settings.editor_font_size };
            DockArea::new(&mut self.dock).style(style).show_add_buttons(false).show_leaf_close_all_buttons(false).show_leaf_collapse_buttons(false).show_inside(ui, &mut viewer);
        });
        for r in requests {
            match r {
                TabRequest::Close(id) => self.retain_tabs(|t| t.id != id),
                TabRequest::CloseOthers(id) => self.retain_tabs(|t| t.id == id),
                TabRequest::CloseAll => self.retain_tabs(|_| false),
                TabRequest::TogglePin(id) => {
                    let pinned = self.dock.iter_all_tabs().find(|(_, t)| t.id == id).is_some_and(|(_, t)| t.pinned);
                    self.set_pinned(id, !pinned);
                }
            }
        }
        for (_, a) in sql_actions {
            self.sql_action(a);
        }
        for (id, a) in data_actions {
            match a {
                DataAction::Refetch => self.fetch_tab(id),
                DataAction::CountExact => self.count_exact(id),
                DataAction::FollowFk { table, column, value } => self.open_table_with(table, OpenMode::NewTab, vec![datatab::fk_filter(&column, &value)]),
                DataAction::Toast(m) => env.toasts.ok(m),
                DataAction::Error(m) => env.toasts.err(m),
                DataAction::ChooseKey => self.open_key_dialog(id),
            }
        }
        self.modals(ctx);
        for (is_err, m) in self.notices.drain(..) {
            if is_err {
                env.toasts.err(m);
            } else {
                env.toasts.ok(m);
            }
        }
        match sidebar_action {
            Some(SidebarAction::Open(t, mode)) => self.open_table(t, mode),
            Some(SidebarAction::CopyName(n)) => {
                ctx.copy_text(n);
                env.toasts.ok("Name copied");
            }
            Some(SidebarAction::SwitchSchema(s)) => {
                self.current_schema = Some(s);
                self.load_schema();
            }
            Some(SidebarAction::Refresh) => self.load_schema(),
            None => {}
        }
        if self.palette.is_some() {
            let items = self.palette_items();
            let mut close = false;
            let choice = self.palette.as_mut().and_then(|p| palette::show(ctx, p, &items, env.keymap, &mut close));
            if close {
                self.palette = None;
            }
            match choice {
                Some(PaletteChoice::Open(t, mode)) => {
                    self.palette = None;
                    self.open_table(t, mode);
                }
                Some(PaletteChoice::Run(a)) => {
                    self.palette = None;
                    if let Some(o) = self.handle_action(a) {
                        out.push(o);
                    }
                }
                None => {}
            }
        }
        out
    }

    /// Opens the dialog choosing key columns for a tab.
    fn open_key_dialog(&mut self, id: TabId) {
        let Some(t) = self.table_tab_mut(id) else { return };
        let names: Vec<String> = datatab::merge_columns(&t.data.columns, t.structure.as_ref()).into_iter().map(|c| c.name).collect();
        let selected = names.iter().map(|n| t.data.key_cols.contains(n)).collect();
        self.key_dialog = Some(KeyDialog { tab: id, names, selected });
    }

    /// Preview, safe-mode and key-column dialogs.
    fn modals(&mut self, ctx: &egui::Context) {
        if let Some(statements) = self.preview.clone() {
            let mut action = None;
            widgets::modal(ctx, "Pending changes", |ui| {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for s in &statements {
                        ui.add(egui::Label::new(RichText::new(s).monospace().color(theme::TEXT)).wrap());
                        ui.add_space(4.0);
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Commit").clicked() {
                        action = Some(0);
                    }
                    if widgets::danger_button(ui, "Discard").clicked() {
                        action = Some(1);
                    }
                    if widgets::secondary_button(ui, "Close").clicked() {
                        action = Some(2);
                    }
                });
            });
            match action {
                Some(0) => {
                    self.preview = None;
                    self.commit_active();
                }
                Some(1) => {
                    self.preview = None;
                    self.discard_active();
                }
                Some(_) => self.preview = None,
                None => {}
            }
        }
        if let Some(g) = self.gate.as_mut() {
            let mut decision = None;
            let title = if g.needs_password { "Enter the password to continue" } else { "Review the statements" };
            widgets::modal(ctx, title, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for s in &g.statements {
                        ui.add(egui::Label::new(RichText::new(s).monospace().color(theme::TEXT)).wrap());
                    }
                });
                if g.needs_password {
                    ui.add_space(8.0);
                    widgets::field_label(ui, "Database password");
                    widgets::secret_input(ui, &mut g.input, &mut g.reveal);
                }
                if let Some(e) = &g.error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Send").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    let ok = !g.needs_password || self.password.as_deref().is_some_and(|p| verify_password(p, &g.input));
                    if ok {
                        if let Some(g) = self.gate.take() {
                            match g.action {
                                GateAction::Commit => self.send_commit(g.tab, g.statements),
                                GateAction::Run { labels } => self.send_run(g.tab, g.statements, labels),
                            }
                        }
                    } else if let Some(g) = self.gate.as_mut() {
                        g.error = Some("The password is not correct".into());
                    }
                }
                Some(false) => self.gate = None,
                None => {}
            }
        }
        if let Some(k) = self.key_dialog.as_mut() {
            let mut decision = None;
            widgets::modal(ctx, "Key columns", |ui| {
                widgets::muted(ui, "Rows are updated and deleted using these columns in the WHERE clause.");
                for (i, n) in k.names.iter().enumerate() {
                    ui.checkbox(&mut k.selected[i], n);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Use these columns").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(k) = self.key_dialog.take() {
                        let chosen: Vec<String> = k.names.iter().zip(&k.selected).filter(|(_, s)| **s).map(|(n, _)| n.clone()).collect();
                        if let Some(t) = self.table_tab_mut(k.tab) {
                            t.data.key_cols = chosen;
                        }
                    }
                }
                Some(false) => self.key_dialog = None,
                None => {}
            }
        }
    }

    /// The toolbar (SPEC 7.2).
    fn toolbar(&mut self, ctx: &egui::Context, env: &WsEnv, pending: bool, out: &mut Vec<WsOutput>) {
        let tip = |name: &str, a: Action| {
            let h = env.keymap.hint(a);
            if h.is_empty() { name.to_owned() } else { format!("{name} ({h})") }
        };
        egui::TopBottomPanel::top("ws_toolbar").frame(Frame::new().fill(theme::NAVY_DARK).inner_margin(Margin::symmetric(10, 6))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if widgets::ico_button(ui, Ico::Sidebar, &tip("Toggle left sidebar", Action::ToggleLeftSidebar)).clicked() {
                    self.left_open = !self.left_open;
                }
                ui.add_space(6.0);
                ui.add_enabled_ui(pending, |ui| {
                    if widgets::ico_button(ui, Ico::Discard, &tip("Discard changes", Action::Discard)).clicked() {
                        self.discard_active();
                    }
                    if widgets::ico_button(ui, Ico::Preview, &tip("Preview changes", Action::Preview)).clicked() {
                        self.preview_active();
                    }
                    if widgets::ico_button(ui, Ico::Commit, &tip("Commit", Action::Commit)).clicked() {
                        self.commit_active();
                    }
                });
                ui.add_space(6.0);
                widgets::ico_button(ui, Ico::Shield, &format!("Safe mode: {}", super::connform::safe_label(self.conn.safe_level)));
                if widgets::ico_button(ui, Ico::File, &tip("New SQL editor", Action::NewSqlEditor)).clicked() {
                    self.open_sql(String::new());
                }
                if widgets::ico_button(ui, Ico::Search, &tip("Open anything", Action::OpenAnything)).clicked() {
                    self.palette = Some(PaletteState::default());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::ico_button(ui, Ico::Sidebar, &tip("Toggle right sidebar", Action::ToggleRightSidebar)).clicked() {
                        self.right_open = !self.right_open;
                    }
                    if widgets::ico_button(ui, Ico::Console, &tip("Toggle console", Action::ToggleConsole)).clicked() {
                        self.console_open = !self.console_open;
                    }
                    if widgets::ico_button(ui, Ico::Refresh, &tip("Reload", Action::ReloadWorkspace)).clicked() {
                        self.load_schema();
                    }
                    if widgets::ico_button(ui, Ico::Close, "Close this workspace").clicked() {
                        out.push(WsOutput::Close);
                    }
                });
            });
        });
    }

    /// The status bar tinted with the connection colour.
    fn status_bar(&mut self, ctx: &egui::Context, env: &WsEnv, _out: &mut Vec<WsOutput>) {
        let tint = self
            .conn
            .status_color
            .as_deref()
            .and_then(theme::parse_hex_color)
            .or_else(|| self.conn.tag.as_ref().and_then(|t| env.tags.color(t)).and_then(|h| theme::parse_hex_color(&h)))
            .unwrap_or(theme::PRIMARY);
        egui::TopBottomPanel::bottom("ws_status").exact_height(28.0).frame(Frame::new().fill(tint.gamma_multiply(0.18)).inner_margin(Margin::symmetric(12, 2))).show(ctx, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let (state, color) = match &self.status {
                    WsStatus::Connecting => ("connecting...".to_owned(), theme::TERTIARY),
                    WsStatus::Connected(v) => (v.clone(), theme::OK),
                    WsStatus::Failed(e) | WsStatus::Disconnected(e) => (e.clone(), theme::ERROR),
                };
                widgets::status_dot(ui, color, &state);
                let db = if self.current_db.is_empty() { String::new() } else { format!(" / {}", self.current_db) };
                ui.label(RichText::new(format!("{}{db}", self.conn.name)).size(12.0).color(theme::TEXT));
                if let Some(t) = &self.conn.tag {
                    ui.label(RichText::new(t.to_uppercase()).font(egui::FontId::new(11.0, theme::semibold())).color(tint));
                }
                ui.label(RichText::new(state).size(12.0).color(theme::MUTED));
                if matches!(self.status, WsStatus::Failed(_) | WsStatus::Disconnected(_)) && ui.small_button("Reconnect").clicked() {
                    self.reconnect();
                }
            });
        });
    }
}

/// Draws the content of tabs and collects their requests.
struct Viewer<'a> {
    requests: &'a mut Vec<TabRequest>,
    data_actions: &'a mut Vec<(TabId, DataAction)>,
    dialect: &'a dyn Dialect,
    csv: &'a CsvOptions,
    alternating: bool,
    row_height: f32,
    structures: &'a HashMap<TableRef, TableStructure>,
    sql_actions: &'a mut Vec<(TabId, SqlAction)>,
    keywords: &'a HashSet<String>,
    driver: DriverId,
    font_size: f32,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        let t = tab.title();
        if tab.pinned { RichText::new(t).into() } else { RichText::new(t).italics().into() }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let id = tab.id;
        let t = match &mut tab.kind {
            TabKind::Table(t) => t,
            TabKind::Sql(sq) => {
                let env = SqlEnv { keywords: self.keywords, driver: self.driver, font_size: self.font_size, alternating: self.alternating, row_height: self.row_height };
                for a in sqltab::show(ui, sq, &env) {
                    self.sql_actions.push((id, a));
                }
                return;
            }
        };
        let table = t.table.clone();
        let env = DataEnv { dialect: self.dialect, csv: self.csv, alternating: self.alternating, row_height: self.row_height, table: &table, structure: self.structures.get(&table), is_view: t.is_view };
        for a in datatab::show(ui, &mut t.data, &env) {
            self.data_actions.push((id, a));
        }
    }

    fn on_tab_button(&mut self, tab: &mut Tab, response: &egui::Response) {
        if response.double_clicked() && !tab.pinned {
            self.requests.push(TabRequest::TogglePin(tab.id));
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Tab, _surface: egui_dock::SurfaceIndex, _node: egui_dock::NodeIndex) {
        widgets::flat_menu_style(ui);
        if ui.button(if tab.pinned { "Unpin tab" } else { "Keep tab open" }).clicked() {
            self.requests.push(TabRequest::TogglePin(tab.id));
            ui.close_menu();
        }
        if ui.button("Close tab").clicked() {
            self.requests.push(TabRequest::Close(tab.id));
            ui.close_menu();
        }
        if ui.button("Close other tabs").clicked() {
            self.requests.push(TabRequest::CloseOthers(tab.id));
            ui.close_menu();
        }
        if ui.button("Close all tabs").clicked() {
            self.requests.push(TabRequest::CloseAll);
            ui.close_menu();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::changes::CellValue;
    use dbzilla_core::value::Value;
    use dbzilla_drivers::{ConnectConfig, SslMode};
    use dbzilla_session::{default_factory, spawn, SessionConfig};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A workspace connected to a SQLite file holding `t(id pk, name)` with two rows.
    fn connected(rt: &tokio::runtime::Runtime) -> (tempfile::TempDir, std::path::PathBuf, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.sqlite3");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1,'alpha'),(2,'beta');").unwrap();
        let mut conn = StoredConnection::new("test", DriverId::Sqlite);
        conn.database = path.to_string_lossy().into_owned();
        let session = spawn(rt.handle(), SessionConfig { query_timeout: None, keep_alive: None }, default_factory(), Arc::new(|| {}));
        session.send(Command::Connect(ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: conn.database.clone(), ssl: SslMode::Disabled, bootstrap: vec![] }));
        let mut ws = Workspace::new(conn, session, 1000, 300, None);
        wait(&mut ws, |w| matches!(w.status, WsStatus::Connected(_)) && !w.objects.is_empty());
        (dir, path, ws)
    }

    /// Polls the workspace until `done` holds (10 s limit).
    fn wait(ws: &mut Workspace, done: impl Fn(&mut Workspace) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            ws.poll();
            if done(ws) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for the workspace");
    }

    /// Opens table `t` and waits for its rows; focuses its tab.
    fn open_t(ws: &mut Workspace) {
        ws.open_table(TableRef::simple("t"), OpenMode::Pin);
        let loc = ws.dock.iter_all_tabs().next().map(|(l, _)| l).expect("a tab");
        ws.dock.set_focused_node_and_surface(loc);
        wait(ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 2));
    }

    fn name_of_row(ws: &mut Workspace, r: usize) -> Value {
        ws.active_tab_mut().map(|(_, t)| t.data.rows[r][1].clone()).unwrap()
    }

    /// Edit, commit, reload: the database and the grid both show the change.
    #[test]
    fn edit_commit_reload() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        open_t(&mut ws);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("changed".into())));
        assert!(ws.has_pending());
        ws.commit_active();
        wait(&mut ws, |w| !w.has_pending() && w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && !t.data.committing));
        assert_eq!(name_of_row(&mut ws, 0), Value::Text("changed".into()));
        let c = rusqlite::Connection::open(&path).unwrap();
        let v: String = c.query_row("SELECT name FROM t WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "changed");
    }

    /// A failing commit leaves the database untouched and keeps the pending changes.
    #[test]
    fn failed_commit_keeps_pending_changes_and_the_database() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        open_t(&mut ws);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("ok".into())));
        t.data.pending.edit_cell(&mut t.data.rows, 1, 0, CellValue::Value(Value::I64(1)));
        ws.commit_active();
        wait(&mut ws, |w| w.notices.iter().any(|(err, m)| *err && m.contains("Commit failed")));
        assert!(ws.has_pending(), "nothing is lost after a failed commit");
        let c = rusqlite::Connection::open(&path).unwrap();
        let names: Vec<String> = c.prepare("SELECT name FROM t ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(names, vec!["alpha", "beta"], "the first statement was rolled back too");
    }

    /// Changes on a table without any key cannot be committed, and say why.
    #[test]
    fn updates_without_a_key_are_refused() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_t(&mut ws);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.key_cols.clear();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("x".into())));
        ws.commit_active();
        assert!(ws.notices.iter().any(|(err, m)| *err && m.contains("no primary key")), "{:?}", ws.notices);
    }

    /// Commits can be intercepted by the safe-mode gate.
    #[test]
    fn safe_mode_asks_before_sending() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        ws.conn.safe_level = dbzilla_core::safemode::SafeLevel::Alert1;
        open_t(&mut ws);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("gated".into())));
        ws.commit_active();
        assert!(ws.gate.is_some(), "the statements wait for confirmation");
        std::thread::sleep(Duration::from_millis(200));
        let c = rusqlite::Connection::open(&path).unwrap();
        let v: String = c.query_row("SELECT name FROM t WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "alpha", "nothing was sent yet");
    }

    /// Opens an SQL editor with `text` and focuses it.
    fn open_sql_focused(ws: &mut Workspace, text: &str) {
        ws.open_sql(text.to_owned());
        let loc = ws.dock.iter_all_tabs().last().map(|(l, _)| l).expect("a tab");
        ws.dock.set_focused_node_and_surface(loc);
    }

    fn sql_running(ws: &mut Workspace) -> bool {
        ws.active_sql_mut().is_some_and(|(_, t)| t.running)
    }

    /// A script runs statement by statement: named tabs, an error does not stop the others.
    #[test]
    fn sql_script_with_named_results_and_an_error() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "-- name: First\nSELECT 1 AS a;\nSELEC bad;\nSELECT 2 AS b");
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        let (_, t) = ws.active_sql_mut().unwrap();
        let titles: Vec<&str> = t.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["First", "Query 2", "Query 3"]);
        assert!(matches!(t.results[0].status, ResStatus::Done { .. }));
        assert_eq!(t.results[0].rows, vec![vec![Value::I64(1)]]);
        assert!(matches!(&t.results[1].status, ResStatus::Error(e) if e.to_lowercase().contains("syntax") || e.contains("SELEC")));
        assert_eq!(t.results[2].rows, vec![vec![Value::I64(2)]]);
    }

    /// The row limit is applied to SELECTs without a LIMIT, and only to them.
    #[test]
    fn sql_row_limit_is_applied() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "SELECT * FROM t");
        ws.active_sql_mut().unwrap().1.row_limit = Some(1);
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        assert_eq!(ws.active_sql_mut().unwrap().1.results[0].rows.len(), 1);
        ws.active_sql_mut().unwrap().1.row_limit = None;
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        assert_eq!(ws.active_sql_mut().unwrap().1.results[0].rows.len(), 2);
    }

    /// Pinned results survive the next run; the others are replaced.
    #[test]
    fn pinned_results_survive_a_rerun() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "SELECT 1");
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        ws.active_sql_mut().unwrap().1.results[0].pinned = true;
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        assert_eq!(ws.active_sql_mut().unwrap().1.results.len(), 2);
    }

    /// A running statement can be cancelled and the editor stays usable.
    #[test]
    fn sql_cancel() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 500000000) SELECT count(*) FROM c");
        ws.active_sql_mut().unwrap().1.row_limit = None;
        ws.run_sql(true);
        wait(&mut ws, |w| w.active_sql_mut().is_some_and(|(_, t)| !t.results.is_empty()));
        std::thread::sleep(Duration::from_millis(200));
        ws.cancel_sql();
        wait(&mut ws, |w| !sql_running(w));
        assert_eq!(ws.active_sql_mut().unwrap().1.results[0].status, ResStatus::Cancelled);
        ws.active_sql_mut().unwrap().1.text = "SELECT 5".into();
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        assert_eq!(ws.active_sql_mut().unwrap().1.results[0].rows, vec![vec![Value::I64(5)]]);
    }

    /// NFR-01: a million streamed rows never block a frame for long.
    #[test]
    fn million_rows_stream_without_blocking_frames() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 1000000) SELECT x, 'row ' || x FROM c");
        ws.active_sql_mut().unwrap().1.row_limit = None;
        ws.run_sql(true);
        let (mut worst, mut frames) = (Duration::ZERO, 0u32);
        let start = Instant::now();
        while sql_running(&mut ws) && start.elapsed() < Duration::from_secs(60) {
            let t0 = Instant::now();
            ws.poll();
            worst = worst.max(t0.elapsed());
            frames += 1;
            std::thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(ws.active_sql_mut().unwrap().1.results[0].rows.len(), 1_000_000);
        assert!(worst < Duration::from_millis(250), "slowest frame took {worst:?} over {frames} frames");
    }
}

