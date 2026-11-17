//! User management (SPEC 11.5): a sheet with the users of the server and their privileges. Edits stay
//! in the sheet; the statements that would apply them are shown as an SQL preview and run only after
//! the safe mode agrees. It works on its own connection.

use super::jobs::JobEnv;
use super::process::run;
use super::widgets;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::Row;
use dbzilla_core::users::{self, UserModel};
use dbzilla_drivers::{driver_for, Conn};
use eframe::egui::{self, Align2, RichText, Ui, Vec2};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Work for the background connection.
enum Cmd {
    Load,
    Grants { key: String, sql: String },
    Apply(Vec<String>),
}

/// Answers of the background connection.
enum Msg {
    Users { users: Vec<UserModel>, scopes: Vec<String> },
    Grants { key: String, rows: Vec<Row> },
    Applied(Result<(), String>),
    Error(String),
}

/// Requests of the sheet toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum UsersAction {
    /// Statements to run, passwords masked; the workspace asks the safe mode, then calls `confirm_apply`.
    Apply(Vec<String>),
    /// Close the sheet.
    Dismiss,
}

/// What an applied change did, for the history and the toasts.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The statements, passwords masked.
    pub statements: Vec<String>,
    pub error: Option<String>,
}

/// Tabs of the right side.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tab {
    General,
    Global,
    Scoped,
    Resources,
}

/// One user of the list with its edits.
struct Entry {
    /// As read from the server; `None` for a user created in the sheet.
    original: Option<UserModel>,
    edited: UserModel,
    deleted: bool,
    /// The grants of the server user have been read.
    loaded: bool,
}

/// State of the sheet and handle of its connection. Dropping it closes the connection.
pub struct UserManager {
    driver: DriverId,
    tx: mpsc::UnboundedSender<Cmd>,
    rx: std_mpsc::Receiver<Msg>,
    entries: Vec<Entry>,
    /// Tables (PostgreSQL) or databases (MySQL) privileges can be given on.
    scopes: Vec<String>,
    selected: Option<usize>,
    filter: String,
    tab: Tab,
    reveal: bool,
    scope_filter: String,
    loading: bool,
    applying: bool,
    error: Option<String>,
    /// Statements held while the safe mode decides.
    pending: Option<(Vec<String>, Vec<String>)>,
    /// Result of the last apply, handed to the workspace once.
    applied: Option<Applied>,
}

