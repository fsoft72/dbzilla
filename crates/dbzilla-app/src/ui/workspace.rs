//! An open connection: session, sidebar, tabs, console and the workspace screen (SPEC 7).

use super::console::{self, ConsoleLog};
use super::palette::{self, PaletteChoice, PaletteItem, PaletteState};
use super::sidebar::{self, SidebarAction, SidebarState, SidebarTab};
use super::tabs::{decide_open, OpenDecision, OpenMode, TabId, TabMeta, TabTarget};
use super::widgets;
use super::Toasts;
use crate::glyphs::Ico;
use crate::keymap::{Action, KeyMap};
use crate::theme;
use dbzilla_core::caps::Capabilities;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ObjectInfo, ObjectKind, TableRef, TableStructure};
use dbzilla_drivers::DatabaseInfo;
use dbzilla_session::{Command, Event, SessionHandle};
use dbzilla_store::connections::StoredConnection;
use dbzilla_store::settings::Settings;
use dbzilla_store::tags::TagStore;
use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Stroke};
use egui_dock::{DockArea, DockState, Style, TabViewer};
use std::collections::HashMap;

/// Connection state shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum WsStatus {
    Connecting,
    Connected(String),
    Failed(String),
    Disconnected(String),
}

/// Content of a table tab (the data grid arrives with the data tab).
pub struct TableTab {
    pub table: TableRef,
    pub structure: Option<TableStructure>,
    pub error: Option<String>,
}

/// What a tab holds.
pub enum TabKind {
    Table(TableTab),
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
}

impl Workspace {
    /// Creates the workspace of a connection whose session is already connecting.
    pub fn new(conn: StoredConnection, session: SessionHandle, console_entries: usize) -> Self {
        let caps = Capabilities::for_driver(conn.driver);
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

    /// Stores a structure and hands it to the tabs showing that table.
    fn apply_structure(&mut self, st: TableStructure) {
        for (_, tab) in self.dock.iter_all_tabs_mut() {
            let TabKind::Table(t) = &mut tab.kind;
            if t.table == st.table {
                t.structure = Some(st.clone());
                t.error = None;
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
            })
            .collect()
    }

    /// Id of the focused tab, if any.
    fn focused_tab(&mut self) -> Option<TabId> {
        self.dock.find_active_focused().map(|(_, t)| t.id)
    }

    /// Table of the focused tab.
    fn active_table(&mut self) -> Option<TableRef> {
        self.dock.find_active_focused().map(|(_, t)| match &t.kind {
            TabKind::Table(tt) => tt.table.clone(),
        })
    }

    /// Opens a table or view following the preview-tab rules.
    pub fn open_table(&mut self, table: TableRef, mode: OpenMode) {
        let metas = self.tab_metas();
        let focused = self.focused_tab();
        let decision = decide_open(&metas, focused, &TabTarget::Table(table.clone()), mode);
        let new_tab = |ws: &mut Workspace, pinned: bool| {
            let id = ws.next_tab_id;
            ws.next_tab_id += 1;
            Tab { id, pinned, kind: TabKind::Table(TableTab { structure: ws.structures.get(&table).cloned(), table: table.clone(), error: None }) }
        };
        match decision {
            OpenDecision::Focus(id) => {
                if let Some(loc) = self.tab_location(id) {
                    self.dock.set_active_tab(loc);
                }
                if mode == OpenMode::Pin {
                    self.set_pinned(id, true);
                }
            }
            OpenDecision::Replace(id) => {
                let fresh = new_tab(self, mode == OpenMode::Pin);
                if let Some((_, slot)) = self.dock.iter_all_tabs_mut().find(|(_, t)| t.id == id) {
                    *slot = fresh;
                }
            }
            OpenDecision::New => {
                let fresh = new_tab(self, mode != OpenMode::Preview);
                self.dock.push_to_focused_leaf(fresh);
            }
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
            Action::Reconnect => {
                self.status = WsStatus::Connecting;
                self.session.send(Command::Reconnect);
            }
            other => return Some(WsOutput::Run(other)),
        }
        None
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
        self.toolbar(ctx, env, &mut out);
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
        let mut requests = Vec::new();
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
            let mut viewer = Viewer { requests: &mut requests };
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

    /// The toolbar (SPEC 7.2).
    fn toolbar(&mut self, ctx: &egui::Context, env: &WsEnv, out: &mut Vec<WsOutput>) {
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
                ui.add_enabled_ui(false, |ui| {
                    widgets::ico_button(ui, Ico::Discard, &tip("Discard changes", Action::Discard));
                    widgets::ico_button(ui, Ico::Preview, &tip("Preview changes", Action::Preview));
                    widgets::ico_button(ui, Ico::Commit, &tip("Commit", Action::Commit));
                });
                ui.add_space(6.0);
                widgets::ico_button(ui, Ico::Shield, &format!("Safe mode: {}", super::connform::safe_label(self.conn.safe_level)));
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
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        let t = tab.title();
        if tab.pinned { RichText::new(t).into() } else { RichText::new(t).italics().into() }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let TabKind::Table(t) = &mut tab.kind;
        egui::Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
            widgets::heading(ui, &t.table.name);
            match (&t.structure, &t.error) {
                (_, Some(e)) => {
                    ui.label(RichText::new(e).color(theme::ERROR));
                }
                (Some(st), _) => {
                    widgets::muted(ui, &format!("{} columns", st.columns.len()));
                    for c in &st.columns {
                        ui.label(RichText::new(format!("{}  {}", c.name, c.type_name)).monospace().color(theme::TEXT));
                    }
                }
                (None, None) => {
                    ui.spinner();
                }
            }
        });
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
