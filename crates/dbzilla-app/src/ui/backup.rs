//! Backup and restore sheet (SPEC 12.3): runs the dump tools of the server, or archives an SQLite file.
//! The work happens on its own thread, the tool output streams into the sheet.

use super::jobs::JobEnv;
use super::widgets;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_drivers::{driver_for, NewDatabase};
use dbzilla_io::backup::{self, FileIo, Target, ToolPlan};
use dbzilla_io::naming::render_name;
use dbzilla_io::IoError;
use dbzilla_store::backup::{BackupPrefs, BackupStore};
use eframe::egui::{self, Align2, RichText, Ui, Vec2};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Pattern of the suggested backup file name.
const NAME_PATTERN: &str = "{connection}_{database}_{date}_{time}";
/// Lines of tool output kept in the sheet.
const LOG_MAX: usize = 2000;

/// What a run does once it starts.
#[derive(Debug, Clone)]
enum Job {
    Tool { plan: ToolPlan, new_database: Option<String> },
    SqliteBackup { db: PathBuf, out: PathBuf },
    SqliteRestore { archive: PathBuf, target: PathBuf, overwrite: bool },
}

/// Messages of the worker thread.
enum Msg {
    Log(String),
    Finished(Result<String, String>),
}

/// Requests of the sheet toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum BackupAction {
    /// A restore changes the data: the workspace asks the safe mode about these lines, then calls `confirm_restore`.
    Restore(Vec<String>),
    /// Close the sheet.
    Dismiss,
}

/// State of the sheet.
pub struct BackupSheet {
    driver: DriverId,
    restore: bool,
    /// Databases of the server to pick from.
    databases: Vec<String>,
    database: String,
    new_database: String,
    use_new: bool,
    target: Target,
    /// Database file of an SQLite connection.
    sqlite_path: String,
    prefs: BackupPrefs,
    /// Backup output or restore input.
    file: String,
    replace_existing: bool,
    suggestion: String,
    store: BackupStore,
    conn_id: String,
    env: JobEnv,
    repaint: Arc<dyn Fn() + Send + Sync>,
    pending: Option<Job>,
    log: Vec<String>,
    running: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    error: Option<String>,
    /// What the last run did: `(is_error, text)`.
    result: Option<(bool, String)>,
}

impl BackupSheet {
    /// Opens the sheet on the Backup or Restore tab, with what the connection remembered.
    pub fn open(env: &JobEnv, repaint: Arc<dyn Fn() + Send + Sync>, store: BackupStore, conn_id: &str, databases: Vec<String>, current_db: &str, restore: bool) -> Self {
        let cfg = &env.cfg;
        let mut prefs = store.load(conn_id);
        if prefs.backup_options.is_empty() {
            prefs.backup_options = backup::default_backup_options(cfg.driver).to_owned();
        }
        if prefs.restore_options.is_empty() {
            prefs.restore_options = backup::default_restore_options(cfg.driver).to_owned();
        }
        let database = if databases.iter().any(|d| d == current_db) { current_db.to_owned() } else { databases.first().cloned().unwrap_or_else(|| current_db.to_owned()) };
        let target = Target { driver: cfg.driver, host: cfg.host.clone(), port: cfg.port, user: cfg.user.clone(), password: cfg.password.clone(), ssl: cfg.ssl };
        let (tx, rx) = channel();
        let mut me = Self {
            driver: cfg.driver,
            restore,
            databases,
            database,
            new_database: String::new(),
            use_new: false,
            target,
            sqlite_path: cfg.database.clone(),
            prefs,
            file: String::new(),
            replace_existing: false,
            suggestion: String::new(),
            store,
            conn_id: conn_id.to_owned(),
            env: env.clone(),
            repaint,
            pending: None,
            log: Vec::new(),
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
            error: None,
            result: None,
        };
        me.refresh_suggestion();
        me
    }

    /// Switches to the Backup or Restore tab.
    pub fn show_tab(&mut self, restore: bool) {
        self.restore = restore;
        self.error = None;
    }

