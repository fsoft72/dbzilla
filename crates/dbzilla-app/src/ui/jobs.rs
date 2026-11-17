//! Background import/export jobs, their dialogs and the notifications bell (SPEC 12).

use super::widgets;
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef};
use dbzilla_drivers::{driver_for, ConnectConfig};
use dbzilla_io::export::{run_export, CsvExportOptions, ExportFormat, ExportOptions, ExportSource, LineBreak, SqlExportOptions};
use dbzilla_io::import::{detect_types, match_columns, parse_csv, parse_json, preview, run_import, run_sql_dump, CsvImportOptions, ImportPlan, InsertMode, MatchStrategy, Parsed};
use dbzilla_io::naming::NameContext;
use dbzilla_io::{IoError, Progress};
use eframe::egui::{self, RichText, Ui};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Rows of the import preview table.
const PREVIEW_SHOWN: usize = 6;

/// What a job needs to open its own connection.
#[derive(Clone)]
pub struct JobEnv {
    pub rt: tokio::runtime::Handle,
    pub cfg: ConnectConfig,
    /// Display names for the file name pattern.
    pub names: NameContext,
}

/// State of one job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Running { item: String, rows: u64 },
    Done(String),
    Failed(String),
    Cancelled,
}

/// A background job listed in the bell popover.
pub struct Job {
    pub id: u64,
    pub title: String,
    pub state: JobState,
    pub files: Vec<PathBuf>,
    cancel: Arc<AtomicBool>,
}

/// Message from a job thread.
enum JobMsg {
    Progress(Progress),
    Finished(Result<(String, Vec<PathBuf>), IoError>),
}

/// Owns the jobs of a workspace.
pub struct JobCenter {
    pub jobs: Vec<Job>,
    tx: Sender<(u64, JobMsg)>,
    rx: Receiver<(u64, JobMsg)>,
    next: u64,
    /// Finished jobs the user has not looked at yet.
    pub unread: usize,
}

impl Default for JobCenter {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { jobs: Vec::new(), tx, rx, next: 1, unread: 0 }
    }
}

impl JobCenter {
    /// Registers a job and returns its id, message sender and cancel flag.
    fn add(&mut self, title: String) -> (u64, Sender<(u64, JobMsg)>, Arc<AtomicBool>) {
        let id = self.next;
        self.next += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.jobs.insert(0, Job { id, title, state: JobState::Running { item: String::new(), rows: 0 }, files: Vec::new(), cancel: cancel.clone() });
        (id, self.tx.clone(), cancel)
    }

    /// Applies the messages of the job threads; returns the finished jobs as `(title, state)`.
    pub fn poll(&mut self) -> Vec<(String, JobState)> {
        let mut finished = Vec::new();
        while let Ok((id, msg)) = self.rx.try_recv() {
            let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else { continue };
            match msg {
                JobMsg::Progress(p) => job.state = JobState::Running { item: p.item, rows: p.rows },
                JobMsg::Finished(res) => {
                    job.state = match res {
                        Ok((text, files)) => {
                            job.files = files;
                            JobState::Done(text)
                        }
                        Err(IoError::Cancelled) => JobState::Cancelled,
                        Err(e) => JobState::Failed(e.to_string()),
                    };
                    self.unread += 1;
                    finished.push((job.title.clone(), job.state.clone()));
                }
            }
        }
        finished
    }

    /// Number of running jobs.
    pub fn running(&self) -> usize {
        self.jobs.iter().filter(|j| matches!(j.state, JobState::Running { .. })).count()
    }

    /// Starts an export on its own connection.
    pub fn spawn_export(&mut self, env: &JobEnv, repaint: Arc<dyn Fn() + Send + Sync>, title: String, sources: Vec<ExportSource>, opts: ExportOptions) {
        let (id, tx, cancel) = self.add(title);
        let env = env.clone();
        env.rt.clone().spawn(async move {
            let progress_tx = tx.clone();
            let progress_repaint = repaint.clone();
            let progress = move |p: Progress| {
                let _ = progress_tx.send((id, JobMsg::Progress(p)));
                progress_repaint();
            };
            let result = async {
                let mut conn = driver_for(env.cfg.driver).map_err(IoError::Driver)?.connect(&env.cfg).await?;
                let files = run_export(conn.as_mut(), env.cfg.driver, &sources, &opts, &env.names, &progress, &cancel).await?;
                Ok((format!("Exported {} file(s) to {}", files.len(), opts.dir.display()), files))
            }
            .await;
            let _ = tx.send((id, JobMsg::Finished(result)));
            repaint();
        });
    }

