//! Top level application state and frame loop.

use crate::cli::{CliArgs, OpenTarget};
use crate::glyphs::Ico;
use crate::keymap::{Action, KeyMap};
use crate::mcpbackend::{McpHost, PasswordResolver};
use crate::theme;
use crate::ui::connform::{self, config_for, ConnForm, FormAction, TestState};
use crate::ui::connlist::{self, ListAction, ListState};
use crate::ui::settings::{self, SettingsTab};
use crate::ui::workspace::{Stores, Workspace, WsEnv, WsOutput};
use crate::ui::{widgets, Toasts};
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::url::{format_url, parse_url};
use dbzilla_drivers::driver_for;
use dbzilla_session::{default_factory, spawn, Command, SessionConfig};
use dbzilla_store::connections::{ConnectionStore, PasswordMode, StoredConnection};
use dbzilla_store::history::HistoryStore;
use dbzilla_store::paths::AppPaths;
use dbzilla_store::queries::QueryStore;
use dbzilla_store::secrets::{secret_key, KeyringStore, MemorySecretStore, SecretStore};
use dbzilla_store::settings::Settings;
use dbzilla_store::tags::TagStore;
use eframe::egui::{self, Align, Frame, Layout, Margin, Stroke};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

/// Maximum width of the detail column.
const DETAIL_MAX_WIDTH: f32 = 820.0;
/// Width of the navigation rail.
const RAIL_WIDTH: f32 = 64.0;
/// Key used to probe the OS keychain.
const PROBE_KEY: &str = "dbzilla/probe/test";

/// Which area is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Connections,
    Workspace(usize),
    Settings,
}

/// A destructive action waiting for confirmation.
pub enum ConfirmAction {
    DeleteConnection(String),
    DeleteGroup(String),
    CloseWorkspace(usize),
}

/// Confirmation dialog content.
pub struct Confirm {
    pub title: String,
    pub body: String,
    pub button: String,
    pub action: ConfirmAction,
}

/// Dialog asking for a password at connect time.
struct PasswordPrompt {
    conn: StoredConnection,
    value: String,
    reveal: bool,
}

/// A password command running in the background.
struct PendingCommand {
    conn: StoredConnection,
    rx: Receiver<Result<String, String>>,
}

/// Root application object.
pub struct App {
    paths: AppPaths,
    settings: Settings,
    store: ConnectionStore,
    tags: TagStore,
    secrets: Arc<dyn SecretStore>,
    keymap: KeyMap,
    rt: tokio::runtime::Runtime,
    toasts: Toasts,
    confirm: Option<Confirm>,
    section: Section,
    list: ListState,
    form: Option<Box<ConnForm>>,
    settings_tab: SettingsTab,
    workspaces: Vec<Workspace>,
    import_url: Option<String>,
    new_group: Option<String>,
    password_prompt: Option<PasswordPrompt>,
    pending_commands: Vec<PendingCommand>,
    mcp: McpHost,
    mcp_synced: Option<std::time::Instant>,
}

/// Chooses the OS keychain when it works, otherwise an in-memory store.
fn pick_secret_store(toasts: &mut Toasts) -> Arc<dyn SecretStore> {
    let k = KeyringStore;
    let usable = k.set(PROBE_KEY, "probe").is_ok() && k.get(PROBE_KEY).ok().flatten().as_deref() == Some("probe");
    let _ = k.delete(PROBE_KEY);
    if usable {
        return Arc::new(KeyringStore);
    }
    toasts.info("The OS keychain is not available: passwords are kept in memory for this session only");
    Arc::new(MemorySecretStore::default())
}