impl UserManager {
    /// Opens the sheet and its connection, and asks for the users.
    pub fn open(env: &JobEnv, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let driver = env.cfg.driver;
        let (tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (msg_tx, rx) = std_mpsc::channel::<Msg>();
        let cfg = env.cfg.clone();
        env.rt.spawn(async move {
            let mut conn: Option<Box<dyn Conn>> = None;
            while let Some(cmd) = cmd_rx.recv().await {
                let msg = work(&mut conn, &cfg, driver, cmd).await;
                if msg_tx.send(msg).is_err() {
                    break;
                }
                repaint();
            }
        });
        let me = Self { driver, tx, rx, entries: Vec::new(), scopes: Vec::new(), selected: None, filter: String::new(), tab: Tab::General, reveal: false, scope_filter: String::new(), loading: true, applying: false, error: None, pending: None, applied: None };
        let _ = me.tx.send(Cmd::Load);
        me
    }

    /// Takes in what the connection answered.
    pub fn poll(&mut self) -> Option<Applied> {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Users { users, scopes } => {
                    self.loading = false;
                    self.error = None;
                    self.scopes = scopes;
                    self.entries = users.into_iter().map(|u| Entry { original: Some(u.clone()), edited: u, deleted: false, loaded: false }).collect();
                    self.selected = None;
                }
                Msg::Grants { key, rows } => {
                    if let Some(e) = self.entries.iter_mut().find(|e| e.original.as_ref().is_some_and(|o| o.key() == key)) {
                        let (mut orig, mut edit) = (e.original.take().unwrap_or_else(|| e.edited.clone()), e.edited.clone());
                        // The edits made before the grants arrived stay: only the base is filled in.
                        let before = orig.clone();
                        if self.driver == DriverId::Postgres {
                            users::apply_pg_grants(&mut orig, &rows);
                        } else {
                            let lines: Vec<String> = rows.iter().filter_map(|r| r.first().map(|v| v.to_display())).collect();
                            users::apply_mysql_grants(&mut orig, &lines);
                        }
                        if edit == before {
                            edit = orig.clone();
                        } else {
                            edit.scoped = orig.scoped.clone();
                            edit.globals = orig.globals.clone();
                        }
                        e.original = Some(orig);
                        e.edited = edit;
                        e.loaded = true;
                    }
                }
                Msg::Applied(res) => {
                    self.applying = false;
                    let statements = self.pending.take().map(|(masked, _)| masked).unwrap_or_default();
                    self.error = res.clone().err();
                    self.applied = Some(Applied { statements, error: res.err() });
                    self.reload();
                }
                Msg::Error(e) => {
                    self.loading = false;
                    self.applying = false;
                    self.error = Some(e);
                }
            }
        }
        self.applied.take()
    }

    /// Reads the users again, dropping the edits.
    pub fn reload(&mut self) {
        self.loading = true;
        let _ = self.tx.send(Cmd::Load);
    }

    /// Statements of every change, in the order of the list, with the real passwords.
    fn statements(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.entries {
            match (&e.original, e.deleted) {
                (Some(o), true) => out.extend(users::drop_statements(o)),
                (None, true) => {}
                (_, false) if e.edited.name.trim().is_empty() => {}
                (o, false) => out.extend(users::diff(o.as_ref(), &e.edited)),
            }
        }
        out
    }

    /// The statements as the user may see them.
    fn preview(&self) -> Vec<String> {
        let edited: Vec<UserModel> = self.entries.iter().map(|e| e.edited.clone()).collect();
        users::mask_passwords(&self.statements(), &edited)
    }

    /// Holds the statements until the safe mode agrees.
    fn request_apply(&mut self) -> Option<UsersAction> {
        let real = self.statements();
        if real.is_empty() {
            return None;
        }
        let masked = self.preview();
        self.pending = Some((masked.clone(), real));
        Some(UsersAction::Apply(masked))
    }

    /// Runs the statements the safe mode agreed to.
    pub fn confirm_apply(&mut self) {
        if let Some((_, real)) = self.pending.as_ref() {
            self.applying = true;
            let _ = self.tx.send(Cmd::Apply(real.clone()));
        }
    }

    /// Asks for the grants of the selected user when they are not known yet.
    fn load_grants(&mut self, i: usize) {
        let e = &self.entries[i];
        let (Some(o), false) = (&e.original, e.loaded) else { return };
        let sql = if self.driver == DriverId::Postgres { users::pg_grants_sql(&o.name) } else { users::mysql_grants_sql(o) };
        self.entries[i].loaded = true;
        let key = self.entries[i].edited.key();
        let _ = self.tx.send(Cmd::Grants { key, sql });
    }

    /// Adds a user to create.
    fn new_user(&mut self) {
        let mut u = UserModel::new(self.driver, "");
        u.password = Some(String::new());
        self.entries.push(Entry { original: None, edited: u, deleted: false, loaded: true });
        self.selected = Some(self.entries.len() - 1);
        self.tab = Tab::General;
    }
}