    /// Starts an import on its own connection.
    pub fn spawn_import(&mut self, env: &JobEnv, repaint: Arc<dyn Fn() + Send + Sync>, title: String, req: ImportRequest) {
        let (id, tx, cancel) = self.add(title);
        let env = env.clone();
        env.rt.clone().spawn(async move {
            let progress_tx = tx.clone();
            let progress_repaint = repaint.clone();
            let progress = move |p: Progress| {
                let _ = progress_tx.send((id, JobMsg::Progress(p)));
                progress_repaint();
            };
            let result = run_request(&env, req, &progress, &cancel).await;
            let _ = tx.send((id, JobMsg::Finished(result)));
            repaint();
        });
    }
}

/// What to import.
pub enum ImportRequest {
    /// Rows of a CSV or JSON file.
    Rows { table: TableRef, create: bool, parsed: Arc<Parsed>, strategy: MatchStrategy, mode: InsertMode, key_columns: Vec<String> },
    /// An SQL dump.
    Dump(Arc<str>),
}

/// Runs an import request on a fresh connection.
async fn run_request(env: &JobEnv, req: ImportRequest, progress: &(dyn Fn(Progress) + Send + Sync), cancel: &AtomicBool) -> Result<(String, Vec<PathBuf>), IoError> {
    let driver = env.cfg.driver;
    let mut conn = driver_for(driver).map_err(IoError::Driver)?.connect(&env.cfg).await?;
    match req {
        ImportRequest::Dump(text) => {
            let n = run_sql_dump(conn.as_mut(), driver, &text, progress, cancel).await?;
            Ok((format!("Ran {n} statement(s)"), Vec::new()))
        }
        ImportRequest::Rows { table, create, parsed, strategy, mode, key_columns } => {
            let (create_cols, mapping) = if create {
                let cols: Vec<(String, String)> = parsed.headers.iter().zip(detect_types(&parsed)).map(|(h, t)| (h.clone(), t.sql_type(driver).to_owned())).collect();
                (Some(cols.clone()), cols.into_iter().map(Some).collect())
            } else {
                let st = conn.table_structure(&table).await?;
                let names: Vec<String> = st.columns.iter().map(|c| c.name.clone()).collect();
                let m = match_columns(strategy, &parsed.headers, &names).into_iter().map(|i| i.map(|i| (st.columns[i].name.clone(), st.columns[i].type_name.clone()))).collect();
                (None, m)
            };
            let plan = ImportPlan { table, create: create_cols, mapping, mode, key_columns };
            let summary = run_import(conn.as_mut(), driver, &plan, &parsed, progress, cancel).await?;
            Ok((format!("Imported {} row(s) into {}", summary.rows, plan.table.name), Vec::new()))
        }
    }
}

/// Wizard state of an export.
pub struct ExportDialog {
    pub tables: Vec<(TableRef, bool)>,
    pub format: ExportFormat,
    pub csv: CsvExportOptions,
    pub sql: SqlExportOptions,
    pub gzip: bool,
    pub dir: String,
    pub pattern: String,
    pub where_sql: String,
    pub order_sql: String,
    /// Columns of the first selected table, when a single table is exported.
    pub columns: Option<Vec<(String, bool)>>,
}