    /// Recomputes the file name offered by the Browse dialog.
    fn refresh_suggestion(&mut self) {
        let mut ctx = self.env.names.clone();
        ctx.database = if self.driver == DriverId::Sqlite { Path::new(&self.sqlite_path).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()) } else { self.database.clone() };
        let ext = match self.driver {
            DriverId::Sqlite => "tar.bz2",
            DriverId::Postgres if backup::is_custom_format(&backup::split_options(&self.prefs.backup_options)) => "dump",
            _ if self.prefs.gzip => "sql.gz",
            _ => "sql",
        };
        self.suggestion = format!("{}.{ext}", render_name(NAME_PATTERN, "", &ctx));
    }

    /// Directory of the tools, when one is set.
    fn tool_dir(&self) -> Option<PathBuf> {
        let d = self.prefs.tool_dir.trim();
        if d.is_empty() { None } else { Some(PathBuf::from(d)) }
    }

    /// Database the run works on: the typed new name for a restore, else the picked one.
    fn chosen_database(&self) -> String {
        if self.restore && self.use_new { self.new_database.trim().to_owned() } else { self.database.clone() }
    }

    /// Builds what a run would do, or says why it cannot.
    fn prepare(&self) -> Result<Job, String> {
        let file = self.file.trim();
        if file.is_empty() {
            return Err(if self.restore { "choose the file to restore" } else { "choose where to save the backup" }.into());
        }
        let path = PathBuf::from(file);
        if self.driver == DriverId::Sqlite {
            let db = PathBuf::from(&self.sqlite_path);
            return Ok(if self.restore {
                let target = if self.replace_existing { db } else { sibling_name(&db) };
                Job::SqliteRestore { archive: path, target, overwrite: self.replace_existing }
            } else {
                Job::SqliteBackup { db, out: path }
            });
        }
        let database = self.chosen_database();
        if self.restore {
            if self.use_new && database.is_empty() {
                return Err("type the name of the new database".into());
            }
            let plan = backup::restore_plan(&self.target, &database, &self.prefs.restore_options, &path).map_err(|e| e.to_string())?;
            Ok(Job::Tool { plan, new_database: self.use_new.then_some(database) })
        } else {
            let plan = backup::backup_plan(&self.target, &database, &self.prefs.backup_options, &path, self.prefs.gzip).map_err(|e| e.to_string())?;
            Ok(Job::Tool { plan, new_database: None })
        }
    }

    /// Lines the safe mode shows before a restore.
    fn statements(&self, job: &Job) -> Vec<String> {
        match job {
            Job::Tool { plan, new_database } => {
                let mut lines = Vec::new();
                if let Some(n) = new_database {
                    lines.push(format!("CREATE DATABASE {n}"));
                }
                let into = plan.args.iter().skip_while(|a| *a != "-d").nth(1).cloned().unwrap_or_else(|| self.chosen_database());
                lines.push(format!("-- {} loads {} into {into}", plan.tool, self.file.trim()));
                lines
            }
            Job::SqliteRestore { archive, target, overwrite } => vec![format!("-- extract {} to {}{}", archive.display(), target.display(), if *overwrite { " (replacing existing files)" } else { "" })],
            Job::SqliteBackup { .. } => Vec::new(),
        }
    }

    /// Starts the run of the sheet; a restore is held until the safe mode agrees.
    fn start(&mut self) -> Option<BackupAction> {
        self.error = None;
        let job = match self.prepare() {
            Ok(j) => j,
            Err(e) => {
                self.error = Some(e);
                return None;
            }
        };
        if let Err(e) = self.store.save(&self.conn_id, &self.prefs) {
            self.error = Some(format!("cannot remember the options: {e}"));
        }
        if self.restore {
            let lines = self.statements(&job);
            self.pending = Some(job);
            return Some(BackupAction::Restore(lines));
        }
        self.spawn(job);
        None
    }

    /// Runs the restore the safe mode agreed to.
    pub fn confirm_restore(&mut self) {
        if let Some(job) = self.pending.take() {
            self.spawn(job);
        }
    }

    /// Runs a job on its own thread.
    fn spawn(&mut self, job: Job) {
        self.running = true;
        self.result = None;
        self.log.clear();
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, repaint, cancel, env, tool_dir) = (self.tx.clone(), self.repaint.clone(), self.cancel.clone(), self.env.clone(), self.tool_dir());
        std::thread::spawn(move || {
            let log: backup::LogFn = {
                let (tx, repaint) = (tx.clone(), repaint.clone());
                Arc::new(move |line| {
                    let _ = tx.send(Msg::Log(line));
                    repaint();
                })
            };
            let outcome = run_job(&job, &env, tool_dir.as_deref(), log, &cancel);
            let _ = tx.send(Msg::Finished(match outcome {
                Ok(text) => Ok(text),
                Err(IoError::Cancelled) => Err("cancelled".to_owned()),
                Err(e) => Err(e.to_string()),
            }));
            repaint();
        });
    }

    /// Takes in what the worker said; returns the outcome when a run has just finished.
    pub fn poll(&mut self) -> Option<(bool, String)> {
        let mut finished = None;
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(l) => {
                    self.log.push(l);
                    if self.log.len() > LOG_MAX {
                        self.log.remove(0);
                    }
                }
                Msg::Finished(r) => {
                    self.running = false;
                    let what = if self.restore { "Restore" } else { "Backup" };
                    let outcome = match r {
                        Ok(t) => (false, format!("{what} finished: {t}")),
                        Err(e) => (true, format!("{what} failed: {e}")),
                    };
                    self.result = Some(outcome.clone());
                    finished = Some(outcome);
                }
            }
        }
        finished
    }
}