/// Runs one command on the background connection, opening it when needed.
async fn work(conn: &mut Option<Box<dyn Conn>>, cfg: &dbzilla_drivers::ConnectConfig, driver: DriverId, cmd: Cmd) -> Msg {
    if conn.is_none() {
        let opened = match driver_for(cfg.driver) {
            Ok(d) => d.connect(cfg).await,
            Err(e) => Err(e),
        };
        match opened {
            Ok(c) => *conn = Some(c),
            Err(e) => return Msg::Error(e.to_string()),
        }
    }
    let Some(c) = conn.as_mut() else { return Msg::Error("no connection".into()) };
    let c = c.as_mut();
    match cmd {
        Cmd::Load => {
            let Some(sql) = users::list_sql(driver) else { return Msg::Error("user management is not available for this database".into()) };
            let rows = match run(c, sql).await {
                Ok((_, rows)) => rows,
                Err(e) => return Msg::Error(e),
            };
            let scope_sql = if driver == DriverId::Postgres { users::PG_TABLES_SQL } else { "SHOW DATABASES" };
            let scopes = match run(c, scope_sql).await {
                Ok((_, r)) if driver == DriverId::Postgres => r.iter().map(|r| format!("{}.{}", r[0].to_display(), r[1].to_display())).collect(),
                Ok((_, r)) => r.iter().filter_map(|r| r.first().map(|v| v.to_display())).filter(|d| d != "information_schema" && d != "performance_schema").collect(),
                Err(e) => return Msg::Error(e),
            };
            Msg::Users { users: users::parse_list(driver, &rows), scopes }
        }
        Cmd::Grants { key, sql } => match run(c, &sql).await {
            Ok((_, rows)) => Msg::Grants { key, rows },
            Err(e) => Msg::Error(e),
        },
        Cmd::Apply(statements) => Msg::Applied(c.execute_batch(&statements, false).await.map(|_| ()).map_err(|e| e.to_string())),
    }
}

/// Draws the sheet and returns what it asks of the workspace.
pub fn show(ctx: &egui::Context, m: &mut UserManager) -> Option<UsersAction> {
    let mut action = None;
    let mut open = true;
    egui::Window::new("Users").id(egui::Id::new("user-manager")).open(&mut open).default_size(Vec2::new(900.0, 560.0)).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        ui.horizontal_top(|ui| {
            ui.allocate_ui(Vec2::new(210.0, 380.0), |ui| list(ui, m));
            ui.separator();
            ui.vertical(|ui| detail(ui, m));
        });
        ui.separator();
        preview(ui, m, &mut action);
    });
    if !open {
        action = Some(UsersAction::Dismiss);
    }
    action
}

/// Search box, users and the New user button.
fn list(ui: &mut Ui, m: &mut UserManager) {
    widgets::search_box(ui, &mut m.filter, "Search users");
    if widgets::secondary_button(ui, "New user").clicked() {
        m.new_user();
    }
    let needle = m.filter.to_lowercase();
    let mut remove = None;
    egui::ScrollArea::vertical().max_height(340.0).id_salt("user-list").show(ui, |ui| {
        if m.loading {
            ui.spinner();
        }
        for i in 0..m.entries.len() {
            let e = &m.entries[i];
            if !needle.is_empty() && !e.edited.key().to_lowercase().contains(&needle) {
                continue;
            }
            let changed = e.original.as_ref().is_none_or(|o| *o != e.edited);
            let mut text = RichText::new(format!("{}{}", if changed { "* " } else { "" }, if e.edited.name.is_empty() { "(new user)".to_owned() } else { e.edited.key() }));
            if e.deleted {
                text = text.strikethrough().color(theme::ERROR);
            }
            let r = ui.selectable_label(m.selected == Some(i), text);
            if r.clicked() {
                m.selected = Some(i);
                m.load_grants(i);
            }
            r.context_menu(|ui| {
                let label = if m.entries[i].deleted { "Keep user" } else { "Delete user" };
                if ui.button(label).clicked() {
                    if m.entries[i].original.is_none() {
                        remove = Some(i);
                    } else {
                        m.entries[i].deleted = !m.entries[i].deleted;
                        m.load_grants(i);
                    }
                    ui.close_menu();
                }
            });
        }
    });
    if let Some(i) = remove {
        m.entries.remove(i);
        m.selected = None;
    }
}