impl ExportDialog {
    /// New dialog listing every table; `selected` starts ticked.
    pub fn new(objects: &[ObjectInfo], selected: Option<&TableRef>, columns: Option<Vec<String>>) -> Self {
        let tables = objects.iter().filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView)).map(|o| (o.table.clone(), selected.is_some_and(|s| *s == o.table))).collect();
        let dir = dirs_home().join("Downloads");
        Self { tables, format: ExportFormat::Csv, csv: CsvExportOptions::default(), sql: SqlExportOptions::default(), gzip: false, dir: dir.to_string_lossy().into_owned(), pattern: "{table}".into(), where_sql: String::new(), order_sql: String::new(), columns: columns.map(|c| c.into_iter().map(|n| (n, true)).collect()) }
    }

    /// Builds the sources and options; the error is shown in the dialog.
    pub fn build(&self) -> Result<(Vec<ExportSource>, ExportOptions), String> {
        let picked: Vec<&TableRef> = self.tables.iter().filter(|(_, on)| *on).map(|(t, _)| t).collect();
        if picked.is_empty() {
            return Err("Select at least one table".into());
        }
        if self.dir.trim().is_empty() {
            return Err("Choose a destination folder".into());
        }
        for (label, text) in [("filter", &self.where_sql), ("order", &self.order_sql)] {
            if dbzilla_core::splitter::split_statements(text, DriverId::MySql).len() > 1 {
                return Err(format!("The {label} must be a single expression, not several statements"));
            }
        }
        let single = picked.len() == 1;
        let cols = self.columns.as_ref().filter(|_| single).map(|c| c.iter().filter(|(_, on)| *on).map(|(n, _)| n.clone()).collect::<Vec<_>>());
        if cols.as_ref().is_some_and(Vec::is_empty) {
            return Err("Select at least one column".into());
        }
        let sources = picked.iter().map(|t| ExportSource::Table { table: (*t).clone(), columns: cols.clone(), where_sql: Some(self.where_sql.clone()).filter(|w| single && !w.trim().is_empty()), order_sql: Some(self.order_sql.clone()).filter(|o| single && !o.trim().is_empty()), page: None }).collect();
        Ok((sources, ExportOptions { format: self.format, csv: self.csv.clone(), sql: self.sql.clone(), gzip: self.gzip, dir: PathBuf::from(self.dir.trim()), name_pattern: self.pattern.clone() }))
    }
}

/// Home directory, falling back to the current one.
fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// A checkbox row.
fn check(ui: &mut Ui, on: &mut bool, text: &str) {
    ui.checkbox(on, RichText::new(text).size(13.0));
}

/// Draws the export dialog; returns `Some(true)` to start and `Some(false)` to close.
pub fn export_dialog(ctx: &egui::Context, d: &mut ExportDialog, error: &mut Option<String>) -> Option<bool> {
    let mut decision = None;
    widgets::modal(ctx, "Export data", |ui| {
        ui.set_width(460.0);
        widgets::field_label(ui, "Format");
        ui.horizontal(|ui| {
            for (f, label) in [(ExportFormat::Csv, "CSV"), (ExportFormat::Json, "JSON"), (ExportFormat::Sql, "SQL"), (ExportFormat::Xlsx, "XLSX")] {
                ui.selectable_value(&mut d.format, f, label);
            }
        });
        widgets::field_label(ui, "Tables");
        egui::ScrollArea::vertical().max_height(120.0).id_salt("export_tables").show(ui, |ui| {
            for (t, on) in &mut d.tables {
                check(ui, on, &t.name);
            }
        });
        if let Some(cols) = d.columns.as_mut().filter(|_| d.tables.iter().filter(|(_, on)| *on).count() == 1) {
            widgets::field_label(ui, "Columns");
            egui::ScrollArea::vertical().max_height(90.0).id_salt("export_columns").show(ui, |ui| {
                for (n, on) in cols.iter_mut() {
                    check(ui, on, n);
                }
            });
        }
        if d.tables.iter().filter(|(_, on)| *on).count() == 1 && d.format != ExportFormat::Sql {
            widgets::field_label(ui, "Filter (WHERE, optional)");
            widgets::input(ui, &mut d.where_sql, "price > 10");
            widgets::field_label(ui, "Order (optional)");
            widgets::input(ui, &mut d.order_sql, "id DESC");
        }
        match d.format {
            ExportFormat::Csv | ExportFormat::Xlsx => {
                if d.format == ExportFormat::Csv {
                    ui.horizontal(|ui| {
                        let mut delim = d.csv.delimiter.to_string();
                        ui.label("Delimiter");
                        if ui.add(egui::TextEdit::singleline(&mut delim).desired_width(30.0)).changed() {
                            d.csv.delimiter = delim.chars().next().unwrap_or(',');
                        }
                        let mut dec = d.csv.decimal.to_string();
                        ui.label("Decimal");
                        if ui.add(egui::TextEdit::singleline(&mut dec).desired_width(30.0)).changed() {
                            d.csv.decimal = dec.chars().next().unwrap_or('.');
                        }
                    });
                    ui.horizontal(|ui| {
                        for (l, label) in [(LineBreak::Lf, "LF"), (LineBreak::CrLf, "CRLF"), (LineBreak::Cr, "CR")] {
                            ui.selectable_value(&mut d.csv.line_break, l, label);
                        }
                    });
                    check(ui, &mut d.csv.quote_always, "Quote every value");
                    check(ui, &mut d.csv.null_as_empty, "Write NULL as an empty value");
                    check(ui, &mut d.csv.newline_to_space, "Replace line breaks with a space");
                }
                check(ui, &mut d.csv.header, "Header row");
            }
            ExportFormat::Sql => {
                check(ui, &mut d.sql.structure, "Structure (CREATE TABLE)");
                check(ui, &mut d.sql.drop, "DROP TABLE IF EXISTS first");
                check(ui, &mut d.sql.data, "Data (INSERT)");
                check(ui, &mut d.gzip, "Compress with gzip");
            }
            ExportFormat::Json => {}
        }
        widgets::field_label(ui, "Destination folder");
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 100.0).max(100.0);
            ui.allocate_ui(egui::vec2(w, 32.0), |ui| widgets::input(ui, &mut d.dir, "/path/to/folder"));
            if widgets::secondary_button(ui, "Browse").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    d.dir = p.to_string_lossy().into_owned();
                }
            }
        });
        widgets::field_label(ui, "File name ({connection} {database} {table} {tag} {date} {time})");
        widgets::input(ui, &mut d.pattern, "{table}");
        if let Some(e) = error.as_ref() {
            ui.colored_label(theme::ERROR, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, "Export").clicked() {
                decision = Some(true);
            }
            if widgets::secondary_button(ui, "Cancel").clicked() {
                decision = Some(false);
            }
        });
    });
    decision
}

