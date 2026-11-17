//! An open connection: session, sidebar, tabs, console and the workspace screen (SPEC 7).

use super::console::{self, ConsoleLog};
use std::sync::Arc;
use super::jobs::{self, ExportDialog, ImportDialog, JobCenter, JobEnv, JobState};
use super::datatab::{self, DataAction, DataEnv, DataState, Total};
use super::dbsearch::{self, SearchAction, SearchState};
use super::details::{self, DetailsAction, DetailsState};
use super::diagram::{self, DiagramAction, DiagramTab};
use super::metrics::{self, MetricsAction, MetricsTab};
use super::process::{self, ProcessAction, ProcessList};
use dbzilla_core::diagram::DocLink;
use super::pending::default_key_columns;
use super::palette::{self, PaletteChoice, PaletteItem, PaletteState};
use super::sidebar::{self, ExtraAction, SidebarAction, SidebarState, SidebarTab};
use super::sqltab::{self, ResStatus, ResultTab, SqlAction, SqlEnv, SqlTab};
use super::structdraft::{FkDraft, StructDraft};
use super::structure::{self, StructAction, StructEnv};
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
use dbzilla_core::copyfmt::{copy_script, format_rows, CopyContext, CopyFormat, CsvOptions, ScriptKind};
use dbzilla_core::ddl::{ForeignKeyDef, generate_ddl, StructuralOp};
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::filter::Filter;
use dbzilla_core::rowlimit::apply_row_limit;
use dbzilla_core::safemode::SafeLevel;
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef, TableStructure};
use dbzilla_drivers::DatabaseInfo;
use dbzilla_session::gate::{gate_check, verify_password, GateDecision};
use dbzilla_session::{Command, Event, PageRequest, RequestId, SessionHandle};
use dbzilla_store::connections::StoredConnection;
use dbzilla_store::diagrams::DiagramStore;
use dbzilla_store::metrics::MetricsStore;
use dbzilla_store::history::{HistoryEntry, HistoryKind, HistoryStore};
use dbzilla_store::queries::{QueryEntry, QuerySort, QueryStore};
use dbzilla_store::settings::Settings;
use dbzilla_store::tags::TagStore;
use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Stroke};
use egui_dock::{DockArea, DockState, Style, TabViewer};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// Statements that replace a view with the edited definition.
fn view_apply_statements(driver: DriverId, table: &TableRef, ddl: &str) -> Vec<String> {
    let d = dialect_for(driver);
    let body = ddl.trim().trim_end_matches(';').to_owned();
    match driver {
        DriverId::Postgres => vec![body],
        DriverId::MySql => {
            let rest = body.strip_prefix("CREATE").or_else(|| body.strip_prefix("create")).map(str::to_owned);
            vec![rest.map_or(body, |r| format!("ALTER{r}"))]
        }
        DriverId::Sqlite => vec![format!("DROP VIEW {}", d.quote_table(table)), body],
    }
}

/// Quotes an identifier only when it is not a plain lower-case word.
fn quote_if_needed(d: &dyn Dialect, name: &str) -> String {
    let plain = !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain { name.to_owned() } else { d.quote_ident(name) }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Connection state shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum WsStatus {
    Connecting,
    Connected(String),
    Failed(String),
    Disconnected(String),
}

/// Which view of a table is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabView {
    Data,
    Structure,
    Index,
    Ddl,
}

/// Content of a table tab.
pub struct TableTab {
    pub table: TableRef,
    pub structure: Option<TableStructure>,
    pub error: Option<String>,
    pub data: DataState,
    pub is_view: bool,
    pub is_matview: bool,
    pub view: TabView,
    pub draft: Option<StructDraft>,
    pub ddl_text: String,
    /// The last commit changed the structure (reload it afterwards).
    ddl_commit: bool,
    /// New name when the last commit renames the table.
    rename_to: Option<String>,
}

impl TableTab {
    /// True when the DDL text differs from the database.
    fn ddl_dirty(&self) -> bool {
        self.structure.as_ref().is_some_and(|s| s.ddl != self.ddl_text)
    }
}

/// What happens after a statement batch that is not a tab commit.
#[derive(Debug, Clone, Default)]
struct DdlAfter {
    closed: Option<TableRef>,
    message: String,
}

/// Table level dialogs (SPEC 8.9).
enum OpDialog {
    Clone { source: TableRef, name: String, with_data: bool },
    Truncate { table: TableRef, cascade: bool, restart: bool },
    Drop { table: TableRef, cascade: bool },
}

/// Database picker and its operations (SPEC 11.1).
struct DbPicker {
    query: String,
    selected: Option<String>,
}

/// Dialogs of database operations.
enum DbDialog {
    New { name: String, encoding: String, collation: String },
    Rename { from: String, to: String },
    Drop { name: String, typed: String },
}

/// Asks for the password before the safe mode level changes.
struct LevelDialog {
    level: dbzilla_core::safemode::SafeLevel,
    input: String,
    reveal: bool,
    error: Option<String>,
}

/// Dialog creating a foreign key.
struct FkDialog {
    tab: TabId,
    col: usize,
    ref_table: String,
    ref_columns: String,
    on_update: String,
    on_delete: String,
}

/// What a link drawn in the diagram becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    /// A real foreign key, created with DDL.
    Database,
    /// A dashed line kept in the application only.
    DiagramOnly,
}

/// Dialog asking what to do with a link dragged between two columns of the diagram.
struct LinkDialog {
    link: DocLink,
    kind: LinkKind,
    constraint: String,
    on_update: String,
    on_delete: String,
    error: Option<String>,
}

/// Foreign key actions offered in menus.
const FK_ACTIONS: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

/// The persistent stores a workspace writes to.
pub struct Stores {
    pub history: HistoryStore,
    pub diagrams: DiagramStore,
    pub metrics: MetricsStore,
    pub queries: QueryStore,
}

/// Dialog saving a statement as a query file.
struct SaveDialog {
    name: String,
    keywords: String,
    sql: String,
}

/// What a request of a tab is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReqKind {
    Page,
    Count,
    Commit,
    Sql,
    Ddl,
    Search,
    /// A query of the metrics board object with this id.
    Metric(u64),
}

/// What a confirmed gate does.
enum GateAction {
    Commit,
    Ddl(DdlAfter),
    DbOp(dbzilla_session::DbOp),
    Import(String, jobs::ImportRequest),
    Run { labels: Vec<Option<String>>, originals: Vec<String> },
    /// Statements that stop queries, run by the process list.
    Kill,
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

/// What the user chose in the Variables dialog.
enum ParamsDecision {
    Apply,
    Cleanup,
    Cancel,
}

/// Variables dialog of the query parameters (SPEC 10.11), shown before a run.
struct ParamsModal {
    tab: TabId,
    /// Statements to run: (text with placeholders, result tab label).
    statements: Vec<(String, Option<String>)>,
    /// Variable names in order of appearance.
    names: Vec<String>,
    /// Typed values, parallel to `names`.
    values: Vec<String>,
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
    Diagram(Box<DiagramTab>),
    Metrics(Box<MetricsTab>),
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
            TabKind::Diagram(_) => "Diagram".to_owned(),
            TabKind::Metrics(_) => "Metrics".to_owned(),
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
    /// Open another database of this connection in a new workspace.
    OpenDatabase(String),
    /// The safe mode level of the connection changed (to be saved).
    SetSafeLevel(dbzilla_core::safemode::SafeLevel),
    /// Edit the connection in the connection list.
    EditConnection,
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
    pub schema_loaded: bool,
    history: HistoryStore,
    diagrams: DiagramStore,
    metrics: MetricsStore,
    queries: QueryStore,
    history_cache: Vec<HistoryEntry>,
    history_for: Option<String>,
    history_dirty: bool,
    queries_cache: Vec<QueryEntry>,
    queries_sort: QuerySort,
    queries_dirty: bool,
    saved_keywords: HashMap<String, String>,
    save_dialog: Option<SaveDialog>,
    requested_structs: HashSet<String>,
    commit_log: HashMap<RequestId, (Vec<String>, Option<String>)>,
    ddl_after: HashMap<RequestId, DdlAfter>,
    search: Option<SearchState>,
    db_picker: Option<DbPicker>,
    jobs: JobCenter,
    job_env: Option<JobEnv>,
    repaint: Option<Arc<dyn Fn() + Send + Sync>>,
    export_dialog: Option<(ExportDialog, Option<String>)>,
    import_dialog: Option<ImportDialog>,
    db_dialog: Option<DbDialog>,
    level_dialog: Option<LevelDialog>,
    timeout_req: Option<RequestId>,
    outputs: Vec<WsOutput>,
    op_dialog: Option<OpDialog>,
    fk_dialog: Option<FkDialog>,
    link_dialog: Option<LinkDialog>,
    /// The kind chosen the last time, offered again next time.
    link_kind: LinkKind,
    triggers_for: Option<TabId>,
    pending_copy: Option<(TableRef, ScriptKind)>,
    clipboard_out: Option<String>,
    /// The process list sheet, when open.
    process: Option<ProcessList>,
    new_table_no: usize,
    log_original: bool,
    sql_counter: usize,
    fmt_width: u8,
    fmt_with: dbzilla_store::settings::IndentWith,
    fmt_kind: dbzilla_store::settings::IndentType,
    fmt_upper: bool,
    pub details: DetailsState,
    gate: Option<GateModal>,
    params_modal: Option<ParamsModal>,
    /// Last value typed for each variable, offered again on the next run.
    param_values: std::collections::BTreeMap<String, String>,
    param_opts: dbzilla_core::params::ParamOptions,
    params_enabled: bool,
    preview: Option<Vec<String>>,
    key_dialog: Option<KeyDialog>,
    /// Messages for the toast area: (is_error, text).
    notices: Vec<(bool, String)>,
}

impl Workspace {
    /// Creates the workspace of a connection whose session is already connecting.
    pub fn new(conn: StoredConnection, session: SessionHandle, console_entries: usize, page_size: u64, password: Option<String>, stores: Stores) -> Self {
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
            schema_loaded: false,
            history: stores.history,
            diagrams: stores.diagrams,
            metrics: stores.metrics,
            queries: stores.queries,
            history_cache: Vec::new(),
            history_for: None,
            history_dirty: true,
            queries_cache: Vec::new(),
            queries_sort: QuerySort::Name,
            queries_dirty: true,
            saved_keywords: HashMap::new(),
            save_dialog: None,
            requested_structs: HashSet::new(),
            commit_log: HashMap::new(),
            ddl_after: HashMap::new(),
            search: None,
            db_picker: None,
            jobs: JobCenter::default(),
            job_env: None,
            repaint: None,
            export_dialog: None,
            import_dialog: None,
            db_dialog: None,
            level_dialog: None,
            timeout_req: None,
            outputs: Vec::new(),
            op_dialog: None,
            fk_dialog: None,
            link_dialog: None,
            link_kind: LinkKind::DiagramOnly,
            triggers_for: None,
            pending_copy: None,
            clipboard_out: None,
            process: None,
            new_table_no: 0,
            log_original: false,
            sql_counter: 0,
            fmt_width: 4,
            fmt_with: dbzilla_store::settings::IndentWith::Spaces,
            fmt_kind: dbzilla_store::settings::IndentType::Standard,
            fmt_upper: true,
            details: DetailsState::default(),
            gate: None,
            params_modal: None,
            param_values: std::collections::BTreeMap::new(),
            param_opts: dbzilla_core::params::ParamOptions::default(),
            params_enabled: false,
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
        if let Some(p) = self.process.as_mut() {
            p.poll();
        }
        for (title, state) in self.jobs.poll() {
            match state {
                JobState::Done(t) => self.notices.push((false, format!("{title}: {t}"))),
                JobState::Failed(e) => self.notices.push((true, format!("{title} failed: {e}"))),
                JobState::Cancelled => self.notices.push((false, format!("{title} cancelled"))),
                JobState::Running { .. } => {}
            }
        }
    }

    /// Gives the workspace what background jobs need to open their own connection.
    pub fn set_job_env(&mut self, env: JobEnv, repaint: Arc<dyn Fn() + Send + Sync>) {
        self.job_env = Some(env);
        self.repaint = Some(repaint);
    }

    /// Opens the export dialog, optionally with a table ticked.
    pub fn open_export(&mut self, table: Option<TableRef>) {
        let columns = table.as_ref().and_then(|t| self.structures.get(t)).map(|s| s.columns.iter().map(|c| c.name.clone()).collect());
        self.export_dialog = Some((ExportDialog::new(&self.objects, table.as_ref(), columns), None));
    }

    /// Opens the import dialog, optionally targeting a table.
    pub fn open_import(&mut self, table: Option<TableRef>) {
        self.import_dialog = Some(ImportDialog::new(&self.objects, table));
    }

    /// Export and import dialogs.
    fn job_dialogs(&mut self, ctx: &egui::Context) {
        if let Some((d, err)) = self.export_dialog.as_mut() {
            match jobs::export_dialog(ctx, d, err) {
                Some(true) => match d.build() {
                    Ok((sources, opts)) => {
                        let title = format!("Export of {}", d.tables.iter().filter(|(_, on)| *on).map(|(t, _)| t.name.as_str()).collect::<Vec<_>>().join(", "));
                        self.export_dialog = None;
                        self.start_export(title, sources, opts);
                    }
                    Err(e) => *err = Some(e),
                },
                Some(false) => self.export_dialog = None,
                None => {}
            }
        }
        if let Some(d) = self.import_dialog.as_mut() {
            match jobs::import_dialog(ctx, d, self.conn.driver, self.repaint.clone().unwrap_or_else(|| Arc::new(|| {}))) {
                Some(true) => match d.build(self.current_schema.as_deref().filter(|_| self.caps.schemas)) {
                    Ok(req) => {
                        let title = format!("Import of {}", std::path::Path::new(d.path.trim()).file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()));
                        self.import_dialog = None;
                        self.start_import(title, req);
                    }
                    Err(e) => d.error = Some(e),
                },
                Some(false) => self.import_dialog = None,
                None => {}
            }
        }
    }

    /// Starts an export job.
    pub fn start_export(&mut self, title: String, sources: Vec<dbzilla_io::export::ExportSource>, opts: dbzilla_io::export::ExportOptions) {
        let (Some(env), Some(repaint)) = (self.job_env.clone(), self.repaint.clone()) else {
            return self.notices.push((true, "Background jobs are not available for this workspace".into()));
        };
        self.jobs.spawn_export(&env, repaint, title, sources, opts);
    }