/// The tabs of the selected user.
fn detail(ui: &mut Ui, m: &mut UserManager) {
    let Some(i) = m.selected.filter(|i| *i < m.entries.len()) else {
        ui.label(RichText::new("Select a user, or create one").color(theme::MUTED));
        return;
    };
    ui.horizontal(|ui| {
        ui.selectable_value(&mut m.tab, Tab::General, "General");
        ui.selectable_value(&mut m.tab, Tab::Global, "Global privileges");
        if m.driver != DriverId::Postgres || !m.scopes.is_empty() {
            ui.selectable_value(&mut m.tab, Tab::Scoped, if m.driver == DriverId::Postgres { "Table privileges" } else { "Database privileges" });
        }
        if m.driver == DriverId::MySql {
            ui.selectable_value(&mut m.tab, Tab::Resources, "Resources");
        }
    });
    ui.add_space(6.0);
    if m.entries[i].deleted {
        ui.label(RichText::new("This user will be deleted when you apply").color(theme::ERROR));
        return;
    }
    let (tab, driver, reveal) = (m.tab, m.driver, &mut m.reveal);
    let scopes = m.scopes.clone();
    let scope_filter = &mut m.scope_filter;
    let e = &mut m.entries[i];
    let is_new = e.original.is_none();
    egui::ScrollArea::vertical().max_height(340.0).id_salt("user-detail").show(ui, |ui| match tab {
        Tab::General => {
            widgets::field_label(ui, "Username");
            ui.add_enabled_ui(is_new, |ui| widgets::input(ui, &mut e.edited.name, "username"));
            if driver == DriverId::MySql {
                widgets::field_label(ui, "Host");
                ui.add_enabled_ui(is_new, |ui| widgets::input(ui, &mut e.edited.host, "localhost or %"));
            }
            widgets::field_label(ui, if is_new { "Password" } else { "New password (empty keeps the current one)" });
            let mut pw = e.edited.password.clone().unwrap_or_default();
            widgets::secret_input(ui, &mut pw, reveal);
            e.edited.password = if pw.is_empty() && !is_new { None } else { Some(pw) };
        }
        Tab::Global => global_tab(ui, e, driver),
        Tab::Scoped => scoped_tab(ui, e, driver, &scopes, scope_filter),
        Tab::Resources => {
            for (label, v) in [("Max updates per hour", &mut e.edited.limits.max_updates), ("Max connections per hour", &mut e.edited.limits.max_connections), ("Max questions per hour", &mut e.edited.limits.max_questions)] {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(v).range(0..=u32::MAX));
                    ui.label(label);
                });
            }
            ui.label(RichText::new("0 means no limit").color(theme::MUTED));
        }
    });
}

/// Role attributes (PostgreSQL) or global privileges (MySQL and MariaDB).
fn global_tab(ui: &mut Ui, e: &mut Entry, driver: DriverId) {
    if driver == DriverId::Postgres {
        for (key, label) in users::PG_ATTRIBUTES {
            toggle(ui, &mut e.edited.globals, key, label);
        }
        return;
    }
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, "Check all").clicked() {
            e.edited.globals = users::mysql_global_privileges().into_iter().map(str::to_owned).collect();
        }
        if widgets::secondary_button(ui, "Uncheck all").clicked() {
            e.edited.globals.clear();
        }
    });
    if users::mysql_global_privileges().iter().all(|p| e.edited.globals.contains(*p)) {
        ui.label(RichText::new("Every global privilege is granted: this account is almost root").color(theme::TERTIARY));
    }
    for (group, privs) in users::MYSQL_GLOBAL_GROUPS {
        widgets::section_title(ui, group);
        for p in privs {
            toggle(ui, &mut e.edited.globals, p, p);
        }
    }
}

/// Adds or removes a keyword of a set with a checkbox.
fn toggle(ui: &mut Ui, set: &mut std::collections::BTreeSet<String>, key: &str, label: &str) {
    let mut on = set.contains(key);
    if ui.checkbox(&mut on, label).changed() {
        if on {
            set.insert(key.to_owned());
        } else {
            set.remove(key);
        }
    }
}