/// Kind of file being imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Csv,
    Json,
    SqlDump,
}

/// What the background reader produced.
enum Loaded {
    Rows(Arc<Parsed>),
    Dump(Arc<str>),
}

/// Wizard state of an import.
pub struct ImportDialog {
    pub path: String,
    pub kind: ImportKind,
    pub csv: CsvImportOptions,
    pub parsed: Option<Arc<Parsed>>,
    pub sql_text: Option<Arc<str>>,
    /// NULL marker of CSV files; empty means no marker.
    pub null_text: String,
    /// A file being read and parsed in the background.
    loading: Option<Receiver<Result<Loaded, String>>>,
    pub tables: Vec<TableRef>,
    pub target: Option<TableRef>,
    pub new_name: String,
    pub strategy: MatchStrategy,
    pub mode: InsertMode,
    pub key_columns: String,
    pub error: Option<String>,
}

impl ImportDialog {
    /// New dialog listing the tables of the database.
    pub fn new(objects: &[ObjectInfo], target: Option<TableRef>) -> Self {
        let tables = objects.iter().filter(|o| o.kind == ObjectKind::Table).map(|o| o.table.clone()).collect();
        Self { path: String::new(), kind: ImportKind::Csv, csv: CsvImportOptions::default(), parsed: None, sql_text: None, null_text: "NULL".into(), loading: None, tables, target, new_name: String::new(), strategy: MatchStrategy::NameAndOrder, mode: InsertMode::StopOnError, key_columns: String::new(), error: None }
    }

    /// Reads and parses the chosen file on a background thread, so a huge file never freezes the window.
    pub fn load(&mut self, repaint: Arc<dyn Fn() + Send + Sync>) {
        self.parsed = None;
        self.sql_text = None;
        self.error = None;
        let path = PathBuf::from(self.path.trim());
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        self.kind = match ext.as_str() {
            "json" => ImportKind::Json,
            "sql" => ImportKind::SqlDump,
            _ => ImportKind::Csv,
        };
        if self.new_name.is_empty() {
            self.new_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("imported").to_owned();
        }
        let (kind, mut csv) = (self.kind, self.csv.clone());
        csv.null_text = Some(self.null_text.clone()).filter(|t| !t.is_empty());
        let (tx, rx) = channel();
        self.loading = Some(rx);
        std::thread::spawn(move || {
            let result = std::fs::read(&path).map_err(|e| format!("Cannot read the file: {e}")).and_then(|bytes| match kind {
                ImportKind::Csv => parse_csv(&bytes, &csv).map(|p| Loaded::Rows(Arc::new(p))),
                ImportKind::Json => parse_json(&bytes).map(|p| Loaded::Rows(Arc::new(p))),
                ImportKind::SqlDump => dbzilla_io::import::decode_text(&bytes, &csv.encoding).map(|t| Loaded::Dump(Arc::from(t))),
            }
            .map_err(|e| e.to_string()));
            let _ = tx.send(result);
            repaint();
        });
    }