impl App {
    /// Creates the app: loads the stored data and installs the Obsidian theme exactly once.
    pub fn new(cc: &eframe::CreationContext<'_>, cli: CliArgs) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut toasts = Toasts::default();
        let paths = cli.data_dir.clone().map(AppPaths::new).or_else(AppPaths::platform_default).unwrap_or_else(|| AppPaths::new(std::env::temp_dir().join("dbzilla")));
        let settings = Settings::load(&paths);
        let store = ConnectionStore::load(&paths);
        let tags = TagStore::load(&paths);
        for w in [&settings.warning, &store.warning, &tags.warning].into_iter().flatten() {
            toasts.err(w.clone());
        }
        for w in &cli.warnings {
            toasts.err(w.clone());
        }
        let secrets = pick_secret_store(&mut toasts);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(3).enable_all().build().expect("cannot start the async runtime");
        let resolver: PasswordResolver = {
            let secrets = secrets.clone();
            Arc::new(move |c: &StoredConnection| match &c.password_mode {
                PasswordMode::NoPassword => Ok(None),
                PasswordMode::Keychain => secrets.get(&secret_key(&c.id, "password")).map_err(|e| format!("cannot read the password: {e}")),
                PasswordMode::AskEveryTime => Err("this connection asks for its password every time, so it cannot be opened through MCP".to_owned()),
                PasswordMode::Command(cmd) => run_password_command(cmd).map(Some),
            })
        };
        let mcp = McpHost::new(paths.mcp_tokens_file(), paths.mcp_activity_file(), resolver);
        let mut app = Self {
            paths,
            settings: settings.value,
            store: store.value,
            tags: tags.value,
            secrets,
            keymap: KeyMap::default(),
            rt,
            toasts,
            confirm: None,
            section: Section::Connections,
            list: ListState::default(),
            form: None,
            settings_tab: SettingsTab::General,
            workspaces: Vec::new(),
            import_url: None,
            new_group: None,
            password_prompt: None,
            pending_commands: Vec::new(),
            mcp,
            mcp_synced: None,
        };
        if let Some(t) = cli.open {
            app.open_target(&cc.egui_ctx, t);
        }
        app
    }

    /// Saves the connection store, reporting failures.
    fn save_connections(&mut self) {
        if let Err(e) = self.store.save(&self.paths) {
            self.toasts.err(format!("Cannot save connections: {e}"));
        }
    }