    /// Starts an import job after the safe-mode check of what it will run.
    pub fn start_import(&mut self, title: String, req: jobs::ImportRequest) {
        let statements: Vec<String> = match &req {
            jobs::ImportRequest::Dump(text) => dbzilla_core::splitter::split_statements(text, self.conn.driver).into_iter().map(|s| s.text).collect(),
            jobs::ImportRequest::Rows { table, parsed, .. } => vec![format!("INSERT INTO {} VALUES (/* {} rows from the file */)", dialect_for(self.conn.driver).quote_table(table), parsed.rows.len())],
        };
        match gate_check(self.conn.safe_level, self.conn.driver, &statements) {
            GateDecision::Allow => self.spawn_import_now(title, req),
            GateDecision::Confirm(t) => self.gate = Some(GateModal { action: GateAction::Import(title, req), statements: t, needs_password: false, input: String::new(), reveal: false, tab: 0, error: None }),
            GateDecision::Password(t) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::Import(title, req), statements: t, needs_password: known, input: String::new(), reveal: false, tab: 0, error: None });
            }
        }
    }

    /// Starts an import job; the table list refreshes when it ends.
    fn spawn_import_now(&mut self, title: String, req: jobs::ImportRequest) {
        let (Some(env), Some(repaint)) = (self.job_env.clone(), self.repaint.clone()) else {
            return self.notices.push((true, "Background jobs are not available for this workspace".into()));
        };
        self.jobs.spawn_import(&env, repaint, title, req);
    }

    /// Applies one event to the state.
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Connected { server_version } => {
                self.status = WsStatus::Connected(server_version);
                self.load_schema();
                if self.conn.driver != DriverId::Sqlite && self.current_db.is_empty() {
                    self.db_picker = Some(DbPicker { query: String::new(), selected: None });
                }
            }
            Event::ConnectFailed { error } => self.status = WsStatus::Failed(error),
            Event::Disconnected { reason } => self.status = WsStatus::Disconnected(reason),
            Event::Databases(d) => self.databases = d,
            Event::Schema { schema, schemas, objects, .. } => {
                self.schema_loaded = true;
                self.current_schema = schema;
                self.schemas = schemas;
                self.objects = objects;
            }
            Event::Structure(st) => self.apply_structure(*st),
            Event::Diagram { tables, skipped, .. } => {
                let key = self.diagram_key();
                let (saved, links) = (self.diagrams.load(&key), self.diagrams.load_links(&key));
                let fresh = self.diagram_tab_mut().and_then(|d| d.set_tables(tables, skipped, &saved, links));
                if let Some(positions) = fresh {
                    self.save_diagram_positions(positions);
                }
            }
            Event::StatementStarted { req, index, sql } => self.on_statement_started(req, index, sql),
            Event::RowBatch { req, index, columns, rows } => match self.req_map.get(&req).copied() {
                Some((_, ReqKind::Metric(obj))) => {
                    if let Some(m) = self.metrics_tab_mut() {
                        m.on_rows(obj, columns, rows);
                    }
                }
                Some((id, ReqKind::Sql)) => self.on_sql_rows(id, index, columns, rows),
                _ => self.on_rows(req, columns, rows),
            },
            Event::QueryDone { req, index, summary } => match self.req_map.get(&req).copied() {
                Some((_, ReqKind::Metric(_))) => {}
                Some((id, ReqKind::Sql)) => self.on_sql_done(id, index, ResStatus::Done { elapsed: summary.elapsed, affected: summary.affected, messages: summary.messages }),
                _ => self.on_page_done(req, summary.elapsed),
            },
            Event::QueryError { req, index, error, cancelled } => match self.req_map.get(&req).copied() {
                Some((_, ReqKind::Metric(obj))) => {
                    if let Some(m) = self.metrics_tab_mut() {
                        m.on_error(obj, error);
                    }
                }
                Some((id, ReqKind::Sql)) => self.on_sql_done(id, index, if cancelled { ResStatus::Cancelled } else { ResStatus::Error(error) }),
                _ => self.on_page_error(req, error),
            },
            Event::RunFinished { req } => self.on_run_finished(req),
            Event::Timeout { req } => self.timeout_req = Some(req),
            Event::DbOpDone { what } => {
                self.notices.push((false, format!("Done: {what}")));
                self.session.send(Command::LoadDatabases);
            }
            Event::DbOpFailed { what, error } => self.notices.push((true, format!("Could not {what}: {error}"))),
            Event::DatabaseChanged(db) => {
                if let Some(env) = self.job_env.as_mut() {
                    // later imports and exports must hit the database now in use
                    env.cfg.database = if env.cfg.driver == DriverId::Sqlite { std::path::Path::new(&env.cfg.database).with_file_name(&db).to_string_lossy().into_owned() } else { db.clone() };
                }
                self.current_db = db;
                self.objects.clear();
                self.structures.clear();
                self.retain_tabs(|_| false);
                self.load_schema();
            }
            Event::Count { req, count } => self.on_total(req, Total::Exact(count)),
            Event::Estimate { req, count } => self.on_total(req, count.map_or(Total::Unknown, Total::Estimated)),
            Event::CommitDone { req, counts } => self.on_commit_done(req, counts.len()),
            Event::CommitError { req, error } => self.on_commit_error(req, error),
            Event::Console { ts_ms, sql } => self.console.push(ts_ms, sql),
            Event::Error { error } => {
                self.console.push(0, format!("-- error: {error}"));
            }
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
            TabKind::Sql(_) | TabKind::Diagram(_) | TabKind::Metrics(_) => None,
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
        if let Some((_, ReqKind::Search)) = self.req_map.get(&req).copied() {
            self.req_map.remove(&req);
            if let Some(sr) = self.search.as_mut() {
                sr.skip(error);
            }
            return self.search_step();
        }
        let Some((id, _)) = self.req_map.remove(&req) else { return };
        if let Some(t) = self.table_tab_mut(id) {
            t.data.loading = false;
            t.data.error = Some(error);
        }
    }

    /// A count or estimate arrived.
    fn on_total(&mut self, req: RequestId, total: Total) {
        if let Some((_, ReqKind::Search)) = self.req_map.get(&req).copied() {
            self.req_map.remove(&req);
            if let (Some(sr), Total::Exact(n)) = (self.search.as_mut(), total) {
                sr.record(n);
            }
            return self.search_step();
        }
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
                if t.draft.as_ref().is_none_or(|d| !d.has_changes() && !d.is_new) {
                    t.draft = Some(StructDraft::from_structure(&st));
                }
                if !t.ddl_dirty() {
                    t.ddl_text = st.ddl.clone();
                }
            }
        }
        let search_waiting = self.search.as_ref().is_some_and(|s| s.awaiting_structure.as_ref() == Some(&st.table));
        if let Some((table, kind)) = self.pending_copy.take() {
            if table == st.table {
                self.clipboard_out = Some(copy_script(kind, &st, self.dialect.as_ref()));
                self.notices.push((false, "Script copied".into()));
            } else {
                self.pending_copy = Some((table, kind));
            }
        }
        self.structures.insert(st.table.clone(), st);
        if search_waiting {
            if let Some(sr) = self.search.as_mut() {
                sr.awaiting_structure = None;
            }
            self.search_count_current();
        }
    }

    /// Facts about the open tabs for the opening rules.
    fn tab_metas(&self) -> Vec<TabMeta> {
        self.dock
            .iter_all_tabs()
            .map(|(_, t)| match &t.kind {
                TabKind::Table(tt) => TabMeta { id: t.id, target: TabTarget::Table(tt.table.clone()), pinned: t.pinned, dirty: false },
                TabKind::Sql(sq) => TabMeta { id: t.id, target: TabTarget::Sql(t.id), pinned: true, dirty: !sq.text.is_empty() },
                TabKind::Diagram(_) => TabMeta { id: t.id, target: TabTarget::Diagram, pinned: true, dirty: false },
                TabKind::Metrics(_) => TabMeta { id: t.id, target: TabTarget::Metrics, pinned: true, dirty: false },
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
            TabKind::Sql(_) | TabKind::Diagram(_) | TabKind::Metrics(_) => None,
        })
    }

    /// Opens a table or view following the preview-tab rules.
    pub fn open_table(&mut self, table: TableRef, mode: OpenMode) {
        self.open_table_with(table, mode, Vec::new());
    }

    /// Opens a table with initial filters (used when following a foreign key).
    pub fn open_table_with(&mut self, table: TableRef, mode: OpenMode, filters: Vec<Filter>) {
        if let Some(kind) = self.objects.iter().find(|o| o.table == table).map(|o| o.kind).filter(|k| matches!(k, ObjectKind::Function | ObjectKind::Procedure)) {
            return self.open_definition(&table, kind);
        }
        let metas = self.tab_metas();
        let focused = self.focused_tab();
        let decision = decide_open(&metas, focused, &TabTarget::Table(table.clone()), mode);
        let new_tab = |ws: &mut Workspace, pinned: bool| {
            let id = ws.next_tab_id;
            ws.next_tab_id += 1;
            let mut data = DataState::new(ws.page_size);
            data.filters = filters.clone();
            data.bar.rows = filters.clone();
            data.bar.open = !filters.is_empty();
            let is_view = ws.objects.iter().any(|o| o.table == table && matches!(o.kind, ObjectKind::View | ObjectKind::MaterializedView));
            if let Some(st) = ws.structures.get(&table) {
                data.key_cols = default_key_columns(Some(st));
            }
            let is_matview = ws.objects.iter().any(|o| o.table == table && o.kind == ObjectKind::MaterializedView);
            let ddl_text = ws.structures.get(&table).map(|s| s.ddl.clone()).unwrap_or_default();
            Tab { id, pinned, kind: TabKind::Table(Box::new(TableTab { structure: ws.structures.get(&table).cloned(), table: table.clone(), error: None, data, is_view, is_matview, view: TabView::Data, draft: None, ddl_text, ddl_commit: false, rename_to: None })) }
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
            Action::ToggleFilters => {
                if let Some((_, t)) = self.active_tab_mut() {
                    t.data.bar.open = !t.data.bar.open;
                } else if let Some((_, s)) = self.active_sql_mut() {
                    s.find = if s.find.is_some() { None } else { Some(sqltab::FindState { focus: true, ..Default::default() }) };
                }
            }
            Action::OpenDatabase => self.db_picker = Some(DbPicker { query: String::new(), selected: None }),
            Action::SearchInDatabase => {
                let tables: Vec<TableRef> = self.objects.iter().filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View)).map(|o| o.table.clone()).collect();
                self.search = Some(SearchState::new(&tables));
            }
            Action::ShowDiagram => self.open_diagram(),
            Action::ShowMetrics => self.open_metrics(),
            Action::ProcessList => self.open_process_list(),
            Action::ShowData => self.set_view(TabView::Data),
            Action::ShowStructure => self.set_view(TabView::Structure),
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
                TabKind::Sql(_) | TabKind::Diagram(_) | TabKind::Metrics(_) => None,
            }
        })
    }

    /// The focused SQL tab.
    fn active_sql_mut(&mut self) -> Option<(TabId, &mut SqlTab)> {
        self.dock.find_active_focused().and_then(|(_, t)| {
            let id = t.id;
            match &mut t.kind {
                TabKind::Sql(sq) => Some((id, sq.as_mut())),
                TabKind::Table(_) | TabKind::Diagram(_) | TabKind::Metrics(_) => None,
            }
        })
    }

    /// SQL tab with the given id.
    fn sql_tab_mut(&mut self, id: TabId) -> Option<&mut SqlTab> {
        self.dock.iter_all_tabs_mut().map(|(_, t)| t).find(|t| t.id == id).and_then(|t| match &mut t.kind {
            TabKind::Sql(sq) => Some(sq.as_mut()),
            TabKind::Table(_) | TabKind::Diagram(_) | TabKind::Metrics(_) => None,
        })
    }

    /// The ER diagram tab, if open.
    fn diagram_tab_mut(&mut self) -> Option<&mut DiagramTab> {
        self.dock.iter_all_tabs_mut().map(|(_, t)| t).find_map(|t| match &mut t.kind {
            TabKind::Diagram(d) => Some(d.as_mut()),
            _ => None,
        })
    }

    /// Key under which the positions of the current diagram are saved.
    fn diagram_key(&self) -> String {
        let schema = if self.caps.schemas { Some(self.current_schema.as_deref().unwrap_or("public")) } else { None };
        let c = &self.conn;
        let server = format!("{:?}://{}@{}:{}/{}", c.driver, c.user, c.host, c.port.map(|p| p.to_string()).unwrap_or_default(), c.database);
        DiagramStore::key(&server, &self.current_db, schema)
    }

    /// Keeps the table positions of the current diagram; a failure becomes a notice.
    fn save_diagram_positions(&mut self, positions: dbzilla_store::diagrams::Positions) {
        if let Err(e) = self.diagrams.save(&self.diagram_key(), positions) {
            self.notices.push((true, format!("cannot save the diagram positions: {e}")));
        }
    }

    /// Keeps the links drawn in the current diagram; a failure becomes a notice.
    fn save_diagram_links(&mut self) {
        let Some(links) = self.diagram_tab_mut().map(|d| d.links().to_vec()) else { return };
        if let Err(e) = self.diagrams.save_links(&self.diagram_key(), links) {
            self.notices.push((true, format!("cannot save the diagram links: {e}")));
        }
    }

    /// Applies the link dialog: a real foreign key goes through the safe-mode gate, a drawn link is kept.
    /// On a problem the dialog stays open with the reason.
    fn apply_link_dialog(&mut self, dialog: LinkDialog) {
        self.link_kind = dialog.kind;
        let reopen = |ws: &mut Self, mut d: LinkDialog, why: String| {
            d.error = Some(why);
            ws.link_dialog = Some(d);
        };
        match dialog.kind {
            LinkKind::DiagramOnly => {
                let added = self.diagram_tab_mut().is_some_and(|d| d.add_link(dialog.link.clone()));
                if added {
                    self.save_diagram_links();
                } else {
                    self.notices.push((false, "That link is already in the diagram".into()));
                }
            }
            LinkKind::Database => {
                let l = &dialog.link;
                let (Some(from), Some(to)) = (self.diagram_tab_mut().and_then(|d| d.structure(&l.from_table).cloned()), self.diagram_tab_mut().and_then(|d| d.structure(&l.to_table).cloned())) else {
                    return reopen(self, dialog, "The table is no longer in the diagram".into());
                };
                let name = dialog.constraint.trim();
                let op = StructuralOp::AddForeignKey {
                    table: from.table.clone(),
                    fk: ForeignKeyDef { name: (!name.is_empty()).then(|| name.to_owned()), columns: vec![l.from_column.clone()], ref_table: to.table.clone(), ref_columns: vec![l.to_column.clone()], on_update: dialog.on_update.clone(), on_delete: dialog.on_delete.clone() },
                    current: Some(Box::new(from)),
                };
                if let Err(e) = generate_ddl(&op, dialect_for(self.conn.driver).as_ref()) {
                    return reopen(self, dialog, e.to_string());
                }
                self.submit_ddl(vec![op], DdlAfter { closed: None, message: format!("Foreign key from {}.{} to {}.{} created", l.from_table, l.from_column, l.to_table, l.to_column) });
            }
        }
    }

    /// Asks the session for every table of the current schema.
    fn request_diagram(&mut self) {
        let schema = if self.caps.schemas { Some(self.current_schema.clone().unwrap_or_else(|| "public".into())) } else { None };
        self.session.send(Command::LoadDiagram { db: self.current_db.clone(), schema });
    }

    /// Opens the ER diagram of the current schema, or focuses the one already open.
    pub fn open_diagram(&mut self) {
        let existing = self.tab_metas().into_iter().find(|t| t.target == TabTarget::Diagram).map(|t| t.id);
        if let Some(id) = existing {
            if let Some(loc) = self.tab_location(id) {
                self.dock.set_active_tab(loc);
                self.dock.set_focused_node_and_surface((loc.0, loc.1));
            }
            return;
        }
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.dock.push_to_focused_leaf(Tab { id, pinned: true, kind: TabKind::Diagram(Box::new(DiagramTab::new())) });
        if let Some(loc) = self.tab_location(id) {
            self.dock.set_active_tab(loc);
            self.dock.set_focused_node_and_surface((loc.0, loc.1));
        }
        self.request_diagram();
    }

    /// Opens the process list on its own connection, or says why it cannot.
    pub fn open_process_list(&mut self) {
        if !dbzilla_core::processes::supported(self.conn.driver) {
            self.notices.push((false, "The process list is not available for this database".into()));
            return;
        }
        if self.process.is_some() {
            return;
        }
        let (Some(env), Some(repaint)) = (self.job_env.as_ref(), self.repaint.clone()) else {
            self.notices.push((true, "The process list needs a connection".into()));
            return;
        };
        self.process = Some(ProcessList::open(env, repaint));
    }

    /// Draws the process list sheet and acts on its requests.
    fn process_sheet(&mut self, ctx: &egui::Context) {
        let Some(list) = self.process.as_mut() else { return };
        match process::show(ctx, list) {
            Some(ProcessAction::Dismiss) => self.process = None,
            Some(ProcessAction::Kill(ids)) => self.submit_kill(ids),
            None => {}
        }
    }

    /// Stops the queries of these sessions, after the safe mode agrees. A kill counts as a write.
    fn submit_kill(&mut self, ids: Vec<String>) {
        let driver = self.conn.driver;
        let statements: Vec<String> = ids.iter().filter_map(|id| dbzilla_core::processes::kill_sql(driver, id)).collect();
        if statements.is_empty() {
            return;
        }
        let level = self.conn.safe_level;
        let kind = dbzilla_core::safemode::StatementKind::Write;
        let modal = |needs_password: bool, statements| GateModal { action: GateAction::Kill, statements, needs_password, input: String::new(), reveal: false, tab: 0, error: None };
        if level.requires_password(kind) {
            let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
            self.gate = Some(modal(known, statements));
        } else if level.requires_confirm(kind) {
            self.gate = Some(modal(false, statements));
        } else {
            self.kill_now(statements);
        }
    }

    /// Sends the kill statements on the process list connection.
    fn kill_now(&mut self, statements: Vec<String>) {
        if let Some(p) = self.process.as_mut() {
            p.kill(statements);
        }
    }

    /// The metrics board tab, if open.
    fn metrics_tab_mut(&mut self) -> Option<&mut MetricsTab> {
        self.dock.iter_all_tabs_mut().map(|(_, t)| t).find_map(|t| match &mut t.kind {
            TabKind::Metrics(m) => Some(m.as_mut()),
            _ => None,
        })
    }

    /// Opens the metrics board, or focuses the one already open.
    pub fn open_metrics(&mut self) {
        let existing = self.tab_metas().into_iter().find(|t| t.target == TabTarget::Metrics).map(|t| t.id);
        let id = match existing {
            Some(id) => id,
            None => {
                let id = self.next_tab_id;
                self.next_tab_id += 1;
                self.dock.push_to_focused_leaf(Tab { id, pinned: true, kind: TabKind::Metrics(Box::new(MetricsTab::new(self.metrics.load()))) });
                id
            }
        };
        if let Some(loc) = self.tab_location(id) {
            self.dock.set_active_tab(loc);
            self.dock.set_focused_node_and_surface((loc.0, loc.1));
        }
    }

    /// Reacts to a request of the metrics board tab.
    fn metrics_action(&mut self, id: TabId, a: MetricsAction) {
        match a {
            MetricsAction::Close => self.retain_tabs(|t| t.id != id),
            MetricsAction::Run { object, sql } => {
                let req = RequestId::next();
                self.req_map.insert(req, (id, ReqKind::Metric(object)));
                self.session.send(Command::RunSql { req, statements: vec![sql], opts: dbzilla_drivers::QueryOpts::default(), stop_on_error: true });
            }
            MetricsAction::Save(boards) => {
                if let Err(e) = self.metrics.save(&boards) {
                    self.notices.push((true, format!("cannot save the metrics boards: {e}")));
                }
            }
            MetricsAction::Notice(error, text) => self.notices.push((error, text)),
        }
    }

    /// Reacts to a request of the diagram tab.
    fn diagram_action(&mut self, id: TabId, a: DiagramAction) {
        match a {
            DiagramAction::Close => self.retain_tabs(|t| t.id != id),
            DiagramAction::Rearrange => {
                if let Some(d) = self.diagram_tab_mut() {
                    d.start_rearrange();
                }
                self.request_diagram();
            }
            DiagramAction::Save(positions) => self.save_diagram_positions(positions),
            DiagramAction::RequestLink(link) => {
                self.link_dialog = Some(LinkDialog { link, kind: self.link_kind, constraint: String::new(), on_update: FK_ACTIONS[0].into(), on_delete: FK_ACTIONS[0].into(), error: None });
            }
            DiagramAction::DeleteLink(link) => {
                if let Some(d) = self.diagram_tab_mut() {
                    d.remove_link(&link);
                }
                self.save_diagram_links();
            }
            DiagramAction::OpenTable(name) => {
                let table = self.objects.iter().find(|o| o.table.name == name).map(|o| o.table.clone());
                if let Some(t) = table {
                    self.open_table(t, OpenMode::Pin);
                }
            }
        }
    }

    /// Opens a new SQL editor tab.
    pub fn open_sql(&mut self, text: String) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.sql_counter += 1;
        self.dock.push_to_focused_leaf(Tab { id, pinned: true, kind: TabKind::Sql(Box::new(SqlTab::new(self.sql_counter, text))) });
        if let Some(loc) = self.tab_location(id) {
            self.dock.set_active_tab(loc);
            self.dock.set_focused_node_and_surface((loc.0, loc.1));
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
        let raw: Vec<(String, Option<String>)> = stmts.into_iter().map(|s| (s.text, s.label)).collect();
        if self.params_enabled {
            let mut names: Vec<String> = Vec::new();
            for (text, _) in &raw {
                for n in dbzilla_core::params::param_names(text, self.param_opts) {
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
            }
            if !names.is_empty() {
                let values = names.iter().map(|n| self.param_values.get(n).cloned().unwrap_or_default()).collect();
                self.params_modal = Some(ParamsModal { tab: id, statements: raw, names, values, error: None });
                return;
            }
        }
        self.start_run(id, raw);
    }

    /// Stores the typed values, substitutes them in the statements and starts the run.
    fn apply_params(&mut self) {
        let Some(m) = self.params_modal.take() else { return };
        for (n, v) in m.names.iter().zip(&m.values) {
            self.param_values.insert(n.clone(), v.clone());
        }
        let values: std::collections::BTreeMap<String, String> = m.names.iter().cloned().zip(m.values.iter().cloned()).collect();
        let mut out = Vec::with_capacity(m.statements.len());
        for (text, label) in &m.statements {
            match dbzilla_core::params::substitute(text, self.param_opts, &values) {
                Ok(t) => out.push((t, label.clone())),
                Err(e) => {
                    self.notices.push((true, e.to_string()));
                    return;
                }
            }
        }
        self.start_run(m.tab, out);
    }

    /// Forgets the remembered values of variables that the current dialog does not use.
    fn cleanup_params(&mut self) {
        let Some(m) = self.params_modal.as_ref() else { return };
        let keep: Vec<String> = m.names.clone();
        self.param_values.retain(|k, _| keep.contains(k));
        self.notices.push((false, "Unused variables forgotten".into()));
    }

    /// Applies the row limit and the safe-mode gate to statements ready to run.
    fn start_run(&mut self, id: TabId, stmts: Vec<(String, Option<String>)>) {
        let driver = self.conn.driver;
        let Some(limit) = self.sql_tab_mut(id).map(|t| t.row_limit) else { return };
        let texts: Vec<String> = stmts.iter().map(|(t, _)| apply_row_limit(t, limit)).collect();
        let labels: Vec<Option<String>> = stmts.iter().map(|(_, l)| l.clone()).collect();
        let originals: Vec<String> = stmts.into_iter().map(|(t, _)| t).collect();
        match gate_check(self.conn.safe_level, driver, &texts) {
            GateDecision::Allow => self.send_run(id, texts, labels, originals),
            GateDecision::Confirm(t) => self.gate = Some(GateModal { action: GateAction::Run { labels, originals }, statements: t, needs_password: false, input: String::new(), reveal: false, tab: id, error: None }),
            GateDecision::Password(t) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::Run { labels, originals }, statements: t, needs_password: known, input: String::new(), reveal: false, tab: id, error: None });
            }
        }
    }

    /// Sends the statements of a SQL tab to the session.
    fn send_run(&mut self, id: TabId, statements: Vec<String>, labels: Vec<Option<String>>, originals: Vec<String>) {
        let req = RequestId::next();
        let Some(tab) = self.sql_tab_mut(id) else { return };
        tab.results.retain(|r| r.pinned);
        tab.active_result = 0;
        tab.running = true;
        tab.run_req = Some(req);
        tab.labels = labels;
        tab.run_texts = originals;
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
        let sql = tab.run_texts.get(index).cloned();
        let (ok, ms) = match &status {
            ResStatus::Done { elapsed, .. } => (true, Some(elapsed.as_millis() as u64)),
            _ => (false, None),
        };
        if let Some(r) = tab.results.iter_mut().rev().find(|r| r.index == index && r.status == ResStatus::Running) {
            r.status = status;
        }
        if let Some(sql) = sql {
            let entry = HistoryEntry { ts: now_ms(), sql: dbzilla_session::console::redact_sql(&sql), kind: HistoryKind::Editor, duration_ms: ms, ok, database: Some(self.current_db.clone()), original_csv: None };
            if let Err(e) = self.history.append(&self.conn.id, &entry) {
                self.notices.push((true, format!("Cannot write the history: {e}")));
            }
            self.history_dirty = true;
        }
    }

    /// The whole run finished.
    fn on_run_finished(&mut self, req: RequestId) {
        if let Some((_, ReqKind::Metric(obj))) = self.req_map.get(&req).copied() {
            self.req_map.remove(&req);
            if let Some(m) = self.metrics_tab_mut() {
                m.on_finished(obj);
            }
            return;
        }
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
            SqlAction::SaveQuery => {
                let driver = self.conn.driver;
                if let Some((_, t)) = self.active_sql_mut() {
                    let sql = t.statements_to_run(false, driver).into_iter().next().map_or_else(|| t.text.clone(), |s| s.text);
                    self.save_dialog = Some(SaveDialog { name: String::new(), keywords: String::new(), sql });
                }
            }
            SqlAction::NeedStructure(name) => {
                let key = name.to_lowercase();
                if self.requested_structs.insert(key) {
                    if let Some(o) = self.objects.iter().find(|o| o.table.name.eq_ignore_ascii_case(&name)) {
                        self.session.send(Command::LoadStructure(o.table.clone()));
                    }
                }
            }
            SqlAction::Toast(m) => self.notices.push((false, m)),
        }
    }

    /// Refreshes the cached history and saved query lists when needed.
    fn refresh_lists(&mut self) {
        let query = self.sidebar.search.clone();
        if self.history_dirty || self.history_for.as_deref() != Some(query.as_str()) && self.sidebar.tab == SidebarTab::History {
            self.history_cache = self.history.search(&self.conn.id, &query, 200).unwrap_or_default();
            self.history_for = Some(query);
            self.history_dirty = false;
        }
        if self.queries_dirty {
            self.queries_cache = self.queries.list(self.queries_sort).unwrap_or_default();
            self.saved_keywords = self.queries.keyword_map().unwrap_or_default();
            self.queries_dirty = false;
        }
    }

    /// Opens a saved query in its own tab (or focuses the one already open).
    pub fn open_query(&mut self, rel: &str) {
        let existing = self.dock.iter_all_tabs().find(|(_, t)| matches!(&t.kind, TabKind::Sql(s) if s.file.as_deref() == Some(rel))).map(|(_, t)| t.id);
        if let Some(id) = existing {
            if let Some(loc) = self.tab_location(id) {
                self.dock.set_active_tab(loc);
                self.dock.set_focused_node_and_surface((loc.0, loc.1));
            }
            return;
        }
        match self.queries.read(rel) {
            Ok(q) => {
                self.open_sql(q.sql.clone());
                if let Some((_, t)) = self.active_sql_mut() {
                    t.file = Some(rel.to_owned());
                    t.query_keywords = q.keywords;
                    t.saved_text = q.sql;
                }
            }
            Err(e) => self.notices.push((true, format!("Cannot open the query: {e}"))),
        }
    }

    /// Actions of the Queries and History tabs.
    fn handle_extra(&mut self, ctx: &egui::Context, a: ExtraAction) {
        match a {
            ExtraAction::OpenQuery(rel) => self.open_query(&rel),
            ExtraAction::InsertQuery(rel) => match self.queries.read(&rel) {
                Ok(q) => match self.active_sql_mut() {
                    Some((_, t)) => {
                        let at = t.selection.0.min(t.text.len());
                        let at = if t.text.is_char_boundary(at) { at } else { t.text.len() };
                        t.text.insert_str(at, &q.sql);
                    }
                    None => self.open_sql(q.sql),
                },
                Err(e) => self.notices.push((true, e.to_string())),
            },
            ExtraAction::RunQuery(rel) => match self.queries.read(&rel) {
                Ok(q) => {
                    self.open_sql(q.sql);
                    self.run_sql(true);
                }
                Err(e) => self.notices.push((true, e.to_string())),
            },
            ExtraAction::DeleteQuery(rel) => {
                if let Err(e) = self.queries.delete(&rel) {
                    self.notices.push((true, e.to_string()));
                }
                self.queries_dirty = true;
            }
            ExtraAction::NewQuery => self.save_dialog = Some(SaveDialog { name: String::new(), keywords: String::new(), sql: String::new() }),
            ExtraAction::SortQueries(s) => {
                self.queries_sort = s;
                self.queries_dirty = true;
            }
            ExtraAction::HistoryInsert(i) => {
                if let Some(e) = self.history_cache.get(i).cloned() {
                    self.open_sql(e.sql);
                }
            }
            ExtraAction::HistoryRun(i) => {
                if let Some(e) = self.history_cache.get(i).cloned() {
                    self.open_sql(e.sql);
                    self.run_sql(true);
                }
            }
            ExtraAction::HistoryCopy(i) => {
                if let Some(e) = self.history_cache.get(i) {
                    ctx.copy_text(e.sql.clone());
                    self.notices.push((false, "Statement copied".into()));
                }
            }
            ExtraAction::HistoryAddToQueries(i) => {
                if let Some(e) = self.history_cache.get(i).cloned() {
                    self.save_dialog = Some(SaveDialog { name: String::new(), keywords: String::new(), sql: e.sql });
                }
            }
            ExtraAction::HistoryClear => {
                if let Err(e) = self.history.clear(&self.conn.id) {
                    self.notices.push((true, e.to_string()));
                }
                self.history_dirty = true;
            }
        }
    }

    /// Writes edited saved queries back to their files once typing pauses.
    fn autosave_queries(&mut self, ctx: &egui::Context, enabled: bool) {
        if !enabled {
            return;
        }
        let Workspace { dock, queries, queries_dirty, notices, .. } = self;
        let mut pending = false;
        for (_, tab) in dock.iter_all_tabs_mut() {
            let TabKind::Sql(t) = &mut tab.kind else { continue };
            let Some(rel) = t.file.clone() else { continue };
            if t.text == t.saved_text {
                continue;
            }
            if t.last_edit.is_some_and(|at| at.elapsed() > std::time::Duration::from_millis(600)) {
                match queries.save(&rel, &t.query_keywords, &t.text) {
                    Ok(()) => {
                        t.saved_text = t.text.clone();
                        *queries_dirty = true;
                    }
                    Err(e) => notices.push((true, format!("Cannot save {rel}: {e}"))),
                }
            } else {
                pending = true;
            }
        }
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(700));
        }
    }

    /// True when the focused tab has pending changes.
    pub fn has_pending(&mut self) -> bool {
        self.active_tab_mut().is_some_and(|(_, t)| match t.view {
            TabView::Data => t.data.pending.has_changes(),
            TabView::Structure | TabView::Index => t.draft.as_ref().is_some_and(StructDraft::has_changes),
            TabView::Ddl => t.ddl_dirty(),
        })
    }

    /// Statements for the pending changes of a tab.
    fn statements_of(&mut self, id: TabId) -> Result<Vec<String>, String> {
        let driver = self.conn.driver;
        let caps = self.caps.clone();
        let Some(t) = self.table_tab_mut(id) else { return Err("tab closed".into()) };
        match t.view {
            TabView::Structure | TabView::Index => {
                let Some(d) = &t.draft else { return Ok(Vec::new()) };
                if !d.has_changes() {
                    return Ok(Vec::new());
                }
                let dialect = dialect_for(driver);
                let mut out = Vec::new();
                for op in d.to_ops(&caps)? {
                    out.extend(generate_ddl(&op, dialect.as_ref()).map_err(|e| e.to_string())?);
                }
                return Ok(out);
            }
            TabView::Ddl => {
                if !t.ddl_dirty() {
                    return Ok(Vec::new());
                }
                return Ok(view_apply_statements(driver, &t.table, &t.ddl_text));
            }
            TabView::Data => {}
        }
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
        let Some((id, t)) = self.active_tab_mut() else { return };
        match t.view {
            TabView::Data => self.fetch_tab_forced(id),
            TabView::Structure | TabView::Index => {
                if t.draft.as_ref().is_some_and(|d| d.is_new) {
                    self.retain_tabs(|x| x.id != id);
                } else {
                    t.draft = t.structure.as_ref().map(StructDraft::from_structure);
                }
            }
            TabView::Ddl => {
                if let Some(st) = &t.structure {
                    t.ddl_text = st.ddl.clone();
                }
            }
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
        let csv = if self.log_original { self.original_values_csv(id) } else { None };
        self.commit_log.insert(req, (statements.clone(), csv));
        if let Some(t) = self.table_tab_mut(id) {
            t.data.committing = true;
            t.ddl_commit = t.view != TabView::Data;
            t.rename_to = t.draft.as_ref().filter(|d| t.view != TabView::Data && !d.is_new && d.new_name.trim() != t.table.name).map(|d| d.new_name.trim().to_owned());
        }
        self.req_map.insert(req, (id, ReqKind::Commit));
        self.session.send(Command::CommitRows { req, statements });
    }

    /// A commit succeeded: reload the tab.
    fn on_commit_done(&mut self, req: RequestId, statements: usize) {
        if let Some((stmts, csv)) = self.commit_log.remove(&req) {
            let mut csv = csv;
            for sql in stmts {
                let writes_existing = sql.starts_with("UPDATE") || sql.starts_with("DELETE");
                let original_csv = if writes_existing { csv.take() } else { None };
                let entry = HistoryEntry { ts: now_ms(), sql: dbzilla_session::console::redact_sql(&sql), kind: HistoryKind::Commit, duration_ms: None, ok: true, database: Some(self.current_db.clone()), original_csv };
                if let Err(e) = self.history.append(&self.conn.id, &entry) {
                    self.notices.push((true, format!("Cannot write the history: {e}")));
                }
            }
            self.history_dirty = true;
        }
        match self.req_map.remove(&req) {
            Some((id, ReqKind::Commit)) => {
                self.notices.push((false, format!("{statements} {} committed", if statements == 1 { "statement" } else { "statements" })));
                self.after_tab_commit(id);
            }
            Some((_, ReqKind::Ddl)) => {
                let after = self.ddl_after.remove(&req).unwrap_or_default();
                if let Some(closed) = &after.closed {
                    let closed = closed.clone();
                    self.structures.remove(&closed);
                    self.retain_tabs(|t| !matches!(&t.kind, TabKind::Table(tt) if tt.table == closed));
                }
                self.notices.push((false, if after.message.is_empty() { "Done".into() } else { after.message }));
                self.load_schema();
                if let Some(d) = self.diagram_tab_mut() {
                    d.start_loading();
                    self.request_diagram();
                }
            }
            _ => {}
        }
    }

    /// After a tab commit: reload the data and, for structure changes, the structure and the item list.
    fn after_tab_commit(&mut self, id: TabId) {
        let Some(t) = self.table_tab_mut(id) else { return };
        t.data.committing = false;
        if !t.ddl_commit {
            return self.fetch_tab_forced(id);
        }
        t.ddl_commit = false;
        let old = t.table.clone();
        let new_name = t.rename_to.take().or_else(|| t.draft.as_ref().filter(|d| d.is_new).map(|d| d.new_name.trim().to_owned()));
        if let Some(n) = new_name {
            t.table.name = n;
        }
        if t.draft.as_ref().is_some_and(|d| d.is_new) {
            t.is_view = false;
        }
        t.draft = None;
        t.structure = None;
        let table = t.table.clone();
        self.structures.remove(&old);
        self.structures.remove(&table);
        self.session.send(Command::LoadStructure(table));
        self.load_schema();
        self.fetch_tab_forced(id);
    }

    /// A commit failed: nothing is lost, the changes stay pending.
    fn on_commit_error(&mut self, req: RequestId, error: String) {
        self.commit_log.remove(&req);
        self.ddl_after.remove(&req);
        let Some((id, kind)) = self.req_map.remove(&req) else { return };
        if kind == ReqKind::Commit {
            if let Some(t) = self.table_tab_mut(id) {
                t.data.committing = false;
                t.ddl_commit = false;
            }
        }
        self.notices.push((true, format!("Commit failed: {error}")));
    }

    /// Table tab showing `table`.
    fn find_table_tab(&mut self, table: &TableRef) -> Option<TabId> {
        self.dock.iter_all_tabs().find(|(_, t)| matches!(&t.kind, TabKind::Table(tt) if &tt.table == table)).map(|(_, t)| t.id)
    }

    /// Opens a table on a given view.
    fn open_table_view(&mut self, table: TableRef, view: TabView) {
        self.open_table(table.clone(), OpenMode::Pin);
        if let Some(id) = self.find_table_tab(&table) {
            if let Some(t) = self.table_tab_mut(id) {
                t.view = view;
            }
        }
    }

    /// Switches the focused table tab to a view.
    fn set_view(&mut self, view: TabView) {
        if let Some((_, t)) = self.active_tab_mut() {
            t.view = view;
        }
    }

    /// Applies filters to a table tab and reloads its first page.
    pub fn apply_filters(&mut self, id: TabId, filters: Vec<Filter>, or: bool) {
        let several = |f: &Filter| f.enabled && f.column == dbzilla_core::filter::FilterColumn::Raw && dbzilla_core::splitter::split_statements(&f.value, DriverId::MySql).len() > 1;
        if filters.iter().any(several) {
            return self.notices.push((true, "A raw filter must be a single expression, not several statements".into()));
        }
        if let Some(t) = self.table_tab_mut(id) {
            t.data.filters = filters;
            t.data.filters_or = or;
            t.data.offset = 0;
            t.data.total = Total::Unknown;
        }
        self.fetch_tab(id);
    }

    /// SQL text of a database operation, used for the safe-mode gate and the history.
    fn dbop_sql(&self, op: &dbzilla_session::DbOp) -> String {
        let d = dialect_for(self.conn.driver);
        match op {
            dbzilla_session::DbOp::Create(n) => format!("CREATE DATABASE {}", d.quote_ident(&n.name)),
            dbzilla_session::DbOp::Rename { from, to } => format!("ALTER DATABASE {} RENAME TO {}", d.quote_ident(from), d.quote_ident(to)),
            dbzilla_session::DbOp::Drop(n) => format!("DROP DATABASE {}", d.quote_ident(n)),
        }
    }

    /// Runs a database operation through the safe-mode gate.
    fn submit_dbop(&mut self, op: dbzilla_session::DbOp) {
        let statements = vec![self.dbop_sql(&op)];
        match gate_check(self.conn.safe_level, self.conn.driver, &statements) {
            GateDecision::Allow => self.session.send(Command::DbOp(op)),
            GateDecision::Confirm(t) => self.gate = Some(GateModal { action: GateAction::DbOp(op), statements: t, needs_password: false, input: String::new(), reveal: false, tab: 0, error: None }),
            GateDecision::Password(t) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::DbOp(op), statements: t, needs_password: known, input: String::new(), reveal: false, tab: 0, error: None });
            }
        }
    }

    /// Changes the safe mode level, asking for the password when leaving a protected level.
    fn request_safe_level(&mut self, level: dbzilla_core::safemode::SafeLevel) {
        
        if level == self.conn.safe_level {
            return;
        }
        let protected = matches!(self.conn.safe_level, SafeLevel::Safe1 | SafeLevel::Safe2);
        if protected && self.password.as_deref().is_some_and(|p| !p.is_empty()) {
            self.level_dialog = Some(LevelDialog { level, input: String::new(), reveal: false, error: None });
        } else {
            self.conn.safe_level = level;
            self.outputs.push(WsOutput::SetSafeLevel(level));
        }
    }

    /// Database picker, database operation dialogs, safe level and timeout dialogs.
    fn database_dialogs(&mut self, ctx: &egui::Context) {
        let caps = self.caps.clone();
        let driver = self.conn.driver;
        if let Some(p) = self.db_picker.as_mut() {
            let mut action: Option<(String, u8)> = None;
            let mut close = false;
            let mut new_clicked = false;
            let current = self.current_db.clone();
            let names: Vec<String> = self.databases.iter().map(|d| d.name.clone()).collect();
            widgets::modal(ctx, "Open a database", |ui| {
                ui.set_min_width(420.0);
                widgets::search_box(ui, &mut p.query, "Search databases");
                let q = p.query.to_lowercase();
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for n in names.iter().filter(|n| q.is_empty() || n.to_lowercase().contains(&q)) {
                        let selected = p.selected.as_deref() == Some(n.as_str());
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
                        if selected || resp.hovered() {
                            ui.painter().rect_filled(rect, theme::RADIUS_CTRL, if selected { theme::INPUT_FILL_BLUE } else { theme::FIELD_ROW_BG });
                        }
                        let label = if *n == current { format!("{n}   (open)") } else { n.clone() };
                        ui.painter().text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::new(13.5, egui::FontFamily::Proportional), theme::TEXT);
                        if resp.clicked() {
                            p.selected = Some(n.clone());
                        }
                        if resp.double_clicked() {
                            action = Some((n.clone(), 0));
                        }
                        resp.context_menu(|ui| {
                            widgets::flat_menu_style(ui);
                            for (label, code) in [("Open in a new workspace", 0u8), ("Open in the current workspace", 1), ("Rename...", 2), ("Drop...", 3)] {
                                let enabled = match code {
                                    2 => caps.rename_db && *n != current,
                                    3 => caps.drop_db && *n != current,
                                    _ => true,
                                };
                                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                    action = Some((n.clone(), code));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let sel = p.selected.clone();
                    ui.add_enabled_ui(sel.is_some(), |ui| {
                        if widgets::primary_button(ui, "Open").clicked() {
                            action = sel.clone().map(|n| (n, 0));
                        }
                    });
                    ui.add_enabled_ui(caps.create_db, |ui| {
                        if widgets::secondary_button(ui, "New...").clicked() {
                            new_clicked = true;
                        }
                    });
                    if widgets::secondary_button(ui, "Close").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.db_picker = None;
            }
            if new_clicked {
                self.db_dialog = Some(DbDialog::New { name: String::new(), encoding: String::new(), collation: String::new() });
            }
            if let Some((name, code)) = action {
                match code {
                    0 => {
                        self.db_picker = None;
                        self.outputs.push(WsOutput::OpenDatabase(name));
                    }
                    1 => {
                        self.db_picker = None;
                        self.session.send(Command::UseDatabase(name));
                    }
                    2 => self.db_dialog = Some(DbDialog::Rename { to: name.clone(), from: name }),
                    _ => self.db_dialog = Some(DbDialog::Drop { name, typed: String::new() }),
                }
            }
        }
        if let Some(dlg) = self.db_dialog.as_mut() {
            let mut decision = None;
            let title = match dlg {
                DbDialog::New { .. } => "New database",
                DbDialog::Rename { .. } => "Rename database",
                DbDialog::Drop { .. } => "Drop database",
            };
            widgets::modal(ctx, title, |ui| {
                match dlg {
                    DbDialog::New { name, encoding, collation } => {
                        widgets::field_label(ui, "Name");
                        widgets::input(ui, name, "database name");
                        if caps.encoding_collation_on_create_db {
                            widgets::field_label(ui, "Encoding");
                            widgets::input(ui, encoding, "utf8mb4 (optional)");
                            widgets::field_label(ui, "Collation");
                            widgets::input(ui, collation, "optional");
                        } else if driver == DriverId::Postgres {
                            widgets::field_label(ui, "Encoding");
                            widgets::input(ui, encoding, "UTF8 (optional)");
                        }
                    }
                    DbDialog::Rename { from, to } => {
                        widgets::muted(ui, &format!("Rename {from} to:"));
                        widgets::input(ui, to, "new name");
                    }
                    DbDialog::Drop { name, typed } => {
                        ui.label(format!("Dropping {name} deletes all of its data. Type its name to confirm."));
                        widgets::input(ui, typed, name);
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let (label, destructive, enabled) = match dlg {
                        DbDialog::New { name, .. } => ("Create", false, !name.trim().is_empty()),
                        DbDialog::Rename { to, from } => ("Rename", false, !to.trim().is_empty() && to != from),
                        DbDialog::Drop { name, typed } => ("Drop", true, typed == name),
                    };
                    ui.add_enabled_ui(enabled, |ui| {
                        let clicked = if destructive { widgets::danger_button(ui, label).clicked() } else { widgets::primary_button(ui, label).clicked() };
                        if clicked {
                            decision = Some(true);
                        }
                    });
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(dlg) = self.db_dialog.take() {
                        let op = match dlg {
                            DbDialog::New { name, encoding, collation } => dbzilla_session::DbOp::Create(dbzilla_drivers::NewDatabase { name: name.trim().to_owned(), encoding: Some(encoding.trim().to_owned()).filter(|e| !e.is_empty()), collation: Some(collation.trim().to_owned()).filter(|c| !c.is_empty()) }),
                            DbDialog::Rename { from, to } => dbzilla_session::DbOp::Rename { from, to: to.trim().to_owned() },
                            DbDialog::Drop { name, .. } => dbzilla_session::DbOp::Drop(name),
                        };
                        self.submit_dbop(op);
                    }
                }
                Some(false) => self.db_dialog = None,
                None => {}
            }
        }
        if let Some(l) = self.level_dialog.as_mut() {
            let mut decision = None;
            widgets::modal(ctx, "Enter the password to change the safe mode", |ui| {
                widgets::secret_input(ui, &mut l.input, &mut l.reveal);
                if let Some(e) = &l.error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Change").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    let ok = self.password.as_deref().is_some_and(|p| verify_password(p, &l.input));
                    if ok {
                        let level = l.level;
                        self.level_dialog = None;
                        self.conn.safe_level = level;
                        self.outputs.push(WsOutput::SetSafeLevel(level));
                    } else {
                        l.error = Some("The password is not correct".into());
                    }
                }
                Some(false) => self.level_dialog = None,
                None => {}
            }
        }
        if let Some(req) = self.timeout_req {
            let mut decision = None;
            widgets::modal(ctx, "The server is taking too long to respond", |ui| {
                ui.label("The running statement has not finished yet. You can keep waiting or cancel it and reconnect.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Wait").clicked() {
                        decision = Some(false);
                    }
                    if widgets::danger_button(ui, "Reconnect").clicked() {
                        decision = Some(true);
                    }
                });
            });
            match decision {
                Some(true) => {
                    self.timeout_req = None;
                    self.session.send(Command::Cancel(req));
                    self.status = WsStatus::Connecting;
                    self.session.send(Command::Reconnect);
                }
                Some(false) => self.timeout_req = None,
                None => {}
            }
        }
    }

    /// Moves the search on: next table, or finish.
    fn search_step(&mut self) {
        let Some(sr) = self.search.as_mut() else { return };
        let Some(table) = sr.next_table() else { return };
        if self.structures.contains_key(&table) {
            self.search_count_current();
        } else {
            sr.awaiting_structure = Some(table.clone());
            self.session.send(Command::LoadStructure(table));
        }
    }

    /// Counts the matches of the table being searched.
    fn search_count_current(&mut self) {
        let Some(sr) = self.search.as_mut() else { return };
        let Some(table) = sr.current.clone() else { return };
        let columns = self.structures.get(&table).map(|s| s.columns.clone()).unwrap_or_default();
        let req = RequestId::next();
        sr.req = Some(req);
        let filter = sr.filter();
        self.req_map.insert(req, (0, ReqKind::Search));
        self.session.send(Command::CountExact { req, table, columns, filters: vec![filter], filters_or: false });
    }

    /// Reacts to the Search in Database dialog.
    fn search_action(&mut self, a: SearchAction) {
        match a {
            SearchAction::Start => {
                if let Some(sr) = self.search.as_mut() {
                    sr.start();
                }
                self.search_step();
            }
            SearchAction::Pause => {
                if let Some(sr) = self.search.as_mut() {
                    sr.paused = true;
                }
            }
            SearchAction::Resume => {
                if let Some(sr) = self.search.as_mut() {
                    sr.paused = false;
                }
                self.search_step();
            }
            SearchAction::Done => self.search = None,
            SearchAction::Open(table) => {
                if let Some(sr) = &self.search {
                    let f = sr.filter();
                    self.open_table_with(table, OpenMode::NewTab, vec![f]);
                }
            }
        }
    }

    /// Opens an editor for a table that does not exist yet.
    pub fn new_table(&mut self) {
        self.new_table_no += 1;
        let table = TableRef { database: None, schema: self.current_schema.clone().filter(|_| self.caps.schemas), name: format!("untitled_table_{}", self.new_table_no) };
        let int_type = self.dialect.types().first().copied().unwrap_or("integer");
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut data = DataState::new(self.page_size);
        data.key_cols = vec!["id".into()];
        let draft = StructDraft::new_table(table.clone(), int_type);
        let tab = TableTab { table, structure: None, error: None, data, is_view: false, is_matview: false, view: TabView::Structure, draft: Some(draft), ddl_text: String::new(), ddl_commit: false, rename_to: None };
        self.dock.push_to_focused_leaf(Tab { id, pinned: true, kind: TabKind::Table(Box::new(tab)) });
        if let Some(loc) = self.tab_location(id) {
            self.dock.set_active_tab(loc);
            self.dock.set_focused_node_and_surface((loc.0, loc.1));
        }
    }

    /// Generates statements for operations and sends them through the safe-mode gate.
    fn submit_ddl(&mut self, ops: Vec<StructuralOp>, after: DdlAfter) {
        let dialect = dialect_for(self.conn.driver);
        let mut statements = Vec::new();
        for op in &ops {
            match generate_ddl(op, dialect.as_ref()) {
                Ok(v) => statements.extend(v),
                Err(e) => return self.notices.push((true, e.to_string())),
            }
        }
        match gate_check(self.conn.safe_level, self.conn.driver, &statements) {
            GateDecision::Allow => self.send_ddl(statements, after),
            GateDecision::Confirm(t) => self.gate = Some(GateModal { action: GateAction::Ddl(after), statements: t, needs_password: false, input: String::new(), reveal: false, tab: 0, error: None }),
            GateDecision::Password(t) => {
                let known = self.password.as_deref().is_some_and(|p| !p.is_empty());
                self.gate = Some(GateModal { action: GateAction::Ddl(after), statements: t, needs_password: known, input: String::new(), reveal: false, tab: 0, error: None });
            }
        }
    }

    /// Sends table level statements.
    fn send_ddl(&mut self, statements: Vec<String>, after: DdlAfter) {
        let req = RequestId::next();
        self.commit_log.insert(req, (statements.clone(), None));
        self.ddl_after.insert(req, after);
        self.req_map.insert(req, (0, ReqKind::Ddl));
        self.session.send(Command::Commit { req, statements });
    }

    /// Copies a creation / drop / truncate / select script of a table.
    fn copy_script_for(&mut self, table: TableRef, kind: ScriptKind) {
        match self.structures.get(&table) {
            Some(st) => {
                self.clipboard_out = Some(copy_script(kind, st, self.dialect.as_ref()));
                self.notices.push((false, "Script copied".into()));
            }
            None => {
                self.pending_copy = Some((table.clone(), kind));
                self.session.send(Command::LoadStructure(table));
            }
        }
    }

    /// Opens the definition of a function or procedure in a SQL tab.
    fn open_definition(&mut self, table: &TableRef, kind: ObjectKind) {
        let d = dialect_for(self.conn.driver);
        let sql = match self.conn.driver {
            DriverId::Postgres => format!(
                "SELECT pg_get_functiondef(p.oid) AS definition FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {} AND p.proname = {}",
                d.literal(&dbzilla_core::value::Value::Text(table.schema.clone().unwrap_or_else(|| "public".into()))),
                d.literal(&dbzilla_core::value::Value::Text(table.name.clone()))
            ),
            DriverId::MySql => format!("SHOW CREATE {} {}.{}", if kind == ObjectKind::Procedure { "PROCEDURE" } else { "FUNCTION" }, d.quote_ident(table.database.as_deref().unwrap_or(&self.current_db)), d.quote_ident(&table.name)),
            DriverId::Sqlite => return self.notices.push((true, "SQLite has no stored functions".into())),
        };
        self.open_sql(sql);
        self.run_sql(true);
    }

    /// Requests of the structure views.
    fn struct_action(&mut self, id: TabId, a: StructAction) {
        match a {
            StructAction::EditFk(col) => {
                let first = self.objects.iter().find(|o| o.kind == ObjectKind::Table).map(|o| o.table.name.clone()).unwrap_or_default();
                self.fk_dialog = Some(FkDialog { tab: id, col, ref_table: first, ref_columns: "id".into(), on_update: FK_ACTIONS[0].into(), on_delete: FK_ACTIONS[0].into() });
            }
            StructAction::ShowTriggers => self.triggers_for = Some(id),
        }
    }

    /// CSV of the rows as they were before the pending updates and deletes.
    fn original_values_csv(&mut self, id: TabId) -> Option<String> {
        let table = self.tab_table(id);
        let st = self.structures.get(&table).cloned();
        let dialect = dialect_for(self.conn.driver);
        let t = self.table_tab_mut(id)?;
        let cols = datatab::merge_columns(&t.data.columns, st.as_ref().or(t.structure.as_ref()));
        let rows: Vec<dbzilla_core::model::Row> = t.data.pending.meta.iter().filter(|m| matches!(m.state, super::pending::RowState::Modified | super::pending::RowState::Deleted)).filter_map(|m| m.original.clone()).collect();
        if rows.is_empty() {
            return None;
        }
        Some(format_rows(CopyFormat::CsvWithHeader, &cols, &rows, &CopyContext { dialect: dialect.as_ref(), table: None, csv: &CsvOptions::default() }))
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
        self.log_original = env.settings.log_original_values;
        self.refresh_lists();
        self.autosave_queries(ctx, env.settings.auto_save_queries);
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
                let view = sidebar::SidebarView { objects: &self.objects, schemas: &self.schemas, current_schema: self.current_schema.as_deref(), caps: &self.caps, active: active.as_ref(), queries: &self.queries_cache, history: &self.history_cache, query_sort: self.queries_sort };
                sidebar_action = sidebar::show(ui, &mut self.sidebar, &view);
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
        let mut struct_actions: Vec<(TabId, StructAction)> = Vec::new();
        let mut diagram_actions: Vec<(TabId, DiagramAction)> = Vec::new();
        let mut metrics_actions: Vec<(TabId, MetricsAction)> = Vec::new();
        self.fmt_width = env.settings.indent_width;
        self.fmt_with = env.settings.indent_with;
        self.fmt_kind = env.settings.indent_type;
        self.fmt_upper = env.settings.uppercase_keywords;
        self.params_enabled = env.settings.query_params.enabled;
        self.param_opts = dbzilla_core::params::ParamOptions { format: env.settings.query_params.format, allow_in_quotes: env.settings.query_params.allow_in_quotes };
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
            let mut viewer = Viewer { requests: &mut requests, data_actions: &mut data_actions, dialect: self.dialect.as_ref(), csv: &csv, alternating, row_height, structures: &self.structures, sql_actions: &mut sql_actions, diagram_actions: &mut diagram_actions, metrics_actions: &mut metrics_actions, struct_actions: &mut struct_actions, caps: &self.caps, keywords: &self.keywords, driver: self.conn.driver, font_size: env.settings.editor_font_size, objects: &self.objects, saved: &self.saved_keywords, uppercase_keywords: env.settings.uppercase_keywords, complete_key: env.settings.complete_key, vim: env.settings.key_binding == dbzilla_store::settings::KeyBinding::Vim, indent_width: env.settings.indent_width, indent_spaces: env.settings.indent_with == dbzilla_store::settings::IndentWith::Spaces };
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
        for (id, a) in diagram_actions {
            self.diagram_action(id, a);
        }
        for (id, a) in metrics_actions {
            self.metrics_action(id, a);
        }
        for (id, a) in struct_actions {
            self.struct_action(id, a);
        }
        for (id, a) in data_actions {
            match a {
                DataAction::Refetch => self.fetch_tab(id),
                DataAction::CountExact => self.count_exact(id),
                DataAction::FollowFk { table, column, value } => self.open_table_with(table, OpenMode::NewTab, vec![datatab::fk_filter(&column, &value)]),
                DataAction::Toast(m) => env.toasts.ok(m),
                DataAction::Error(m) => env.toasts.err(m),
                DataAction::ChooseKey => self.open_key_dialog(id),
                DataAction::ApplyFilters { filters, or } => self.apply_filters(id, filters, or),
            }
        }
        self.modals(ctx);
        if let Some(sr) = self.search.as_mut() {
            if let Some(a) = dbsearch::show(ctx, sr) {
                self.search_action(a);
            }
        }
        if let Some(text) = self.clipboard_out.take() {
            ctx.copy_text(text);
        }
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
            Some(SidebarAction::Extra(a)) => self.handle_extra(ctx, a),
            Some(SidebarAction::OpenStructure(t)) => self.open_table_view(t, TabView::Structure),
            Some(SidebarAction::CopyScript(t, k)) => self.copy_script_for(t, k),
            Some(SidebarAction::CloneTable(t)) => {
                let name = format!("{}_clone", t.name);
                self.op_dialog = Some(OpDialog::Clone { source: t, name, with_data: true });
            }
            Some(SidebarAction::TruncateTable(t)) => self.op_dialog = Some(OpDialog::Truncate { table: t, cascade: false, restart: false }),
            Some(SidebarAction::DropTable(t)) => self.op_dialog = Some(OpDialog::Drop { table: t, cascade: false }),
            Some(SidebarAction::ExportTable(t)) => self.open_export(Some(t)),
            Some(SidebarAction::ImportInto(t)) => self.open_import(Some(t)),
            Some(SidebarAction::NewTable) => self.new_table(),
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
        self.table_dialogs(ctx);
        self.database_dialogs(ctx);
        self.job_dialogs(ctx);
        self.process_sheet(ctx);
        if let Some(d) = self.save_dialog.as_mut() {
            let mut decision = None;
            widgets::modal(ctx, "Add to Queries", |ui| {
                widgets::field_label(ui, "Name (use / for folders)");
                widgets::input(ui, &mut d.name, "monthly report");
                ui.add_space(4.0);
                widgets::field_label(ui, "Keywords (comma separated)");
                widgets::input(ui, &mut d.keywords, "rev, month");
                ui.add_space(4.0);
                widgets::field_label(ui, "Query");
                widgets::text_area(ui, &mut d.sql, 6);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Save").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    let name = d.name.trim().to_owned();
                    let keywords: Vec<String> = d.keywords.split(',').map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()).collect();
                    if name.is_empty() {
                        self.notices.push((true, "Give the query a name".into()));
                    } else {
                        match self.queries.save(&name, &keywords, &d.sql) {
                            Ok(()) => {
                                self.notices.push((false, "Query saved".into()));
                                self.queries_dirty = true;
                                self.save_dialog = None;
                            }
                            Err(e) => self.notices.push((true, format!("Cannot save the query: {e}"))),
                        }
                    }
                }
                Some(false) => self.save_dialog = None,
                None => {}
            }
        }
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
        if let Some(m) = self.params_modal.as_mut() {
            let mut decision = None;
            widgets::modal(ctx, "Variables", |ui| {
                widgets::muted(ui, "Values are inserted into the SQL as typed. Add the quotes yourself for text.");
                let max_h = (ctx.screen_rect().height() * 0.5).max(120.0);
                egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, n) in m.names.iter().enumerate() {
                        widgets::field_label(ui, n);
                        widgets::input(ui, &mut m.values[i], "");
                    }
                });
                if let Some(e) = &m.error {
                    ui.colored_label(theme::ERROR, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Apply").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
                        decision = Some(ParamsDecision::Apply);
                    }
                    if widgets::secondary_button(ui, "Cleanup").on_hover_text("Forget the remembered values of variables not used in this SQL").clicked() {
                        decision = Some(ParamsDecision::Cleanup);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(ParamsDecision::Cancel);
                    }
                });
            });
            match decision {
                Some(ParamsDecision::Apply) => self.apply_params(),
                Some(ParamsDecision::Cleanup) => self.cleanup_params(),
                Some(ParamsDecision::Cancel) => self.params_modal = None,
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
                                GateAction::Ddl(after) => self.send_ddl(g.statements, after),
                                GateAction::DbOp(op) => self.session.send(Command::DbOp(op)),
                                GateAction::Import(title, req) => self.spawn_import_now(title, req),
                                GateAction::Run { labels, originals } => self.send_run(g.tab, g.statements, labels, originals),
                                GateAction::Kill => self.kill_now(g.statements),
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
                // Long column lists scroll inside the dialog so the buttons stay on screen.
                let max_h = (ctx.screen_rect().height() * 0.6).max(120.0);
                egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, n) in k.names.iter().enumerate() {
                        ui.checkbox(&mut k.selected[i], n);
                    }
                });
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

    /// Clone / truncate / drop, foreign key and trigger dialogs.
    fn table_dialogs(&mut self, ctx: &egui::Context) {
        let caps = self.caps.clone();
        if let Some(op) = self.op_dialog.as_mut() {
            let mut decision = None;
            let title = match op {
                OpDialog::Clone { .. } => "Clone table",
                OpDialog::Truncate { .. } => "Truncate table",
                OpDialog::Drop { .. } => "Delete table",
            };
            widgets::modal(ctx, title, |ui| {
                match op {
                    OpDialog::Clone { source, name, with_data } => {
                        widgets::muted(ui, &format!("Copy of {}", source.name));
                        widgets::field_label(ui, "New table name");
                        widgets::input(ui, name, "name");
                        ui.checkbox(with_data, "Duplicate table content");
                    }
                    OpDialog::Truncate { table, cascade, restart } => {
                        ui.label(format!("Delete every row of {}?", table.name));
                        if caps.truncate_cascade {
                            ui.checkbox(cascade, "Cascade");
                        }
                        if caps.truncate_restart_identity {
                            ui.checkbox(restart, "Restart identity");
                        }
                    }
                    OpDialog::Drop { table, cascade } => {
                        ui.label(format!("Delete {}? This cannot be undone.", table.name));
                        if caps.drop_cascade {
                            ui.checkbox(cascade, "Cascade");
                        }
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let destructive = !matches!(op, OpDialog::Clone { .. });
                    let label = match op {
                        OpDialog::Clone { .. } => "Clone",
                        OpDialog::Truncate { .. } => "Truncate",
                        OpDialog::Drop { .. } => "Delete",
                    };
                    let clicked = if destructive { widgets::danger_button(ui, label).clicked() } else { widgets::primary_button(ui, label).clicked() };
                    if clicked {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(op) = self.op_dialog.take() {
                        match op {
                            OpDialog::Clone { source, name, with_data } => {
                                if name.trim().is_empty() {
                                    self.notices.push((true, "Give the new table a name".into()));
                                    self.op_dialog = Some(OpDialog::Clone { source, name, with_data });
                                } else {
                                    let mut target = source.clone();
                                    target.name = name.trim().to_owned();
                                    let msg = format!("Cloned {} to {}", source.name, target.name);
                                    self.submit_ddl(vec![StructuralOp::CloneTable { source, target, with_data }], DdlAfter { closed: None, message: msg });
                                }
                            }
                            OpDialog::Truncate { table, cascade, restart } => {
                                let msg = format!("Truncated {}", table.name);
                                self.submit_ddl(vec![StructuralOp::Truncate { table: table.clone(), cascade, restart_identity: restart }], DdlAfter { closed: None, message: msg });
                                let tabs: Vec<TabId> = self.dock.iter_all_tabs().filter(|(_, t)| matches!(&t.kind, TabKind::Table(tt) if tt.table == table)).map(|(_, t)| t.id).collect();
                                for id in tabs {
                                    if let Some(t) = self.table_tab_mut(id) {
                                        t.data.total = Total::Unknown;
                                    }
                                }
                            }
                            OpDialog::Drop { table, cascade } => {
                                let msg = format!("Deleted {}", table.name);
                                self.submit_ddl(vec![StructuralOp::DropTable { table: table.clone(), cascade }], DdlAfter { closed: Some(table), message: msg });
                            }
                        }
                    }
                }
                Some(false) => self.op_dialog = None,
                None => {}
            }
        }
        if let Some(f) = self.fk_dialog.as_mut() {
            let mut decision = None;
            let tables: Vec<String> = self.objects.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.table.name.clone()).collect();
            widgets::modal(ctx, "Create a foreign key", |ui| {
                widgets::field_label(ui, "Referenced table");
                egui::ComboBox::from_id_salt("fk_table").selected_text(f.ref_table.clone()).width(240.0).show_ui(ui, |ui| {
                    for t in &tables {
                        ui.selectable_value(&mut f.ref_table, t.clone(), t);
                    }
                });
                widgets::field_label(ui, "Referenced column");
                widgets::input(ui, &mut f.ref_columns, "id");
                widgets::field_label(ui, "On update");
                egui::ComboBox::from_id_salt("fk_upd").selected_text(f.on_update.clone()).show_ui(ui, |ui| {
                    for a in FK_ACTIONS {
                        ui.selectable_value(&mut f.on_update, a.to_owned(), a);
                    }
                });
                widgets::field_label(ui, "On delete");
                egui::ComboBox::from_id_salt("fk_del").selected_text(f.on_delete.clone()).show_ui(ui, |ui| {
                    for a in FK_ACTIONS {
                        ui.selectable_value(&mut f.on_delete, a.to_owned(), a);
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Create").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(f) = self.fk_dialog.take() {
                        let target = self.objects.iter().find(|o| o.table.name == f.ref_table).map(|o| o.table.clone()).unwrap_or_else(|| TableRef::simple(&f.ref_table));
                        let cols: Vec<String> = f.ref_columns.split(',').map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect();
                        if cols.is_empty() || f.ref_table.is_empty() {
                            self.notices.push((true, "Choose the referenced table and column".into()));
                        } else if let Some(t) = self.table_tab_mut(f.tab) {
                            if let Some(c) = t.draft.as_mut().and_then(|d| d.cols.get_mut(f.col)) {
                                c.fk = Some(FkDraft { name: None, ref_table: target, ref_columns: cols, on_update: f.on_update, on_delete: f.on_delete });
                            }
                        }
                    }
                }
                Some(false) => self.fk_dialog = None,
                None => {}
            }
        }
        if let Some(d) = self.link_dialog.as_mut() {
            let mut decision = None;
            let sqlite = self.conn.driver == DriverId::Sqlite;
            widgets::modal(ctx, "Link columns", |ui| {
                ui.label(RichText::new(format!("{}.{}  ->  {}.{}", d.link.from_table, d.link.from_column, d.link.to_table, d.link.to_column)).color(theme::TEXT));
                ui.add_space(8.0);
                ui.radio_value(&mut d.kind, LinkKind::DiagramOnly, "Only in the diagram");
                widgets::muted(ui, "A dashed line kept in DBZilla. The database is not changed.");
                ui.add_space(4.0);
                ui.radio_value(&mut d.kind, LinkKind::Database, "Foreign key in the database");
                if d.kind == LinkKind::Database {
                    ui.add_space(4.0);
                    widgets::field_label(ui, "Constraint name (optional)");
                    widgets::input(ui, &mut d.constraint, "chosen by the database");
                    widgets::field_label(ui, "On update");
                    egui::ComboBox::from_id_salt("link_upd").selected_text(d.on_update.clone()).show_ui(ui, |ui| {
                        for a in FK_ACTIONS {
                            ui.selectable_value(&mut d.on_update, a.to_owned(), a);
                        }
                    });
                    widgets::field_label(ui, "On delete");
                    egui::ComboBox::from_id_salt("link_del").selected_text(d.on_delete.clone()).show_ui(ui, |ui| {
                        for a in FK_ACTIONS {
                            ui.selectable_value(&mut d.on_delete, a.to_owned(), a);
                        }
                    });
                    if sqlite {
                        widgets::muted(ui, "SQLite rebuilds the table to add it. Rows that break the key stop the change.");
                    }
                }
                if let Some(e) = &d.error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(e).color(theme::ERROR));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Create").clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(d) = self.link_dialog.take() {
                        self.apply_link_dialog(d);
                    }
                }
                Some(false) => self.link_dialog = None,
                None => {}
            }
        }
        if let Some(id) = self.triggers_for {
            let triggers = self.table_tab_mut(id).and_then(|t| t.structure.as_ref().map(|s| s.triggers.clone())).unwrap_or_default();
            let mut close = false;
            widgets::modal(ctx, "Triggers", |ui| {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for t in &triggers {
                        ui.label(widgets::semibold(format!("{}  {} {}", t.name, t.timing, t.event), 13.0).color(theme::SECONDARY));
                        ui.add(egui::Label::new(RichText::new(&t.statement).monospace().color(theme::TEXT)).selectable(true));
                        ui.add_space(6.0);
                    }
                });
                if widgets::secondary_button(ui, "Close").clicked() {
                    close = true;
                }
            });
            if close {
                self.triggers_for = None;
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
                let shield = widgets::ico_button(ui, Ico::Shield, &format!("Safe mode: {}", super::connform::safe_label(self.conn.safe_level)));
                let level_popup = ui.make_persistent_id("safe_level_menu");
                if shield.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(level_popup));
                }
                egui::popup_below_widget(ui, level_popup, &shield, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(180.0);
                    widgets::flat_menu_style(ui);
                    for l in [SafeLevel::Silent, SafeLevel::Alert1, SafeLevel::Alert2, SafeLevel::Safe1, SafeLevel::Safe2] {
                        if ui.selectable_label(self.conn.safe_level == l, super::connform::safe_label(l)).clicked() {
                            self.request_safe_level(l);
                        }
                    }
                });
                if widgets::ico_button(ui, Ico::Export, "Export data").clicked() {
                    self.open_export(None);
                }
                if widgets::ico_button(ui, Ico::Import, "Import data").clicked() {
                    self.open_import(None);
                }
                jobs::bell(ui, &mut self.jobs);
                if widgets::ico_button(ui, Ico::Database, &tip("Open a database", Action::OpenDatabase)).clicked() {
                    self.db_picker = Some(DbPicker { query: String::new(), selected: None });
                }
                if widgets::ico_button(ui, Ico::File, &tip("New SQL editor", Action::NewSqlEditor)).clicked() {
                    self.open_sql(String::new());
                }
                if widgets::ico_button(ui, Ico::Structure, &tip("Show ER diagram", Action::ShowDiagram)).clicked() {
                    self.open_diagram();
                }
                if widgets::ico_button(ui, Ico::Bolt, &tip("Show metrics board", Action::ShowMetrics)).clicked() {
                    self.open_metrics();
                }
                if widgets::ico_button(ui, Ico::Console, &tip("Process list", Action::ProcessList)).clicked() {
                    self.open_process_list();
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
                let info = ui.add(egui::Label::new(RichText::new(state).size(12.0).color(theme::MUTED)).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
                let popup = ui.make_persistent_id("status_popover");
                if info.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(popup));
                }
                egui::popup_above_or_below_widget(ui, popup, &info, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(200.0);
                    widgets::flat_menu_style(ui);
                    ui.label(RichText::new(format!("{} on {}", self.conn.user, if self.conn.host.is_empty() { "this computer" } else { &self.conn.host })).size(12.0).color(theme::MUTED));
                    ui.separator();
                    if ui.button("Edit connection").clicked() {
                        self.outputs.push(WsOutput::EditConnection);
                    }
                    if ui.button("Reconnect").clicked() {
                        self.reconnect();
                    }
                    if ui.button("Disconnect").clicked() {
                        self.session.send(Command::Disconnect);
                    }
                });
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
    diagram_actions: &'a mut Vec<(TabId, DiagramAction)>,
    metrics_actions: &'a mut Vec<(TabId, MetricsAction)>,
    struct_actions: &'a mut Vec<(TabId, StructAction)>,
    caps: &'a Capabilities,
    keywords: &'a HashSet<String>,
    driver: DriverId,
    font_size: f32,
    objects: &'a [ObjectInfo],
    saved: &'a HashMap<String, String>,
    uppercase_keywords: bool,
    complete_key: dbzilla_store::settings::CompleteKey,
    vim: bool,
    indent_width: u8,
    indent_spaces: bool,
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
            TabKind::Diagram(d) => {
                for a in diagram::show(ui, d) {
                    self.diagram_actions.push((id, a));
                }
                return;
            }
            TabKind::Metrics(m) => {
                for a in metrics::show(ui, m) {
                    self.metrics_actions.push((id, a));
                }
                return;
            }
            TabKind::Sql(sq) => {
                let tables: Vec<(&str, bool)> = self
                    .objects
                    .iter()
                    .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView))
                    .map(|o| (o.table.name.as_str(), !matches!(o.kind, ObjectKind::Table)))
                    .collect();
                let structures = self.structures;
                let columns = |t: &str| -> Option<Vec<(String, String)>> {
                    structures.iter().find(|(k, _)| k.name.eq_ignore_ascii_case(t)).map(|(_, st)| st.columns.iter().map(|c| (c.name.clone(), c.type_name.clone())).collect())
                };
                let dialect = self.dialect;
                let quote = |n: &str| quote_if_needed(dialect, n);
                let env = SqlEnv {
                    keywords: self.keywords,
                    driver: self.driver,
                    font_size: self.font_size,
                    alternating: self.alternating,
                    row_height: self.row_height,
                    tables: &tables,
                    columns: &columns,
                    dialect_keywords: dialect.keywords(),
                    saved: self.saved,
                    uppercase_keywords: self.uppercase_keywords,
                    complete_key: self.complete_key,
                    quote: &quote,
                    completion: true,
                    vim: self.vim,
                    tab_width: usize::from(self.indent_width),
                    insert_spaces: self.indent_spaces,
                    schema_rev: self.structures.len() * 1000 + self.objects.len() + self.saved.len(),
                };
                for a in sqltab::show(ui, sq, &env) {
                    self.sql_actions.push((id, a));
                }
                return;
            }
        };
        let table = t.table.clone();
        let creating = t.draft.as_ref().is_some_and(|d| d.is_new);
        egui::TopBottomPanel::bottom(ui.id().with("view_switch")).exact_height(32.0).frame(egui::Frame::new().fill(theme::NAVY_DARK).inner_margin(Margin::symmetric(10, 4))).show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_enabled_ui(!creating, |ui| {
                    ui.selectable_value(&mut t.view, TabView::Data, "Data");
                });
                ui.selectable_value(&mut t.view, TabView::Structure, "Structure");
                ui.selectable_value(&mut t.view, TabView::Index, "Index");
                ui.add_enabled_ui(!creating, |ui| {
                    ui.selectable_value(&mut t.view, TabView::Ddl, "DDL");
                });
            });
        });
        if t.draft.is_none() && t.view != TabView::Data {
            t.draft = t.structure.as_ref().map(StructDraft::from_structure);
        }
        match t.view {
            TabView::Data => {
                let env = DataEnv { dialect: self.dialect, csv: self.csv, alternating: self.alternating, row_height: self.row_height, table: &table, structure: self.structures.get(&table), is_view: t.is_view, caps: self.caps };
                for a in datatab::show(ui, &mut t.data, &env) {
                    self.data_actions.push((id, a));
                }
            }
            TabView::Structure | TabView::Index => {
                let editable = !t.is_view && !t.is_matview;
                let senv = StructEnv { caps: self.caps, types: self.dialect.types(), editable };
                match &mut t.draft {
                    Some(d) if t.view == TabView::Structure => {
                        for a in structure::show_columns(ui, d, &senv) {
                            self.struct_actions.push((id, a));
                        }
                    }
                    Some(d) => structure::show_indexes(ui, d, &senv),
                    None => {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| ui.spinner());
                    }
                }
            }
            TabView::Ddl => {
                let editable = t.is_view && !t.is_matview;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_enabled(editable, egui::TextEdit::multiline(&mut t.ddl_text).code_editor().desired_width(f32::INFINITY).desired_rows(20));
                });
            }
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
        let stores = Stores { history: HistoryStore::new(dir.path().join("history"), true), diagrams: DiagramStore::new(dir.path().join("diagrams.json")), metrics: MetricsStore::new(dir.path().join("metrics.json")), queries: QueryStore::new(dir.path().join("queries")) };
        let mut ws = Workspace::new(conn, session, 1000, 300, None, stores);
        wait(&mut ws, |w| matches!(w.status, WsStatus::Connected(_)) && !w.objects.is_empty());
        (dir, path, ws)
    }

    /// Polls the workspace until `done` holds (10 s limit).
    fn wait(ws: &mut Workspace, mut done: impl FnMut(&mut Workspace) -> bool) {
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

    /// With query parameters on, a run asks for the variables and runs the substituted SQL.
    #[test]
    fn query_parameters_are_asked_and_substituted() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.params_enabled = true;
        open_sql_focused(&mut ws, "SELECT :a + :b AS s, ':a' AS q");
        ws.run_sql(true);
        assert_eq!(ws.params_modal.as_ref().expect("the Variables dialog").names, vec!["a", "b"]);
        assert!(!sql_running(&mut ws), "nothing runs before Apply");
        ws.params_modal.as_mut().unwrap().values = vec!["40".into(), "2".into()];
        ws.apply_params();
        wait(&mut ws, |w| !sql_running(w));
        let (_, t) = ws.active_sql_mut().unwrap();
        assert_eq!(t.results[0].rows, vec![vec![Value::I64(42), Value::Text(":a".into())]]);
        // the values are remembered for the next run
        ws.run_sql(true);
        assert_eq!(ws.params_modal.as_ref().unwrap().values, vec!["40", "2"]);
    }

    /// Without placeholders, or with the feature off, the dialog never shows up.
    #[test]
    fn query_parameters_do_not_interfere_when_unused() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "SELECT :a");
        ws.params_enabled = false;
        ws.run_sql(true);
        assert!(ws.params_modal.is_none());
        wait(&mut ws, |w| !sql_running(w));
        ws.params_enabled = true;
        ws.active_sql_mut().unwrap().1.text = "SELECT 1".into();
        ws.run_sql(true);
        assert!(ws.params_modal.is_none());
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

    /// Editor statements are logged without the injected LIMIT, failures included.
    #[test]
    fn history_records_editor_statements() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "SELECT * FROM t;\nSELEC oops");
        ws.active_sql_mut().unwrap().1.row_limit = Some(10);
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        let entries = ws.history.search(&ws.conn.id, "", 10).unwrap();
        assert_eq!(entries.len(), 2);
        let ok = entries.iter().find(|e| e.sql == "SELECT * FROM t").expect("the typed statement, without LIMIT");
        assert!(ok.ok && ok.kind == HistoryKind::Editor);
        assert!(entries.iter().any(|e| e.sql == "SELEC oops" && !e.ok));
        assert!(ws.history_dirty);
    }

    /// Commits are logged; with the option on, the rows before the change are kept as CSV.
    #[test]
    fn commit_history_keeps_original_values() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.log_original = true;
        open_t(&mut ws);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("changed".into())));
        ws.commit_active();
        wait(&mut ws, |w| !w.has_pending() && w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && !t.data.committing));
        let entries = ws.history.search(&ws.conn.id, "UPDATE", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, HistoryKind::Commit);
        let csv = entries[0].original_csv.as_deref().expect("original rows");
        assert!(csv.starts_with("id,name") && csv.contains("alpha"), "{csv}");
    }

    /// Saved queries open once, edits are written back, keywords survive, history clears.
    #[test]
    fn saved_queries_autosave_and_history_clear() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.queries.save("reports/q.sql", &["kw".to_owned()], "SELECT 1").unwrap();
        ws.open_query("reports/q.sql");
        ws.open_query("reports/q.sql");
        assert_eq!(ws.dock.iter_all_tabs().count(), 1, "a saved query is opened only once");
        {
            let t = ws.active_sql_mut().unwrap().1;
            assert_eq!(t.text, "SELECT 1");
            t.text = "SELECT 2".into();
            t.last_edit = Some(Instant::now() - Duration::from_secs(5));
        }
        ws.autosave_queries(&egui::Context::default(), true);
        let q = ws.queries.read("reports/q.sql").unwrap();
        assert_eq!((q.sql.as_str(), q.keywords), ("SELECT 2", vec!["kw".to_owned()]));
        assert!(ws.queries_dirty);
        ws.refresh_lists();
        assert_eq!(ws.saved_keywords.get("kw").map(String::as_str), Some("SELECT 2"));
        open_sql_focused(&mut ws, "SELECT 9");
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        ws.refresh_lists();
        assert_eq!(ws.history_cache.len(), 1);
        ws.handle_extra(&egui::Context::default(), ExtraAction::HistoryClear);
        ws.refresh_lists();
        assert!(ws.history_cache.is_empty());
    }

    fn structure_ready(ws: &mut Workspace) -> bool {
        ws.active_tab_mut().is_some_and(|(_, t)| !t.data.committing && !t.data.loading && t.structure.is_some() && t.draft.is_some())
    }

    /// Editing the structure: add a column, commit, the table and the draft reload.
    #[test]
    fn structure_add_column_and_commit() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        open_t(&mut ws);
        wait(&mut ws, structure_ready);
        let (_, t) = ws.active_tab_mut().unwrap();
        t.view = TabView::Structure;
        let mut c = super::super::structdraft::ColDraft::blank("integer");
        c.name = "qty".into();
        c.default = "7".into();
        t.draft.as_mut().unwrap().cols.push(c);
        assert!(ws.has_pending());
        ws.commit_active();
        wait(&mut ws, |w| structure_ready(w) && !w.has_pending() && w.active_tab_mut().is_some_and(|(_, t)| t.structure.as_ref().is_some_and(|s| s.columns.len() == 3)));
        let c = rusqlite::Connection::open(&path).unwrap();
        let v: i64 = c.query_row("SELECT qty FROM t WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 7, "the default filled the new column");
    }

    /// SQLite alter column rebuilds the table and keeps the data; renaming follows.
    #[test]
    fn structure_alter_and_rename_on_sqlite() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        open_t(&mut ws);
        wait(&mut ws, structure_ready);
        {
            let (_, t) = ws.active_tab_mut().unwrap();
            t.view = TabView::Structure;
            let d = t.draft.as_mut().unwrap();
            d.cols[1].name = "label".into();
            d.cols[1].type_name = "VARCHAR(30)".into();
            d.new_name = "things".into();
        }
        ws.commit_active();
        wait(&mut ws, |w| structure_ready(w) && w.active_tab_mut().is_some_and(|(_, t)| t.table.name == "things" && t.structure.as_ref().is_some_and(|s| s.table.name == "things")));
        let c = rusqlite::Connection::open(&path).unwrap();
        let labels: Vec<String> = c.prepare("SELECT label FROM things ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "things") && !w.objects.iter().any(|o| o.table.name == "t"));
    }

    /// A new table is created from the editor and becomes a normal tab.
    #[test]
    fn create_table_from_the_editor() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        ws.new_table();
        {
            let (_, t) = ws.active_tab_mut().unwrap();
            let d = t.draft.as_mut().unwrap();
            d.new_name = "fresh".into();
            let mut c = super::super::structdraft::ColDraft::blank("TEXT");
            c.name = "label".into();
            d.cols.push(c);
        }
        ws.commit_active();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "fresh") && w.active_tab_mut().is_some_and(|(_, t)| t.table.name == "fresh" && t.structure.is_some() && !t.data.loading));
        let c = rusqlite::Connection::open(&path).unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM fresh", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(!ws.active_tab_mut().unwrap().1.draft.as_ref().is_some_and(|d| d.is_new));
    }

    /// Clone, truncate and drop go through the same pipeline as commits.
    #[test]
    fn clone_truncate_and_drop_tables() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        open_t(&mut ws);
        ws.submit_ddl(vec![StructuralOp::CloneTable { source: TableRef::simple("t"), target: TableRef::simple("t_clone"), with_data: true }], DdlAfter::default());
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "t_clone"));
        let count = |table: &str| -> i64 { rusqlite::Connection::open(&path).unwrap().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap() };
        assert_eq!(count("t_clone"), 2);
        ws.submit_ddl(vec![StructuralOp::Truncate { table: TableRef::simple("t_clone"), cascade: false, restart_identity: false }], DdlAfter::default());
        wait(&mut ws, |_| count("t_clone") == 0);
        ws.submit_ddl(vec![StructuralOp::DropTable { table: TableRef::simple("t"), cascade: false }], DdlAfter { closed: Some(TableRef::simple("t")), message: String::new() });
        wait(&mut ws, |w| !w.objects.iter().any(|o| o.table.name == "t") && w.dock.iter_all_tabs().next().is_none());
    }

    /// Copy Script As produces the creation script, loading the structure when needed.
    #[test]
    fn copy_creation_script() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.copy_script_for(TableRef::simple("t"), ScriptKind::Creation);
        wait(&mut ws, |w| w.clipboard_out.is_some());
        let script = ws.clipboard_out.take().unwrap();
        assert!(script.starts_with("CREATE TABLE t") && script.ends_with(';'), "{script}");
        ws.copy_script_for(TableRef::simple("t"), ScriptKind::SelectTop100);
        assert_eq!(ws.clipboard_out.as_deref(), Some("SELECT * FROM \"t\" LIMIT 100;"));
    }

    /// Filters narrow the page and the count; clearing restores everything.
    #[test]
    fn filters_apply_and_clear() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_t(&mut ws);
        let id = ws.active_tab_mut().unwrap().0;
        let f = |op, v: &str| Filter { enabled: true, column: dbzilla_core::filter::FilterColumn::Column("name".into()), op, value: v.into() };
        ws.apply_filters(id, vec![f(dbzilla_core::filter::FilterOp::Contains, "ALP")], false);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 1));
        assert_eq!(name_of_row(&mut ws, 0), Value::Text("alpha".into()));
        ws.apply_filters(id, vec![f(dbzilla_core::filter::FilterOp::Eq, "nope"), f(dbzilla_core::filter::FilterOp::Eq, "beta")], true);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 1));
        assert_eq!(name_of_row(&mut ws, 0), Value::Text("beta".into()), "OR keeps rows matching any filter");
        ws.apply_filters(id, vec![], false);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 2));
    }

    /// The metrics board opens once, runs the query of an object on the session and keeps the boards.
    #[test]
    fn metrics_board_runs_queries_and_saves_boards() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.open_metrics();
        ws.open_metrics();
        assert_eq!(ws.tab_metas().iter().filter(|t| t.target == TabTarget::Metrics).count(), 1);
        let id = ws.metrics_tab_mut().unwrap().add_with_sql(dbzilla_core::metrics::ObjectKind::Scoreboard, "SELECT count(*) AS n, 'rows' FROM t");
        let mut actions = Vec::new();
        ws.metrics_tab_mut().unwrap().dispatch_for_test(&mut actions);
        let tab_id = ws.tab_metas().iter().find(|t| t.target == TabTarget::Metrics).unwrap().id;
        assert_eq!(actions.len(), 1);
        for a in actions {
            ws.metrics_action(tab_id, a);
        }
        wait(&mut ws, |w| w.metrics_tab_mut().and_then(|m| m.data(id).map(|d| !d.loading)).unwrap_or(false));
        let d = ws.metrics_tab_mut().unwrap().data(id).unwrap().clone();
        assert_eq!(d.error, None);
        assert_eq!(d.rows, vec![vec![Value::I64(2), Value::Text("rows".into())]]);
        ws.metrics_action(tab_id, MetricsAction::Save(vec![dbzilla_core::metrics::Board::new("b", "Saved")]));
        assert_eq!(ws.metrics.load()[0].name, "Saved");
        ws.metrics_action(tab_id, MetricsAction::Close);
        assert!(ws.metrics_tab_mut().is_none());
    }

    /// The ER diagram loads every table of the schema with its foreign keys and opens only once.
    #[test]
    fn diagram_loads_tables_and_foreign_keys() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (id INTEGER PRIMARY KEY, p_id INTEGER REFERENCES p(id)); CREATE VIEW v AS SELECT * FROM t;").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "c"));
        ws.handle_action(Action::ShowDiagram);
        ws.handle_action(Action::ShowDiagram);
        assert_eq!(ws.tab_metas().iter().filter(|t| t.target == TabTarget::Diagram).count(), 1);
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let d = ws.diagram_tab_mut().unwrap();
        assert_eq!(d.table_count(), 3, "t, p and c; the view is left out");
        assert_eq!(d.edge_count(false), 1);
        assert_eq!(d.edge_count(true), 0);
        assert!(d.skipped.is_empty(), "{:?}", d.skipped);
        ws.diagram_action(ws.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).unwrap().id, DiagramAction::Close);
        assert!(ws.diagram_tab_mut().is_none());
    }

    /// A column named like a reference, with no foreign key, gets a guessed link.
    #[test]
    fn diagram_guesses_links_from_column_names() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY); CREATE TABLE logs (id INTEGER PRIMARY KEY, id_user INTEGER);").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "logs"));
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let d = ws.diagram_tab_mut().unwrap();
        assert_eq!(d.edge_count(false), 0);
        assert_eq!(d.edge_count(true), 1);
    }

    /// A link drawn between two columns can stay in the diagram only: it is saved, comes back and can be deleted.
    #[test]
    fn diagram_only_links_are_kept_and_deleted() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY); CREATE TABLE logs (id INTEGER PRIMARY KEY, owner INTEGER);").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "logs"));
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let id = ws.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).unwrap().id;
        let link = DocLink { from_table: "logs".into(), from_column: "owner".into(), to_table: "users".into(), to_column: "id".into() };
        ws.diagram_action(id, DiagramAction::RequestLink(link.clone()));
        assert_eq!(ws.link_dialog.as_ref().unwrap().kind, LinkKind::DiagramOnly, "the harmless choice is the default");
        let dialog = ws.link_dialog.take().unwrap();
        ws.apply_link_dialog(dialog);
        assert_eq!(ws.diagram_tab_mut().unwrap().links(), std::slice::from_ref(&link));
        assert_eq!(ws.diagrams.load_links(&ws.diagram_key()), vec![link.clone()]);
        // closed and opened again, the link comes back
        ws.diagram_action(id, DiagramAction::Close);
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        assert_eq!(ws.diagram_tab_mut().unwrap().links(), std::slice::from_ref(&link));
        let id = ws.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).unwrap().id;
        ws.diagram_action(id, DiagramAction::DeleteLink(link));
        assert!(ws.diagram_tab_mut().unwrap().links().is_empty());
        assert!(ws.diagrams.load_links(&ws.diagram_key()).is_empty());
        // the database was never touched
        assert_eq!(rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM pragma_foreign_key_list('logs')", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    /// A link can become a real foreign key: the table is rebuilt on SQLite and the diagram shows the key.
    #[test]
    fn diagram_link_becomes_a_foreign_key() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY); CREATE TABLE logs (id INTEGER PRIMARY KEY, owner INTEGER); INSERT INTO users VALUES (1); INSERT INTO logs (owner) VALUES (1);").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "logs"));
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let id = ws.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).unwrap().id;
        let link = DocLink { from_table: "logs".into(), from_column: "owner".into(), to_table: "users".into(), to_column: "id".into() };
        ws.diagram_action(id, DiagramAction::RequestLink(link));
        let mut dialog = ws.link_dialog.take().unwrap();
        dialog.kind = LinkKind::Database;
        dialog.on_delete = "CASCADE".into();
        ws.apply_link_dialog(dialog);
        assert!(ws.link_dialog.is_none(), "{:?}", ws.link_dialog.as_ref().map(|d| d.error.clone()));
        assert_eq!(ws.link_kind, LinkKind::Database, "the choice is remembered");
        if let Some(g) = ws.gate.take() {
            ws.send_ddl(g.statements, DdlAfter::default());
        }
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading && d.edge_count(false) == 1));
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM logs", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "the data is kept");
        assert_eq!(db.query_row("SELECT on_delete FROM pragma_foreign_key_list('logs')", [], |r| r.get::<_, String>(0)).unwrap(), "CASCADE");
    }

    /// Adding a foreign key to a table that others refer to is refused with a reason, and the dialog stays open.
    #[test]
    fn diagram_link_refused_by_the_database_keeps_the_dialog() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE a (id INTEGER PRIMARY KEY); CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER, x INTEGER); CREATE TABLE c (b_id INTEGER REFERENCES b(id));").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "c"));
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let id = ws.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).unwrap().id;
        ws.diagram_action(id, DiagramAction::RequestLink(DocLink { from_table: "b".into(), from_column: "a_id".into(), to_table: "a".into(), to_column: "id".into() }));
        let mut dialog = ws.link_dialog.take().unwrap();
        dialog.kind = LinkKind::Database;
        ws.apply_link_dialog(dialog);
        let again = ws.link_dialog.as_ref().expect("the dialog stays open");
        assert!(again.error.as_deref().is_some_and(|e| e.contains("refer to it")), "{:?}", again.error);
        assert!(ws.gate.is_none());
    }

    /// Moved tables come back where they were; Rearrange forgets them.
    #[test]
    fn diagram_positions_are_kept_between_openings() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE p (id INTEGER PRIMARY KEY);").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "p"));
        let diagram_id = |w: &Workspace| w.tab_metas().iter().find(|t| t.target == TabTarget::Diagram).map(|t| t.id).unwrap();
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        let auto = ws.diagram_tab_mut().unwrap().position_of("p").unwrap();
        assert!(!ws.diagrams.load(&ws.diagram_key()).is_empty(), "the first layout is saved");
        let moved: dbzilla_store::diagrams::Positions = [("p".to_owned(), (1234.0, 567.0)), ("t".to_owned(), (0.0, 0.0))].into();
        let id = diagram_id(&ws);
        ws.diagram_action(id, DiagramAction::Save(moved));
        ws.diagram_action(id, DiagramAction::Close);
        ws.open_diagram();
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        assert_eq!(ws.diagram_tab_mut().unwrap().position_of("p"), Some((1234.0, 567.0)));
        let id = diagram_id(&ws);
        ws.diagram_action(id, DiagramAction::Rearrange);
        wait(&mut ws, |w| w.diagram_tab_mut().is_some_and(|d| !d.loading));
        assert_eq!(ws.diagram_tab_mut().unwrap().position_of("p"), Some(auto));
        assert_eq!(ws.diagrams.load(&ws.diagram_key()).get("p"), Some(&auto));
    }

    /// Search in Database scans every table and lists the ones with matches.
    #[test]
    fn search_in_database_finds_tables() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, path, mut ws) = connected(&rt);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE u (id INTEGER PRIMARY KEY, note TEXT); INSERT INTO u VALUES (1,'nothing'),(2,'Alpine');").unwrap();
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "u"));
        ws.handle_action(Action::SearchInDatabase);
        {
            let sr = ws.search.as_mut().unwrap();
            sr.value = "alp".into();
        }
        ws.search_action(SearchAction::Start);
        wait(&mut ws, |w| w.search.as_ref().is_some_and(|s| !s.running));
        let sr = ws.search.as_ref().unwrap();
        let mut found: Vec<(String, u64)> = sr.results.iter().map(|(t, n)| (t.name.clone(), *n)).collect();
        found.sort();
        assert_eq!(found, vec![("t".to_owned(), 1), ("u".to_owned(), 1)]);
        assert!(sr.skipped.is_empty(), "{:?}", sr.skipped);
        assert_eq!(sr.scanned, 2);
        ws.search_action(SearchAction::Open(TableRef::simple("u")));
        let loc = ws.dock.iter_all_tabs().last().map(|(l, _)| l).expect("the result tab");
        ws.dock.set_focused_node_and_surface(loc);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| t.table.name == "u" && !t.data.loading && t.data.rows.len() == 1));
    }

    /// Database operations go through the session and the list refreshes.
    #[test]
    fn database_operations_through_the_workspace() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.submit_dbop(dbzilla_session::DbOp::Create(dbzilla_drivers::NewDatabase { name: "other".into(), encoding: None, collation: None }));
        wait(&mut ws, |w| w.databases.iter().any(|d| d.name == "other.sqlite3"));
        ws.submit_dbop(dbzilla_session::DbOp::Drop("other.sqlite3".into()));
        wait(&mut ws, |w| !w.databases.iter().any(|d| d.name == "other.sqlite3"));
        ws.submit_dbop(dbzilla_session::DbOp::Drop("w.sqlite3".into()));
        wait(&mut ws, |w| w.notices.iter().any(|(err, m)| *err && m.contains("currently open")));
    }

    /// Safe mode: dropping a database waits for confirmation; the level can only leave Safe mode with the password.
    #[test]
    fn safe_mode_for_databases_and_level_changes() {
        use dbzilla_core::safemode::SafeLevel;
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.conn.safe_level = SafeLevel::Alert1;
        ws.submit_dbop(dbzilla_session::DbOp::Drop("w.sqlite3".into()));
        assert!(ws.gate.as_ref().is_some_and(|g| g.statements[0].starts_with("DROP DATABASE")), "the drop waits for confirmation");
        ws.gate = None;
        ws.conn.safe_level = SafeLevel::Silent;
        ws.request_safe_level(SafeLevel::Alert2);
        assert_eq!(ws.conn.safe_level, SafeLevel::Alert2);
        assert!(matches!(ws.outputs.last(), Some(WsOutput::SetSafeLevel(SafeLevel::Alert2))));
        ws.password = Some("secret".into());
        ws.conn.safe_level = SafeLevel::Safe1;
        ws.request_safe_level(SafeLevel::Silent);
        assert_eq!(ws.conn.safe_level, SafeLevel::Safe1, "leaving Safe mode needs the password");
        assert!(ws.level_dialog.is_some());
    }

    /// A timeout event raises the dialog instead of failing the query.
    #[test]
    fn timeout_event_opens_the_wait_or_reconnect_dialog() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        let req = RequestId::next();
        ws.apply(Event::Timeout { req });
        assert_eq!(ws.timeout_req, Some(req));
    }

    // ---- real PostgreSQL and MySQL servers (skipped without Docker) ----

    /// Opens a workspace on a remote server.
    fn remote(rt: &tokio::runtime::Runtime, cfg: ConnectConfig) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = StoredConnection::new("remote", cfg.driver);
        conn.host = cfg.host.clone();
        conn.port = cfg.port;
        conn.user = cfg.user.clone();
        conn.database = cfg.database.clone();
        let session = spawn(rt.handle(), SessionConfig { query_timeout: None, keep_alive: None }, default_factory(), Arc::new(|| {}));
        session.send(Command::Connect(cfg));
        let stores = Stores { history: HistoryStore::new(dir.path().join("history"), true), diagrams: DiagramStore::new(dir.path().join("diagrams.json")), metrics: MetricsStore::new(dir.path().join("metrics.json")), queries: QueryStore::new(dir.path().join("queries")) };
        let mut ws = Workspace::new(conn, session, 1000, 300, None, stores);
        wait(&mut ws, |w| matches!(w.status, WsStatus::Connected(_)) && w.schema_loaded);
        (dir, ws)
    }

    /// Runs statements on the server outside the workspace.
    fn server_exec(rt: &tokio::runtime::Runtime, cfg: &ConnectConfig, stmts: &[&str]) {
        rt.block_on(async {
            let mut c = dbzilla_drivers::driver_for(cfg.driver).unwrap().connect(cfg).await.unwrap();
            c.execute_batch(&stmts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), false).await.unwrap();
        });
    }

    /// First column of the first row of a query.
    fn server_scalar(rt: &tokio::runtime::Runtime, cfg: &ConnectConfig, sql: &str) -> Value {
        rt.block_on(async {
            let mut c = dbzilla_drivers::driver_for(cfg.driver).unwrap().connect(cfg).await.unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<dbzilla_drivers::RowBatch>(8);
            let reader = tokio::spawn(async move {
                let mut rows = Vec::new();
                while let Some(b) = rx.recv().await {
                    rows.extend(b.rows);
                }
                rows
            });
            c.query_stream(sql, dbzilla_drivers::QueryOpts::default(), tx).await.unwrap();
            reader.await.unwrap().first().and_then(|r| r.first().cloned()).unwrap_or(Value::Null)
        })
    }

    /// Waits until the server answers.
    fn wait_for_server(rt: &tokio::runtime::Runtime, cfg: &ConnectConfig) {
        rt.block_on(async {
            for _ in 0..240 {
                if let Ok(mut c) = dbzilla_drivers::driver_for(cfg.driver).unwrap().connect(cfg).await {
                    if c.ping().await.is_ok() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            panic!("the database server did not become ready");
        });
    }

    /// Edit, filter, add a column and insert on a table of a real server.
    fn exercise_server(rt: &tokio::runtime::Runtime, cfg: ConnectConfig, create: &[&str], table: &str, name_filter: dbzilla_core::filter::FilterOp, quote: char) {
        server_exec(rt, &cfg, create);
        let (_dir, mut ws) = remote(rt, cfg.clone());
        ws.load_schema();
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == table));
        let target = ws.objects.iter().find(|o| o.table.name == table).map(|o| o.table.clone()).unwrap();
        ws.open_table(target.clone(), OpenMode::Pin);
        let loc = ws.dock.iter_all_tabs().next().map(|(l, _)| l).unwrap();
        ws.dock.set_focused_node_and_surface(loc);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 3 && !t.data.key_cols.is_empty()));
        // edit a cell and commit
        let (_, t) = ws.active_tab_mut().unwrap();
        t.data.pending.edit_cell(&mut t.data.rows, 0, 1, CellValue::Value(Value::Text("ALPHA2".into())));
        ws.commit_active();
        wait(&mut ws, |w| !w.has_pending() && w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && !t.data.committing));
        assert_eq!(server_scalar(rt, &cfg, &format!("SELECT name FROM {q}{table}{q} WHERE id = 1", q = quote)), Value::Text("ALPHA2".into()));
        // case insensitive filter
        let id = ws.active_tab_mut().unwrap().0;
        let f = Filter { enabled: true, column: dbzilla_core::filter::FilterColumn::Column("name".into()), op: name_filter, value: "beta".into() };
        ws.apply_filters(id, vec![f], false);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 1));
        ws.apply_filters(id, vec![], false);
        wait(&mut ws, |w| w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && t.data.rows.len() == 3));
        // insert a row with generated key
        let (_, t) = ws.active_tab_mut().unwrap();
        let cols = super::datatab::merge_columns(&t.data.columns, t.structure.as_ref());
        let r = t.data.pending.insert_row(&mut t.data.rows, &cols);
        t.data.pending.edit_cell(&mut t.data.rows, r, 1, CellValue::Value(Value::Text("delta".into())));
        ws.commit_active();
        wait(&mut ws, |w| !w.has_pending() && w.active_tab_mut().is_some_and(|(_, t)| !t.data.loading && !t.data.committing && t.data.rows.len() == 4));
        // add a column through the structure editor
        {
            let (_, t) = ws.active_tab_mut().unwrap();
            t.view = TabView::Structure;
        }
        wait(&mut ws, structure_ready);
        {
            let (_, t) = ws.active_tab_mut().unwrap();
            let mut c = super::super::structdraft::ColDraft::blank("integer");
            c.name = "extra".into();
            c.default = "5".into();
            t.draft.as_mut().unwrap().cols.push(c);
        }
        ws.commit_active();
        wait(&mut ws, |w| structure_ready(w) && w.active_tab_mut().is_some_and(|(_, t)| t.structure.as_ref().is_some_and(|s| s.columns.iter().any(|c| c.name == "extra"))));
        assert_eq!(server_scalar(rt, &cfg, &format!("SELECT extra FROM {q}{table}{q} WHERE id = 1", q = quote)), Value::I64(5));
    }

    /// End to end on a real PostgreSQL server.
    #[test]
    fn postgres_edit_filter_insert_and_structure() {
        const LABEL: &str = "dbzilla-test-app-pg";
        let Some(c) = crate::testsupport::start(LABEL, "postgres:16-alpine", &[("POSTGRES_PASSWORD", "pw")], 5432) else {
            return eprintln!("SKIPPED: docker is not available");
        };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let cfg = ConnectConfig { driver: DriverId::Postgres, host: "127.0.0.1".into(), port: Some(c.port), user: "postgres".into(), password: Some("pw".into()), database: "postgres".into(), ssl: SslMode::Disabled, bootstrap: vec![] };
        wait_for_server(&rt, &cfg);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exercise_server(
                &rt,
                cfg.clone(),
                &["CREATE TABLE items (id serial PRIMARY KEY, name text NOT NULL, qty int DEFAULT 0)", "INSERT INTO items (name, qty) VALUES ('alpha', 1), ('Beta', 2), ('gamma', 3)"],
                "items",
                dbzilla_core::filter::FilterOp::IContains,
                '"',
            );
        }));
        crate::testsupport::cleanup(LABEL);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// End to end on a real MySQL server.
    #[test]
    fn mysql_edit_filter_insert_and_structure() {
        const LABEL: &str = "dbzilla-test-app-mysql";
        let Some(c) = crate::testsupport::start(LABEL, "mysql:8.4", &[("MYSQL_ROOT_PASSWORD", "pw"), ("MYSQL_DATABASE", "shop")], 3306) else {
            return eprintln!("SKIPPED: docker is not available");
        };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let cfg = ConnectConfig { driver: DriverId::MySql, host: "127.0.0.1".into(), port: Some(c.port), user: "root".into(), password: Some("pw".into()), database: "shop".into(), ssl: SslMode::Disabled, bootstrap: vec![] };
        wait_for_server(&rt, &cfg);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exercise_server(
                &rt,
                cfg.clone(),
                &["CREATE TABLE items (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(40) NOT NULL, qty INT DEFAULT 0) ENGINE=InnoDB", "INSERT INTO items (name, qty) VALUES ('alpha', 1), ('Beta', 2), ('gamma', 3)"],
                "items",
                dbzilla_core::filter::FilterOp::Contains,
                '`',
            );
        }));
        crate::testsupport::cleanup(LABEL);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// Export and import jobs run on their own connection and report back.
    #[test]
    fn export_then_import_jobs_round_trip() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (dir, path, mut ws) = connected(&rt);
        let cfg = ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: path.to_string_lossy().into_owned(), ssl: SslMode::Disabled, bootstrap: vec![] };
        ws.set_job_env(JobEnv { rt: rt.handle().clone(), cfg, names: dbzilla_io::naming::NameContext::now("c", "d", "") }, Arc::new(|| {}));
        wait(&mut ws, |w| w.objects.iter().any(|o| o.table.name == "t"));
        let out = dir.path().join("out");
        let mut d = ExportDialog::new(&ws.objects, Some(&TableRef::simple("t")), None);
        d.dir = out.to_string_lossy().into_owned();
        let (sources, opts) = d.build().unwrap();
        ws.start_export("Export of t".into(), sources, opts);
        wait(&mut ws, |w| w.jobs.jobs.iter().all(|j| matches!(j.state, JobState::Done(_))));
        let file = out.join("t.csv");
        assert!(file.exists(), "the export wrote the file");
        let mut imp = ImportDialog::new(&ws.objects, None);
        imp.path = file.to_string_lossy().into_owned();
        imp.new_name = "t_copy".into();
        imp.load(Arc::new(|| {}));
        wait(&mut ws, |_| {
            imp.poll();
            !imp.is_loading()
        });
        assert!(imp.error.is_none(), "{:?}", imp.error);
        let req = imp.build(None).unwrap();
        ws.start_import("Import of t.csv".into(), req);
        wait(&mut ws, |w| w.jobs.jobs.len() == 2 && w.jobs.jobs.iter().all(|j| !matches!(j.state, JobState::Running { .. })));
        assert!(ws.jobs.jobs.iter().all(|j| matches!(j.state, JobState::Done(_))), "{:?}", ws.jobs.jobs.iter().map(|j| j.state.clone()).collect::<Vec<_>>());
        let conn = rusqlite::Connection::open(&path).unwrap();
        let a: i64 = conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        let b: i64 = conn.query_row("SELECT count(*) FROM t_copy", [], |r| r.get(0)).unwrap();
        assert_eq!(a, b);
        assert!(ws.notices.iter().any(|(err, m)| !*err && m.contains("Imported")));
    }

    /// Review I10: passwords typed in SQL never reach the history file.
    #[test]
    fn history_redacts_passwords() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_sql_focused(&mut ws, "SELECT 'x' AS a, 1 AS password_note; CREATE USER u IDENTIFIED BY 'S3cret'");
        ws.run_sql(true);
        wait(&mut ws, |w| !sql_running(w));
        let entries = ws.history.search(&ws.conn.id, "", 10).unwrap();
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| !e.sql.contains("S3cret")), "{entries:?}");
    }

    /// Review I2: an import waits for the safe-mode answer.
    #[test]
    fn import_is_gated_by_safe_mode() {
        use dbzilla_core::safemode::SafeLevel;
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        ws.conn.safe_level = SafeLevel::Alert1;
        ws.start_import("Import of dump.sql".into(), jobs::ImportRequest::Dump("DROP TABLE t".into()));
        assert!(ws.gate.as_ref().is_some_and(|g| g.statements[0].starts_with("DROP TABLE")), "the dump waits for confirmation");
        assert!(ws.jobs.jobs.is_empty(), "no job started yet");
    }

    /// Review item 6: a raw filter with several statements is refused with an explanation.
    #[test]
    fn raw_filter_with_several_statements_is_refused() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let (_dir, _path, mut ws) = connected(&rt);
        open_t(&mut ws);
        let id = ws.active_tab_mut().unwrap().0;
        let f = Filter { enabled: true, column: dbzilla_core::filter::FilterColumn::Raw, op: dbzilla_core::filter::FilterOp::Eq, value: "1=1; DELETE FROM t".into() };
        ws.apply_filters(id, vec![f], false);
        assert!(ws.notices.iter().any(|(err, m)| *err && m.contains("single expression")));
        assert!(ws.active_tab_mut().unwrap().1.data.filters.is_empty(), "the filter was not applied");
    }
}