    /// True while a file is being read.
    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Takes the result of the background read, if it is ready.
    pub fn poll(&mut self) {
        let Some(rx) = &self.loading else { return };
        match rx.try_recv() {
            Ok(Ok(Loaded::Rows(p))) => self.parsed = Some(p),
            Ok(Ok(Loaded::Dump(t))) => self.sql_text = Some(t),
            Ok(Err(e)) => self.error = Some(e),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(_) => self.error = Some("The file reader stopped unexpectedly".into()),
        }
        self.loading = None;
    }

    /// Builds the request to run; the error is shown in the dialog.
    pub fn build(&self, default_schema: Option<&str>) -> Result<ImportRequest, String> {
        if let Some(text) = &self.sql_text {
            return Ok(ImportRequest::Dump(Arc::clone(text)));
        }
        let parsed = self.parsed.as_ref().map(Arc::clone).ok_or("Choose a file and press Load")?;
        let (table, create) = match &self.target {
            Some(t) => (t.clone(), false),
            None => {
                let name = self.new_name.trim();
                if name.is_empty() {
                    return Err("Choose a target table or type a name for a new one".into());
                }
                (TableRef { database: None, schema: default_schema.map(str::to_owned), name: name.to_owned() }, true)
            }
        };
        let key_columns = self.key_columns.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        Ok(ImportRequest::Rows { table, create, parsed, strategy: self.strategy, mode: self.mode, key_columns })
    }
}