    /// Saves the settings, reporting failures.
    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.paths) {
            self.toasts.err(format!("Cannot save settings: {e}"));
        }
    }

    /// Opens a connection given on the command line without saving it.
    fn open_target(&mut self, ctx: &egui::Context, t: OpenTarget) {
        match t {
            OpenTarget::SqliteFile(p) => {
                let mut c = StoredConnection::new(&p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "SQLite".into()), DriverId::Sqlite);
                c.database = p.to_string_lossy().into_owned();
                c.password_mode = PasswordMode::NoPassword;
                self.open_workspace(ctx, c, None);
            }
            OpenTarget::Url(spec) => {
                let c = StoredConnection::from_spec(&spec);
                self.open_workspace(ctx, c, spec.password.clone());
            }
        }
    }

    /// Starts connecting a saved connection, resolving its password first.
    fn connect(&mut self, ctx: &egui::Context, id: &str) {
        let Some(c) = self.store.get(id).cloned() else { return };
        match c.password_mode.clone() {
            PasswordMode::NoPassword => self.open_workspace(ctx, c, None),
            PasswordMode::Keychain => {
                // the OS keychain can block (unlock prompt, D-Bus), so it is read off the UI thread
                let (tx, rx) = channel();
                let repaint = ctx.clone();
                let secrets = self.secrets.clone();
                let key = secret_key(&c.id, "password");
                std::thread::spawn(move || {
                    let _ = tx.send(secrets.get(&key).map(Option::unwrap_or_default).map_err(|e| format!("cannot read the password from the keychain: {e}")));
                    repaint.request_repaint();
                });
                self.pending_commands.push(PendingCommand { conn: c, rx });
            }
            PasswordMode::AskEveryTime => self.password_prompt = Some(PasswordPrompt { conn: c, value: String::new(), reveal: false }),
            PasswordMode::Command(cmd) => {
                let (tx, rx) = channel();
                let repaint = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(run_password_command(&cmd));
                    repaint.request_repaint();
                });
                self.pending_commands.push(PendingCommand { conn: c, rx });
                self.toasts.info("Running the password command...");
            }
        }
    }

    /// Creates the session for a connection and shows its workspace.
    fn open_workspace(&mut self, ctx: &egui::Context, conn: StoredConnection, password: Option<String>) {
        let caps = Capabilities::for_driver(conn.driver);
        let cfg = SessionConfig {
            query_timeout: caps.query_timeout_supported.then(|| Duration::from_secs(self.settings.query_timeout_secs)),
            keep_alive: self.settings.keep_alive.then(|| Duration::from_secs(self.settings.keep_alive_secs)),
        };
        let repaint_ctx = ctx.clone();
        let session = spawn(self.rt.handle(), cfg, default_factory(), Arc::new(move || repaint_ctx.request_repaint()));
        session.send(Command::Connect(config_for(&conn, password.clone())));
        let job_env = crate::ui::jobs::JobEnv { rt: self.rt.handle().clone(), cfg: config_for(&conn, password.clone()), names: dbzilla_io::naming::NameContext::now(&conn.name, &conn.database, "") };
        let repaint_for_jobs = ctx.clone();
        self.workspaces.push(Workspace::new(conn, session, self.settings.console_max_entries, self.settings.page_size, password, Stores { history: HistoryStore::new(self.paths.history_dir(), self.settings.history_enabled), queries: QueryStore::new(self.paths.queries_dir()) }));
        if let Some(ws) = self.workspaces.last_mut() {
            ws.set_job_env(job_env, Arc::new(move || repaint_for_jobs.request_repaint()));
        }
        self.section = Section::Workspace(self.workspaces.len() - 1);
    }

    /// Handles results of background work.
    fn poll(&mut self, ctx: &egui::Context) {
        let repaint = ctx.clone();
        self.mcp.sync(self.rt.handle(), &self.settings.mcp, Arc::new(move || repaint.request_repaint()));
        self.mcp.poll();
        if self.mcp.running() && self.mcp_synced.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            if let Ok(mut c) = self.mcp.shared.connections.lock() {
                *c = self.store.list().to_vec();
            }
            self.mcp_synced = Some(std::time::Instant::now());
        }
        for ws in &mut self.workspaces {
            ws.poll();
        }
        if let Some(form) = self.form.as_mut() {
            if let Some(TestState::Running(rx)) = &form.test {
                if let Ok(r) = rx.try_recv() {
                    form.test = Some(TestState::Done(r));
                }
            }
        }
        let mut ready = Vec::new();
        self.pending_commands.retain(|p| match p.rx.try_recv() {
            Ok(r) => {
                ready.push((p.conn.clone(), r));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (conn, r) in ready {
            match r {
                Ok(pw) => self.open_workspace(ctx, conn, Some(pw).filter(|p| !p.is_empty())),
                Err(e) => self.toasts.err(format!("Cannot get the password: {e}")),
            }
        }
    }

    /// Handles global shortcuts.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() && !ctx.input(|i| i.modifiers.command) {
            return;
        }
        for a in self.keymap.triggered(ctx) {
            match (a, self.section) {
                (Action::Settings, _) => self.section = Section::Settings,
                (Action::OpenConnection, _) => self.section = Section::Connections,
                (a, Section::Workspace(i)) => {
                    if let Some(WsOutput::Run(_)) = self.workspaces.get_mut(i).and_then(|ws| ws.handle_action(a)) {
                        // actions the workspace does not implement yet
                    }
                }
                _ => {}
            }
        }
    }

    /// The navigation rail.
    fn rail(&mut self, ctx: &egui::Context) {
        let frame = Frame::new().fill(theme::NAVY_DARK).inner_margin(Margin::symmetric(8, 10));
        egui::SidePanel::left("rail").exact_width(RAIL_WIDTH).resizable(false).frame(frame).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 8.0, theme::PRIMARY);
                crate::glyphs::paint(ui.painter(), rect, Ico::Database, egui::Color32::WHITE);
                ui.add_space(14.0);
                if widgets::rail_tile(ui, Ico::Plug, self.section == Section::Connections, "Connections").clicked() {
                    self.section = Section::Connections;
                }
                for i in 0..self.workspaces.len() {
                    let ws = &self.workspaces[i];
                    let tip = format!("{} ({})", ws.conn.name, ws.conn.driver_name());
                    if widgets::rail_tile(ui, Ico::Database, self.section == Section::Workspace(i), &tip).clicked() {
                        self.section = Section::Workspace(i);
                    }
                }
            });
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if widgets::rail_tile(ui, Ico::Settings, self.section == Section::Settings, "Settings").clicked() {
                    self.section = Section::Settings;
                }
            });
        });
    }

    /// Applies an action from the connection list.
    fn list_action(&mut self, ctx: &egui::Context, a: ListAction) {
        match a {
            ListAction::NewConnection(d) => self.form = Some(Box::new(ConnForm::new(d, self.settings.default_safe_level))),
            ListAction::NewGroup => self.new_group = Some(String::new()),
            ListAction::ImportUrl => self.import_url = Some(String::new()),
            ListAction::Select(id) => {
                self.edit_connection(&id);
            }
            ListAction::Edit(id) => self.edit_connection(&id),
            ListAction::Connect(id) => self.connect(ctx, &id),
            ListAction::Duplicate(id) => {
                if let Some(new_id) = self.store.duplicate(&id) {
                    if let Ok(Some(pw)) = self.secrets.get(&secret_key(&id, "password")) {
                        let _ = self.secrets.set(&secret_key(&new_id, "password"), &pw);
                    }
                    self.save_connections();
                    self.toasts.ok("Connection duplicated");
                }
            }
            ListAction::CopyUrl(id) => {
                if let Some(c) = self.store.get(&id) {
                    let pw = self.secrets.get(&secret_key(&c.id, "password")).ok().flatten();
                    ctx.copy_text(format_url(&c.to_spec(pw), true));
                    self.toasts.ok("Connection URL copied");
                }
            }
            ListAction::Delete(id) => {
                let name = self.store.get(&id).map(|c| c.name.clone()).unwrap_or_default();
                self.confirm = Some(Confirm { title: "Delete connection".into(), body: format!("Delete the connection '{name}'? This cannot be undone."), button: "Delete".into(), action: ConfirmAction::DeleteConnection(id) });
            }
            ListAction::ToggleGroup(id) => {
                if !self.list.collapsed.remove(&id) {
                    self.list.collapsed.insert(id);
                }
            }
            ListAction::DeleteGroup(id) => {
                self.confirm = Some(Confirm { title: "Delete group".into(), body: "Deleting a group deletes all the connections inside it.".into(), button: "Delete".into(), action: ConfirmAction::DeleteGroup(id) });
            }
        }
    }

    /// Opens the form for a saved connection.
    fn edit_connection(&mut self, id: &str) {
        let Some(c) = self.store.get(id).cloned() else { return };
        let pw = if c.password_mode == PasswordMode::Keychain { self.secrets.get(&secret_key(&c.id, "password")).ok().flatten() } else { None };
        self.list.selected = Some(c.id.clone());
        self.form = Some(Box::new(ConnForm::from_stored(&c, pw)));
    }

    /// Saves the form; returns the saved id.
    fn save_form(&mut self) -> Option<String> {
        let form = self.form.as_mut()?;
        let stored = match form.to_stored() {
            Ok(c) => c,
            Err(e) => {
                form.error = Some(e);
                return None;
            }
        };
        form.error = None;
        form.id = Some(stored.id.clone());
        let key = secret_key(&stored.id, "password");
        let result = if stored.password_mode == PasswordMode::Keychain && !form.password.is_empty() { self.secrets.set(&key, &form.password) } else { self.secrets.delete(&key) };
        form.mark_saved();
        let id = stored.id.clone();
        self.store.upsert(stored);
        self.list.selected = Some(id.clone());
        if let Err(e) = result {
            self.toasts.err(format!("Cannot store the password: {e}"));
        }
        self.save_connections();
        self.toasts.ok("Connection saved");
        Some(id)
    }

    /// Starts a connection test with the values typed in the form.
    fn test_form(&mut self, ctx: &egui::Context) {
        let Some(form) = self.form.as_mut() else { return };
        let cfg = match form.to_connect_config() {
            Ok(c) => c,
            Err(e) => {
                form.error = Some(e);
                return;
            }
        };
        form.error = None;
        let (tx, rx) = channel();
        let repaint = ctx.clone();
        self.rt.spawn(async move {
            let result = async {
                let driver = driver_for(cfg.driver).map_err(|e| e.to_string())?;
                let mut conn = driver.connect(&cfg).await.map_err(|e| e.to_string())?;
                conn.server_version().await.map_err(|e| e.to_string())
            }
            .await;
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        form.test = Some(TestState::Running(rx));
    }

    /// Applies the confirmed action.
    fn run_confirmed(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteConnection(id) => {
                self.store.delete(&id);
                let _ = self.secrets.delete(&secret_key(&id, "password"));
                if self.form.as_ref().is_some_and(|f| f.id.as_deref() == Some(id.as_str())) {
                    self.form = None;
                }
                self.save_connections();
                self.toasts.ok("Connection deleted");
            }
            ConfirmAction::DeleteGroup(id) => {
                for removed in self.store.delete_group(&id) {
                    let _ = self.secrets.delete(&secret_key(&removed, "password"));
                }
                self.save_connections();
                self.toasts.ok("Group deleted");
            }
            ConfirmAction::CloseWorkspace(i) => {
                if i < self.workspaces.len() {
                    self.workspaces.remove(i);
                    self.section = Section::Connections;
                }
            }
        }
    }

    /// The connections list pane and its detail.
    fn connections_section(&mut self, ctx: &egui::Context) {
        let list_frame = Frame::new().fill(theme::DEEP_DARK).inner_margin(Margin::same(12));
        let mut list_action = None;
        egui::SidePanel::left("list").default_width(340.0).width_range(280.0..=520.0).resizable(true).frame(list_frame).show(ctx, |ui| {
            list_action = connlist::show(ui, &mut self.list, &self.store, &self.tags);
        });
        if let Some(a) = list_action {
            self.list_action(ctx, a);
        }
        let detail_frame = Frame::new().fill(theme::DEEP_DARK).inner_margin(Margin::symmetric(28, 22));
        let mut form_action = None;
        egui::CentralPanel::default().frame(detail_frame).show(ctx, |ui| {
            let width = ui.available_width().min(DETAIL_MAX_WIDTH);
            let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(width, ui.max_rect().height()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| match self.form.as_mut() {
                Some(form) => {
                    let busy = matches!(form.test, Some(TestState::Running(_)));
                    form_action = connform::show(ui, form, &self.tags.all(), busy);
                }
                None => self.welcome(ui),
            });
        });
        match form_action {
            Some(FormAction::Save) => {
                self.save_form();
            }
            Some(FormAction::Connect) => {
                if let Some(id) = self.save_form() {
                    self.connect(ctx, &id);
                }
            }
            Some(FormAction::Test) => self.test_form(ctx),
            Some(FormAction::Cancel) => self.form = None,
            Some(FormAction::Delete) => {
                if let Some(id) = self.form.as_ref().and_then(|f| f.id.clone()) {
                    self.list_action(ctx, ListAction::Delete(id));
                }
            }
            None => {}
        }
    }

    /// Empty detail pane.
    fn welcome(&self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 14.0, theme::INPUT_FILL_BLUE);
            ui.painter().rect_stroke(rect, 14.0, Stroke::new(1.0_f32, theme::PRIMARY.gamma_multiply(0.5)), egui::StrokeKind::Inside);
            crate::glyphs::paint(ui.painter(), rect, Ico::Database, theme::SECONDARY);
            ui.add_space(10.0);
            widgets::heading(ui, "DBZilla");
            widgets::muted(ui, "Select a connection to edit it, double-click to open it, or press New.");
            let n = self.store.list().len();
            widgets::muted(ui, &format!("{n} saved {}  ·  {} open", if n == 1 { "connection" } else { "connections" }, self.workspaces.len()));
        });
    }

    /// The workspace screen of an open connection.
    fn workspace_section(&mut self, ctx: &egui::Context, i: usize) {
        let ctx = ctx.clone();
        let ctx = &ctx;
        let Some(ws) = self.workspaces.get_mut(i) else { return };
        let mut env = WsEnv { settings: &self.settings, keymap: &self.keymap, tags: &self.tags, toasts: &mut self.toasts };
        let outputs = ws.show(ctx, &mut env);
        for o in outputs {
            match o {
                WsOutput::Close => {
                    let name = self.workspaces[i].conn.name.clone();
                    self.confirm = Some(Confirm { title: "Close workspace".into(), body: format!("Close the connection '{name}'?"), button: "Close".into(), action: ConfirmAction::CloseWorkspace(i) });
                }
                WsOutput::Run(Action::Settings) => self.section = Section::Settings,
                WsOutput::Run(Action::OpenConnection) => self.section = Section::Connections,
                WsOutput::Run(_) => {}
                WsOutput::OpenDatabase(db) => {
                    let ws = &self.workspaces[i];
                    let mut conn = ws.conn.clone();
                    conn.id = uuid_like(&conn.id, &db);
                    if conn.driver == DriverId::Sqlite {
                        conn.database = std::path::Path::new(&conn.database).with_file_name(&db).to_string_lossy().into_owned();
                        conn.name = db.clone();
                    } else {
                        conn.database = db.clone();
                        conn.name = format!("{} / {db}", ws.conn.name);
                    }
                    let password = ws.password.clone();
                    self.open_workspace(ctx, conn, password);
                }
                WsOutput::SetSafeLevel(level) => {
                    let id = self.workspaces[i].conn.id.clone();
                    if let Some(c) = self.store.get(&id).cloned() {
                        let mut c = c;
                        c.safe_level = level;
                        self.store.upsert(c);
                        self.save_connections();
                    }
                }
                WsOutput::EditConnection => {
                    let id = self.workspaces[i].conn.id.clone();
                    self.section = Section::Connections;
                    self.edit_connection(&id);
                }
            }
        }
    }

    /// Modal dialogs.
    fn modals(&mut self, ctx: &egui::Context) {
        if let Some(c) = &self.confirm {
            let (title, body, button) = (c.title.clone(), c.body.clone(), c.button.clone());
            let mut decision = None;
            widgets::modal(ctx, &title, |ui| {
                ui.label(&body);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if widgets::danger_button(ui, &button).clicked() {
                        decision = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    if let Some(c) = self.confirm.take() {
                        self.run_confirmed(c.action);
                    }
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        if let Some(text) = self.import_url.as_mut() {
            let mut result = None;
            widgets::modal(ctx, "Import connection from URL", |ui| {
                widgets::field_label(ui, "URL");
                widgets::input(ui, text, "postgresql://user:password@host:5432/database");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Import").clicked() {
                        result = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        result = Some(false);
                    }
                });
            });
            let text = text.clone();
            match result {
                Some(true) => match parse_url(&text) {
                    Ok(spec) => {
                        let c = StoredConnection::from_spec(&spec);
                        let mut f = ConnForm::from_stored(&c, spec.password.clone());
                        f.id = None;
                        self.form = Some(Box::new(f));
                        self.import_url = None;
                    }
                    Err(e) => self.toasts.err(e.to_string()),
                },
                Some(false) => self.import_url = None,
                None => {}
            }
        }
        if let Some(name) = self.new_group.as_mut() {
            let mut result = None;
            widgets::modal(ctx, "New group", |ui| {
                widgets::field_label(ui, "Name");
                widgets::input(ui, name, "Group name");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Create").clicked() {
                        result = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        result = Some(false);
                    }
                });
            });
            let name = name.trim().to_owned();
            match result {
                Some(true) if !name.is_empty() => {
                    self.store.add_group(&name, None);
                    self.save_connections();
                    self.new_group = None;
                }
                Some(false) => self.new_group = None,
                _ => {}
            }
        }
        if let Some(p) = self.password_prompt.as_mut() {
            let mut result = None;
            let title = format!("Password for {}", p.conn.name);
            widgets::modal(ctx, &title, |ui| {
                widgets::secret_input(ui, &mut p.value, &mut p.reveal);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, "Connect").clicked() {
                        result = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel").clicked() {
                        result = Some(false);
                    }
                });
            });
            match result {
                Some(true) => {
                    if let Some(p) = self.password_prompt.take() {
                        self.open_workspace(ctx, p.conn, Some(p.value));
                    }
                }
                Some(false) => self.password_prompt = None,
                None => {}
            }
        }
    }
}