/// Runs a job to the end on the current thread.
fn run_job(job: &Job, env: &JobEnv, tool_dir: Option<&Path>, log: backup::LogFn, cancel: &AtomicBool) -> Result<String, IoError> {
    match job {
        Job::SqliteBackup { db, out } => {
            let n = backup::sqlite_backup(db, out)?;
            Ok(format!("{n} file(s) archived in {}", out.display()))
        }
        Job::SqliteRestore { archive, target, overwrite } => {
            let files = backup::sqlite_restore(archive, target, *overwrite)?;
            Ok(format!("{} file(s) written, database at {}", files.len(), target.display()))
        }
        Job::Tool { plan, new_database } => {
            if let Some(name) = new_database {
                let req = NewDatabase { name: name.clone(), encoding: None, collation: None };
                let cfg = env.cfg.clone();
                env.rt.block_on(async move { driver_for(cfg.driver).map_err(IoError::Driver)?.connect(&cfg).await?.create_database(&req).await.map_err(IoError::Driver) })?;
                log(format!("created database {name}"));
            }
            backup::run_plan(plan, tool_dir, log, cancel)?;
            Ok(match &plan.io {
                FileIo::SaveStdout { path, .. } => format!("saved to {}", path.display()),
                _ => "done".to_owned(),
            })
        }
    }
}

/// The path next to `db` the SQLite restore writes to when nothing may be replaced.
pub fn sibling_name(db: &Path) -> PathBuf {
    let stem = db.file_stem().map_or_else(|| "database".to_owned(), |s| s.to_string_lossy().into_owned());
    let ext = db.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    db.with_file_name(format!("{stem}-restored{ext}"))
}

/// Draws the sheet and returns what it asks of the workspace.
pub fn show(ctx: &egui::Context, sheet: &mut BackupSheet) -> Option<BackupAction> {
    let mut action = None;
    let mut open = true;
    egui::Window::new("Backup and restore").id(egui::Id::new("backup-sheet")).open(&mut open).default_width(560.0).resizable(true).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let before = sheet.restore;
            ui.selectable_value(&mut sheet.restore, false, "Backup");
            ui.selectable_value(&mut sheet.restore, true, "Restore");
            if before != sheet.restore {
                sheet.error = None;
            }
        });
        ui.add_space(6.0);
        ui.add_enabled_ui(!sheet.running, |ui| form(ui, sheet));
        ui.add_space(6.0);
        if let Some(e) = &sheet.error {
            ui.label(RichText::new(e).color(theme::ERROR));
        }
        log_view(ui, sheet);
        if let Some((error, text)) = &sheet.result {
            ui.label(RichText::new(text).color(if *error { theme::ERROR } else { theme::OK }));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if sheet.running {
                ui.spinner();
                if widgets::secondary_button(ui, "Cancel").clicked() {
                    sheet.cancel.store(true, Ordering::Relaxed);
                }
            } else {
                let label = if sheet.restore { "Start restore…" } else { "Start backup…" };
                if widgets::primary_button(ui, label).clicked() {
                    action = sheet.start();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::secondary_button(ui, "Dismiss").clicked() {
                    action = Some(BackupAction::Dismiss);
                }
            });
        });
    });
    if !open {
        action = Some(BackupAction::Dismiss);
    }
    action
}