/// Available scopes on one side, granted ones with their privileges on the other.
fn scoped_tab(ui: &mut Ui, e: &mut Entry, driver: DriverId, scopes: &[String], filter: &mut String) {
    let privileges: Vec<&str> = if driver == DriverId::Postgres { users::PG_TABLE_PRIVILEGES.to_vec() } else { users::mysql_database_privileges() };
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        widgets::field_label(ui, "Available");
        widgets::search_box(ui, filter, "Filter");
        let needle = filter.to_lowercase();
        egui::ScrollArea::vertical().max_height(280.0).id_salt("scopes-available").show(ui, |ui| {
            let available: Vec<&String> = scopes.iter().filter(|s| !e.edited.scoped.contains_key(*s) && s.to_lowercase().contains(&needle)).collect();
            for s in available {
                if ui.selectable_label(false, s).on_hover_text("Click to grant").clicked() {
                    e.edited.scoped.insert(s.clone(), std::iter::once("SELECT".to_owned()).collect());
                }
            }
        });
        let ui = &mut cols[1];
        widgets::field_label(ui, "Granted");
        let mut remove = None;
        egui::ScrollArea::vertical().max_height(280.0).id_salt("scopes-granted").show(ui, |ui| {
            for (scope, set) in e.edited.scoped.iter_mut() {
                egui::CollapsingHeader::new(scope.as_str()).id_salt(("granted", scope.as_str())).show(ui, |ui| {
                    for p in &privileges {
                        toggle(ui, set, p, p);
                    }
                    if widgets::secondary_button(ui, "Revoke all").clicked() {
                        remove = Some(scope.clone());
                    }
                });
            }
        });
        if let Some(s) = remove {
            e.edited.scoped.remove(&s);
        }
    });
}

/// The SQL that Apply would run, with the Apply and Dismiss buttons.
fn preview(ui: &mut Ui, m: &mut UserManager, action: &mut Option<UsersAction>) {
    widgets::field_label(ui, "SQL preview");
    let lines = m.preview();
    egui::ScrollArea::vertical().max_height(110.0).id_salt("user-preview").show(ui, |ui| {
        if lines.is_empty() {
            ui.label(RichText::new("No changes").color(theme::MUTED));
        }
        for l in &lines {
            ui.label(RichText::new(l).monospace().size(12.0));
        }
    });
    if let Some(e) = &m.error {
        ui.label(RichText::new(e).color(theme::ERROR));
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(!lines.is_empty() && !m.applying, egui::Button::new("Apply")).clicked() {
            *action = m.request_apply();
        }
        if widgets::secondary_button(ui, "Reload").on_hover_text("Read the users again and drop the edits").clicked() {
            m.reload();
        }
        if m.applying || m.loading {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, "Dismiss").clicked() {
                *action = Some(UsersAction::Dismiss);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager over a runtime and a driver that cannot list users: enough to test the editing flow.
    fn manager(driver: DriverId) -> (UserManager, tokio::runtime::Runtime) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cfg = dbzilla_drivers::ConnectConfig { driver, host: String::new(), port: None, user: String::new(), password: None, database: String::new(), ssl: Default::default(), bootstrap: Vec::new() };
        let env = JobEnv { rt: rt.handle().clone(), cfg, names: dbzilla_io::naming::NameContext::now("c", "d", "") };
        (UserManager::open(&env, Arc::new(|| {})), rt)
    }

    #[test]
    fn new_user_is_previewed_with_the_password_masked_and_held_for_the_safe_mode() {
        let (mut m, _rt) = manager(DriverId::Postgres);
        assert!(m.request_apply().is_none(), "nothing to apply yet");
        m.new_user();
        m.entries[0].edited.name = "app".into();
        m.entries[0].edited.password = Some("hunter2".into());
        let Some(UsersAction::Apply(lines)) = m.request_apply() else { panic!("changes are held for the safe mode") };
        assert_eq!(lines, vec![format!("CREATE ROLE \"app\" WITH LOGIN PASSWORD {};", users::MASK)]);
        assert!(m.statements()[0].contains("hunter2"), "the real statement keeps the password");
    }

    #[test]
    fn deleting_a_new_user_drops_it_from_the_list_without_statements() {
        let (mut m, _rt) = manager(DriverId::MySql);
        m.new_user();
        assert!(m.statements().is_empty(), "a user without a name creates nothing");
        m.entries[0].deleted = true;
        assert!(m.statements().is_empty());
    }
}