/// Id of the transient connection of another database (keeps the history of the original apart).
fn uuid_like(base: &str, db: &str) -> String {
    let safe: String = db.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("{base}_{safe}")
}

/// Runs a shell command and returns its trimmed stdout.
fn run_password_command(cmd: &str) -> Result<String, String> {
    let output = if cfg!(windows) { std::process::Command::new("cmd").args(["/C", cmd]).output() } else { std::process::Command::new("sh").args(["-c", cmd]).output() }.map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl eframe::App for App {
    /// Draws one frame.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.shortcuts(ctx);
        self.rail(ctx);
        match self.section {
            Section::Connections => self.connections_section(ctx),
            Section::Workspace(i) => self.workspace_section(ctx, i),
            Section::Settings => {
                let frame = Frame::new().fill(theme::DEEP_DARK).inner_margin(Margin::symmetric(28, 22));
                let mut changed = false;
                egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                    let width = ui.available_width().min(DETAIL_MAX_WIDTH);
                    let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(width, ui.max_rect().height()));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        changed = settings::show(ui, &mut self.settings, &mut self.settings_tab, &mut |ui, s| crate::ui::mcp::settings_tab(ui, s, &mut self.mcp));
                    });
                });
                if changed {
                    self.save_settings();
                }
            }
        }
        self.modals(ctx);
        crate::ui::mcp::approval_modal(ctx, &mut self.mcp);
        widgets::draw_toasts(ctx, &mut self.toasts);
    }
}