/// Fields of the current tab.
fn form(ui: &mut Ui, s: &mut BackupSheet) {
    if s.driver == DriverId::Sqlite {
        widgets::field_label(ui, "Database file");
        ui.label(RichText::new(&s.sqlite_path).color(theme::MUTED));
        if s.restore {
            ui.label(RichText::new(format!("Files are extracted next to the original as {}", sibling_name(Path::new(&s.sqlite_path)).display())).color(theme::MUTED));
            ui.checkbox(&mut s.replace_existing, "Replace the database file and its companions (explicit confirmation)");
        } else {
            ui.label(RichText::new("The database and its -wal, -shm and -journal files go into a tar.bz2").color(theme::MUTED));
        }
    } else {
        widgets::field_label(ui, "Database");
        ui.horizontal(|ui| {
            if s.restore {
                ui.checkbox(&mut s.use_new, "New database");
            }
            if s.restore && s.use_new {
                widgets::input(ui, &mut s.new_database, "database name");
            } else {
                let before = s.database.clone();
                egui::ComboBox::from_id_salt("backup-db").selected_text(&s.database).show_ui(ui, |ui| {
                    for d in s.databases.clone() {
                        ui.selectable_value(&mut s.database, d.clone(), d);
                    }
                });
                if before != s.database {
                    s.refresh_suggestion();
                }
            }
        });
        widgets::field_label(ui, "Tools directory (empty: PATH)");
        ui.horizontal(|ui| {
            widgets::input(ui, &mut s.prefs.tool_dir, "/usr/lib/postgresql/17/bin");
            if widgets::secondary_button(ui, "Browse…").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    s.prefs.tool_dir = p.to_string_lossy().into_owned();
                }
            }
        });
        widgets::field_label(ui, "Options");
        let choices = backup::option_choices(s.driver, s.restore);
        let restore = s.restore;
        let mut picked = None;
        ui.horizontal(|ui| {
            let text = if restore { &mut s.prefs.restore_options } else { &mut s.prefs.backup_options };
            widgets::input(ui, text, "extra options");
            ui.menu_button("Add option…", |ui| {
                for c in choices {
                    if ui.button(*c).clicked() {
                        picked = Some(*c);
                        ui.close_menu();
                    }
                }
            });
        });
        if let Some(c) = picked {
            let text = if restore { &mut s.prefs.restore_options } else { &mut s.prefs.backup_options };
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(c);
            s.refresh_suggestion();
        }
        if !s.restore {
            let custom = s.driver == DriverId::Postgres && backup::is_custom_format(&backup::split_options(&s.prefs.backup_options));
            let before = s.prefs.gzip;
            ui.add_enabled(!custom, egui::Checkbox::new(&mut s.prefs.gzip, "Compress with gzip"));
            if custom {
                ui.label(RichText::new("The custom format is compressed already").color(theme::MUTED));
            }
            if before != s.prefs.gzip {
                s.refresh_suggestion();
            }
        }
    }
    widgets::field_label(ui, if s.restore { "Backup file" } else { "Save to" });
    ui.horizontal(|ui| {
        widgets::input(ui, &mut s.file, &s.suggestion.clone());
        if widgets::secondary_button(ui, "Browse…").clicked() {
            let dialog = rfd::FileDialog::new().set_file_name(&s.suggestion);
            let picked = if s.restore { dialog.pick_file() } else { dialog.save_file() };
            if let Some(p) = picked {
                s.file = p.to_string_lossy().into_owned();
            }
        }
    });
}

/// The tool output, newest line last.
fn log_view(ui: &mut Ui, s: &BackupSheet) {
    if s.log.is_empty() {
        return;
    }
    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).id_salt("backup-log").show(ui, |ui| {
        for l in &s.log {
            ui.label(RichText::new(l).monospace().size(12.0).color(theme::MUTED));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_sheet(dir: &Path, restore: bool) -> (BackupSheet, tokio::runtime::Runtime) {
        let db = dir.join("app.sqlite3");
        std::fs::write(&db, b"main").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cfg = dbzilla_drivers::ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: db.to_string_lossy().into_owned(), ssl: Default::default(), bootstrap: Vec::new() };
        let env = JobEnv { rt: rt.handle().clone(), cfg, names: dbzilla_io::naming::NameContext::now("c", "app", "") };
        let store = BackupStore::new(dir.join("backup.json"));
        (BackupSheet::open(&env, Arc::new(|| {}), store, "c1", Vec::new(), "main", restore), rt)
    }

    fn finish(s: &mut BackupSheet) -> (bool, String) {
        for _ in 0..200 {
            if let Some(o) = s.poll() {
                return o;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the run did not finish");
    }

    #[test]
    fn sqlite_backup_then_restore_beside_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _rt) = sqlite_sheet(dir.path(), false);
        s.file = dir.path().join("b.tar.bz2").to_string_lossy().into_owned();
        assert!(s.start().is_none());
        assert!(!finish(&mut s).0);
        s.show_tab(true);
        let Some(BackupAction::Restore(lines)) = s.start() else { panic!("a restore waits for the safe mode") };
        assert!(lines[0].contains("app-restored.sqlite3"), "{lines:?}");
        s.confirm_restore();
        assert!(!finish(&mut s).0);
        assert_eq!(std::fs::read(dir.path().join("app-restored.sqlite3")).unwrap(), b"main");
        // A second restore would overwrite the first one: it must fail unless confirmed.
        s.start();
        s.confirm_restore();
        let (error, text) = finish(&mut s);
        assert!(error && text.contains("already exists"), "{text}");
    }

    #[test]
    fn backup_needs_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _rt) = sqlite_sheet(dir.path(), false);
        assert!(s.start().is_none());
        assert!(s.error.is_some() && !s.running);
    }

    #[test]
    fn sibling_name_keeps_the_extension() {
        assert_eq!(sibling_name(Path::new("/data/app.sqlite3")), PathBuf::from("/data/app-restored.sqlite3"));
        assert_eq!(sibling_name(Path::new("/data/app")), PathBuf::from("/data/app-restored"));
    }
}