/// Draws the import dialog; returns `Some(true)` to start and `Some(false)` to close.
pub fn import_dialog(ctx: &egui::Context, d: &mut ImportDialog, driver: DriverId, repaint: Arc<dyn Fn() + Send + Sync>) -> Option<bool> {
    let mut decision = None;
    d.poll();
    widgets::modal(ctx, "Import data", |ui| {
        ui.set_width(520.0);
        widgets::field_label(ui, "File (CSV, JSON or SQL dump)");
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 170.0).max(100.0);
            ui.allocate_ui(egui::vec2(w, 32.0), |ui| widgets::input(ui, &mut d.path, "/path/to/file.csv"));
            if widgets::secondary_button(ui, "Browse").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Data files", &["csv", "json", "sql", "txt"]).pick_file() {
                    d.path = p.to_string_lossy().into_owned();
                    d.load(repaint.clone());
                }
            }
            if widgets::secondary_button(ui, "Load").clicked() {
                d.load(repaint.clone());
            }
        });
        if d.kind == ImportKind::Csv {
            ui.horizontal(|ui| {
                let mut delim = d.csv.delimiter.to_string();
                ui.label("Delimiter");
                if ui.add(egui::TextEdit::singleline(&mut delim).desired_width(30.0)).changed() {
                    d.csv.delimiter = delim.chars().next().unwrap_or(',');
                }
                ui.label("Encoding");
                ui.add(egui::TextEdit::singleline(&mut d.csv.encoding).desired_width(90.0));
                check(ui, &mut d.csv.header, "Header row");
                ui.label("NULL text");
                ui.add(egui::TextEdit::singleline(&mut d.null_text).desired_width(60.0));
            });
        }
        if d.is_loading() {
            widgets::muted(ui, "Reading the file...");
        }
        if let Some(p) = &d.parsed {
            widgets::muted(ui, &format!("{} row(s), {} column(s)", p.rows.len(), p.headers.len()));
            let shown = preview(p.as_ref());
            egui::ScrollArea::both().max_height(140.0).id_salt("import_preview").show(ui, |ui| {
                egui::Grid::new("import_grid").striped(true).show(ui, |ui| {
                    for h in &shown.headers {
                        ui.label(RichText::new(h).strong().size(12.0));
                    }
                    ui.end_row();
                    for r in shown.rows.iter().take(PREVIEW_SHOWN) {
                        for c in r {
                            ui.label(RichText::new(c.as_deref().unwrap_or("NULL")).size(12.0).color(if c.is_some() { theme::TEXT } else { theme::MUTED }));
                        }
                        ui.end_row();
                    }
                });
            });
            widgets::field_label(ui, "Target");
            egui::ComboBox::from_id_salt("import_target").width(260.0).selected_text(d.target.as_ref().map_or("New table".to_owned(), |t| t.name.clone())).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.target, None, "New table");
                for t in d.tables.clone() {
                    let name = t.name.clone();
                    ui.selectable_value(&mut d.target, Some(t), name);
                }
            });
            if d.target.is_none() {
                widgets::input(ui, &mut d.new_name, "new table name");
            } else {
                ui.horizontal(|ui| {
                    for (s, label) in [(MatchStrategy::NameAndOrder, "Name then order"), (MatchStrategy::Name, "Name"), (MatchStrategy::NameCaseInsensitive, "Name (any case)"), (MatchStrategy::Order, "Order")] {
                        ui.selectable_value(&mut d.strategy, s, label);
                    }
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.mode, InsertMode::StopOnError, "Insert, stop on error");
                    ui.selectable_value(&mut d.mode, InsertMode::UpdateOnDuplicate, "Update on duplicate");
                    ui.selectable_value(&mut d.mode, InsertMode::Replace, "Replace");
                });
                if d.mode == InsertMode::UpdateOnDuplicate && driver != DriverId::MySql {
                    widgets::input(ui, &mut d.key_columns, "key columns, comma separated");
                }
            }
        }
        if d.sql_text.is_some() {
            widgets::muted(ui, "The statements of the dump will run one after another.");
        }
        if let Some(e) = &d.error {
            ui.colored_label(theme::ERROR, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(d.parsed.is_some() || d.sql_text.is_some(), |ui| {
                if widgets::primary_button(ui, "Import").clicked() {
                    decision = Some(true);
                }
            });
            if widgets::secondary_button(ui, "Cancel").clicked() {
                decision = Some(false);
            }
        });
    });
    decision
}

/// The bell button with the job list popover.
pub fn bell(ui: &mut Ui, jobs: &mut JobCenter) {
    let tip = if jobs.running() > 0 { format!("{} job(s) running", jobs.running()) } else { "Notifications".to_owned() };
    let resp = widgets::ico_button(ui, Ico::Alert, &tip);
    if jobs.unread > 0 || jobs.running() > 0 {
        let color = if jobs.running() > 0 { theme::SECONDARY } else { theme::OK };
        ui.painter().circle_filled(resp.rect.right_top() + egui::vec2(-5.0, 5.0), 3.5, color);
    }
    let popup = ui.make_persistent_id("jobs_popup");
    if resp.clicked() {
        jobs.unread = 0;
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup_below_widget(ui, popup, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(300.0);
        widgets::flat_menu_style(ui);
        if jobs.jobs.is_empty() {
            widgets::muted(ui, "No notifications");
        }
        let mut cancel = None;
        for j in jobs.jobs.iter().take(12) {
            ui.label(RichText::new(&j.title).size(13.0));
            match &j.state {
                JobState::Running { item, rows } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{item}: {rows} rows")).size(12.0).color(theme::MUTED));
                        if ui.small_button("Cancel").clicked() {
                            cancel = Some(j.id);
                        }
                    });
                }
                JobState::Done(t) => {
                    ui.label(RichText::new(t).size(12.0).color(theme::OK));
                }
                JobState::Failed(t) => {
                    ui.label(RichText::new(t).size(12.0).color(theme::ERROR));
                }
                JobState::Cancelled => {
                    ui.label(RichText::new("Cancelled").size(12.0).color(theme::MUTED));
                }
            }
            ui.separator();
        }
        if let Some(id) = cancel {
            if let Some(j) = jobs.jobs.iter().find(|j| j.id == id) {
                j.cancel.store(true, Ordering::SeqCst);
            }
        }
    });
}
