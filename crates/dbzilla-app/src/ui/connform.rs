//! The connection form (SPEC 6.2): state, validation and drawing.

use super::widgets::{self, field_label, muted};
use crate::glyphs::Ico;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::SslMode;
use dbzilla_core::safemode::SafeLevel;
use dbzilla_core::url::default_port;
use dbzilla_drivers::ConnectConfig;
use dbzilla_store::connections::{PasswordMode, StoredConnection};
use dbzilla_store::tags::Tag;
use eframe::egui::{self, RichText, Ui};
use std::sync::mpsc::Receiver;

/// Width reserved for the port column.
const PORT_COLUMN: f32 = 110.0;

/// Result of a connection test: server version or error text.
pub type TestResult = Result<String, String>;

/// State of the "Test" button.
pub enum TestState {
    Running(Receiver<TestResult>),
    Done(TestResult),
}

/// Password storage choice shown in the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwKind {
    Keychain,
    Ask,
    None,
    Command,
}

impl PwKind {
    /// Text shown in the combo box.
    pub fn label(self) -> &'static str {
        match self {
            PwKind::Keychain => "Store in keychain",
            PwKind::Ask => "Ask every time",
            PwKind::None => "No password",
            PwKind::Command => "Command line",
        }
    }
}

/// What the form asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    Connect,
    Save,
    Test,
    Cancel,
    Delete,
}

/// Editable copy of a connection.
pub struct ConnForm {
    pub id: Option<String>,
    pub name: String,
    pub driver: DriverId,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub reveal: bool,
    pub database: String,
    pub ssl: SslMode,
    pub safe_level: SafeLevel,
    pub tag: Option<String>,
    pub status_color: String,
    pub pw_kind: PwKind,
    pub command: String,
    pub bootstrap: String,
    pub group: Option<String>,
    pub test: Option<TestState>,
    pub error: Option<String>,
    original: String,
}

impl ConnForm {
    /// Empty form for a new connection.
    pub fn new(driver: DriverId, default_safe: SafeLevel) -> Self {
        let mut f = Self {
            id: None,
            name: String::new(),
            driver,
            host: if driver == DriverId::Sqlite { String::new() } else { "127.0.0.1".into() },
            port: String::new(),
            user: match driver {
                DriverId::Postgres => "postgres".into(),
                DriverId::MySql => "root".into(),
                DriverId::SqlServer => "sa".into(),
                DriverId::Sqlite => String::new(),
            },
            password: String::new(),
            reveal: false,
            database: String::new(),
            ssl: SslMode::Preferred,
            safe_level: default_safe,
            tag: None,
            status_color: String::new(),
            pw_kind: if driver == DriverId::Sqlite { PwKind::None } else { PwKind::Keychain },
            command: String::new(),
            bootstrap: String::new(),
            group: None,
            test: None,
            error: None,
            original: String::new(),
        };
        f.original = f.fingerprint();
        f
    }

    /// Form editing a saved connection; `password` comes from the keychain.
    pub fn from_stored(c: &StoredConnection, password: Option<String>) -> Self {
        let (pw_kind, command) = match &c.password_mode {
            PasswordMode::Keychain => (PwKind::Keychain, String::new()),
            PasswordMode::AskEveryTime => (PwKind::Ask, String::new()),
            PasswordMode::NoPassword => (PwKind::None, String::new()),
            PasswordMode::Command(cmd) => (PwKind::Command, cmd.clone()),
        };
        let mut f = Self {
            id: Some(c.id.clone()),
            name: c.name.clone(),
            driver: c.driver,
            host: c.host.clone(),
            port: c.port.map(|p| p.to_string()).unwrap_or_default(),
            user: c.user.clone(),
            password: password.unwrap_or_default(),
            reveal: false,
            database: c.database.clone(),
            ssl: c.ssl,
            safe_level: c.safe_level,
            tag: c.tag.clone(),
            status_color: c.status_color.clone().unwrap_or_default(),
            pw_kind,
            command,
            bootstrap: c.bootstrap.join("\n"),
            group: c.group.clone(),
            test: None,
            error: None,
            original: String::new(),
        };
        f.original = f.fingerprint();
        f
    }

    /// Text identifying the editable state, for dirty checks.
    fn fingerprint(&self) -> String {
        format!(
            "{}|{:?}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{}|{:?}|{}|{}|{:?}",
            self.name, self.driver, self.host, self.port, self.user, self.password, self.database, self.ssl, self.safe_level, self.tag, self.status_color, self.pw_kind, self.command, self.bootstrap, self.group
        )
    }

    /// True when something changed since the form was opened or last saved.
    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.fingerprint() != self.original
    }

    /// Marks the current state as saved.
    pub fn mark_saved(&mut self) {
        self.original = self.fingerprint();
    }

    /// Validates and converts to a stored connection (keeping the id when editing).
    pub fn to_stored(&self) -> Result<StoredConnection, String> {
        let port = if self.port.trim().is_empty() {
            None
        } else {
            Some(self.port.trim().parse::<u16>().ok().filter(|p| *p > 0).ok_or("The port must be a number between 1 and 65535")?)
        };
        if self.driver == DriverId::Sqlite && self.database.trim().is_empty() {
            return Err("Choose the SQLite database file".into());
        }
        if self.driver != DriverId::Sqlite && self.host.trim().is_empty() {
            return Err("The host is required".into());
        }
        if !self.status_color.trim().is_empty() && theme::parse_hex_color(&self.status_color).is_none() {
            return Err("The status color must be a hex value like 3F6BDB".into());
        }
        if self.pw_kind == PwKind::Command && self.command.trim().is_empty() {
            return Err("Enter the command that prints the password".into());
        }
        let mut c = StoredConnection::new(&self.name, self.driver);
        if let Some(id) = &self.id {
            c.id = id.clone();
        }
        c.host = self.host.trim().to_owned();
        c.port = port;
        c.user = self.user.trim().to_owned();
        c.database = self.database.trim().to_owned();
        c.ssl = self.ssl;
        c.safe_level = self.safe_level;
        c.tag = self.tag.clone();
        c.status_color = if self.status_color.trim().is_empty() { None } else { Some(self.status_color.trim().trim_start_matches('#').to_owned()) };
        c.password_mode = match self.pw_kind {
            PwKind::Keychain => PasswordMode::Keychain,
            PwKind::Ask => PasswordMode::AskEveryTime,
            PwKind::None => PasswordMode::NoPassword,
            PwKind::Command => PasswordMode::Command(self.command.trim().to_owned()),
        };
        c.bootstrap = self.bootstrap.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        c.group = self.group.clone();
        if c.name.trim().is_empty() {
            c.name = default_name(&c);
        }
        Ok(c)
    }

    /// Connection settings for a test run with the typed password.
    pub fn to_connect_config(&self) -> Result<ConnectConfig, String> {
        let c = self.to_stored()?;
        let password = if self.pw_kind == PwKind::Keychain || self.pw_kind == PwKind::Ask { Some(self.password.clone()).filter(|p| !p.is_empty()) } else { None };
        Ok(config_for(&c, password))
    }
}

/// Builds driver settings from a stored connection.
pub fn config_for(c: &StoredConnection, password: Option<String>) -> ConnectConfig {
    ConnectConfig { driver: c.driver, host: c.host.clone(), port: c.port, user: c.user.clone(), password, database: c.database.clone(), ssl: c.ssl, bootstrap: c.bootstrap.clone() }
}

/// Name used when the user leaves the field empty.
fn default_name(c: &StoredConnection) -> String {
    match c.driver {
        DriverId::Sqlite => std::path::Path::new(&c.database).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "SQLite".into()),
        _ if c.database.is_empty() => c.host.clone(),
        _ => format!("{} / {}", c.host, c.database),
    }
}

/// Display name of a driver.
pub fn driver_label(d: DriverId) -> &'static str {
    match d {
        DriverId::Sqlite => "SQLite",
        DriverId::Postgres => "PostgreSQL",
        DriverId::MySql => "MySQL / MariaDB",
        DriverId::SqlServer => "SQL Server",
    }
}

/// TLS modes offered by a driver.
fn ssl_modes(d: DriverId) -> &'static [SslMode] {
    match d {
        DriverId::Postgres => &[SslMode::Preferred, SslMode::Disabled, SslMode::Required, SslMode::VerifyCa, SslMode::VerifyFull],
        _ => &[SslMode::Preferred, SslMode::Disabled, SslMode::Required, SslMode::VerifyCa, SslMode::VerifyFull],
    }
}

/// Text of a TLS mode for a driver.
fn ssl_label(d: DriverId, m: SslMode) -> &'static str {
    match (d, m) {
        (_, SslMode::Preferred) => "PREFERRED",
        (_, SslMode::Disabled) => "DISABLED",
        (_, SslMode::Required) => "REQUIRED",
        (_, SslMode::VerifyCa) => "VERIFY-CA",
        (DriverId::MySql, SslMode::VerifyFull) => "VERIFY-IDENTITY",
        (_, SslMode::VerifyFull) => "VERIFY-FULL",
    }
}

/// Draws the form and returns the requested action.
pub fn show(ui: &mut Ui, f: &mut ConnForm, tags: &[Tag], busy: bool) -> Option<FormAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        widgets::icon_badge(ui, Ico::Database, theme::SECONDARY, 52.0);
        ui.vertical(|ui| {
            let title = if f.id.is_some() { if f.name.is_empty() { "Edit connection".to_owned() } else { f.name.clone() } } else { "New connection".to_owned() };
            widgets::heading(ui, &title);
            muted(ui, driver_label(f.driver));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if f.id.is_some() && widgets::danger_button(ui, "Delete").clicked() {
                action = Some(FormAction::Delete);
            }
            if widgets::primary_button(ui, "Connect").clicked() {
                action = Some(FormAction::Connect);
            }
            if widgets::secondary_button(ui, "Save").clicked() {
                action = Some(FormAction::Save);
            }
            ui.add_enabled_ui(!busy, |ui| {
                if widgets::secondary_button(ui, "Test").clicked() {
                    action = Some(FormAction::Test);
                }
            });
            if widgets::secondary_button(ui, "Cancel").clicked() {
                action = Some(FormAction::Cancel);
            }
        });
    });
    ui.add_space(6.0);
    show_test_state(ui, f);
    if let Some(e) = &f.error {
        ui.label(RichText::new(e).color(theme::ERROR));
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        widgets::card(ui, |ui| general(ui, f, tags));
        if f.driver == DriverId::Sqlite {
            widgets::card(ui, |ui| {
                field_label(ui, "Database file");
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 100.0).max(60.0);
                    ui.add(egui::TextEdit::singleline(&mut f.database).hint_text("/path/to/file.sqlite").desired_width(width).margin(egui::Margin::symmetric(8, 9)));
                    if widgets::secondary_button(ui, "Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("SQLite", &["sqlite", "sqlite3", "db"]).pick_file() {
                            f.database = p.to_string_lossy().into_owned();
                        }
                    }
                });
            });
        } else {
            widgets::card(ui, |ui| server(ui, f));
            widgets::card(ui, |ui| {
                field_label(ui, "SSL mode");
                let d = f.driver;
                egui::ComboBox::from_id_salt("ssl").selected_text(ssl_label(d, f.ssl)).width(220.0).show_ui(ui, |ui| {
                    for m in ssl_modes(d) {
                        ui.selectable_value(&mut f.ssl, *m, ssl_label(d, *m));
                    }
                });
            });
        }
        widgets::card(ui, |ui| {
            field_label(ui, "Bootstrap commands");
            muted(ui, "SQL run right after connecting, one statement per line");
            widgets::text_area(ui, &mut f.bootstrap, 3);
        });
    });
    action
}

/// Result line of the test button.
fn show_test_state(ui: &mut Ui, f: &mut ConnForm) {
    match &f.test {
        Some(TestState::Running(_)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                muted(ui, "Testing the connection...");
            });
        }
        Some(TestState::Done(Ok(v))) => {
            ui.label(RichText::new(format!("Connection works. Server: {v}")).color(theme::OK));
        }
        Some(TestState::Done(Err(e))) => {
            ui.label(RichText::new(format!("Connection failed: {e}")).color(theme::ERROR));
        }
        None => {}
    }
}

/// Name, driver, tag, status colour and safe mode.
fn general(ui: &mut Ui, f: &mut ConnForm, tags: &[Tag]) {
    field_label(ui, "Name");
    widgets::input(ui, &mut f.name, "Connection name");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            field_label(ui, "Tag");
            let selected = f.tag.clone().unwrap_or_else(|| "NO TAG".into());
            egui::ComboBox::from_id_salt("tag").selected_text(selected).width(150.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut f.tag, None, "NO TAG");
                for t in tags {
                    ui.selectable_value(&mut f.tag, Some(t.name.clone()), &t.name);
                }
            });
        });
        ui.vertical(|ui| {
            field_label(ui, "Safe mode");
            egui::ComboBox::from_id_salt("safe").selected_text(safe_label(f.safe_level)).width(170.0).show_ui(ui, |ui| {
                for l in [SafeLevel::Silent, SafeLevel::Alert1, SafeLevel::Alert2, SafeLevel::Safe1, SafeLevel::Safe2] {
                    ui.selectable_value(&mut f.safe_level, l, safe_label(l));
                }
            });
        });
        ui.vertical(|ui| {
            field_label(ui, "Status color");
            ui.add(egui::TextEdit::singleline(&mut f.status_color).hint_text("RRGGBB").desired_width(110.0).margin(egui::Margin::symmetric(8, 9)));
        });
    });
}

/// Text of a safe mode level.
pub fn safe_label(l: SafeLevel) -> &'static str {
    match l {
        SafeLevel::Silent => "Silent Mode",
        SafeLevel::Alert1 => "Alert Mode 1",
        SafeLevel::Alert2 => "Alert Mode 2",
        SafeLevel::Safe1 => "Safe Mode 1",
        SafeLevel::Safe2 => "Safe Mode 2",
    }
}

/// Host, port, user, password and database.
fn server(ui: &mut Ui, f: &mut ConnForm) {
    let total = ui.available_width();
    let gap = ui.spacing().item_spacing.x;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(total - PORT_COLUMN - gap, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            field_label(ui, "Host");
            widgets::input(ui, &mut f.host, "127.0.0.1");
        });
        ui.allocate_ui_with_layout(egui::vec2(PORT_COLUMN, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            field_label(ui, "Port");
            let hint = default_port(f.driver).map(|p| p.to_string()).unwrap_or_default();
            ui.add(egui::TextEdit::singleline(&mut f.port).hint_text(hint).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 9)));
        });
    });
    ui.add_space(4.0);
    field_label(ui, "User");
    widgets::input(ui, &mut f.user, "user");
    ui.add_space(4.0);
    field_label(ui, "Password");
    egui::ComboBox::from_id_salt("pwkind").selected_text(f.pw_kind.label()).width(170.0).show_ui(ui, |ui| {
        for k in [PwKind::Keychain, PwKind::Ask, PwKind::None, PwKind::Command] {
            ui.selectable_value(&mut f.pw_kind, k, k.label());
        }
    });
    match f.pw_kind {
        PwKind::Keychain => {
            widgets::secret_input(ui, &mut f.password, &mut f.reveal);
        }
        PwKind::Command => {
            widgets::input(ui, &mut f.command, "shell command that prints the password");
        }
        PwKind::Ask => {
            muted(ui, "The password is asked every time you connect.");
        }
        PwKind::None => {
            muted(ui, "No password is sent.");
        }
    }
    ui.add_space(4.0);
    field_label(ui, "Database");
    widgets::input(ui, &mut f.database, "database name (optional)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> ConnForm {
        let mut f = ConnForm::new(DriverId::Postgres, SafeLevel::Silent);
        f.host = "db.local".into();
        f
    }

    /// A valid form converts and gets a default name.
    #[test]
    fn valid_form_converts_with_default_name() {
        let mut f = pg();
        f.database = "shop".into();
        f.port = "5433".into();
        let c = f.to_stored().unwrap();
        assert_eq!(c.name, "db.local / shop");
        assert_eq!(c.port, Some(5433));
        assert_eq!(c.password_mode, PasswordMode::Keychain);
    }

    /// Bad ports, missing host/file, bad colours and empty commands are refused.
    #[test]
    fn validation_errors() {
        let mut f = pg();
        f.port = "abc".into();
        assert!(f.to_stored().unwrap_err().contains("port"));
        f.port = "70000".into();
        assert!(f.to_stored().is_err());
        f.port.clear();
        f.host.clear();
        assert!(f.to_stored().unwrap_err().contains("host"));
        let sq = ConnForm::new(DriverId::Sqlite, SafeLevel::Silent);
        assert!(sq.to_stored().unwrap_err().contains("file"));
        let mut f = pg();
        f.status_color = "nothex".into();
        assert!(f.to_stored().unwrap_err().contains("color"));
        let mut f = pg();
        f.pw_kind = PwKind::Command;
        assert!(f.to_stored().is_err());
    }

    /// Editing keeps the id; dirty tracking follows edits and saves.
    #[test]
    fn edit_round_trip_and_dirty_tracking() {
        let mut c = StoredConnection::new("Prod", DriverId::MySql);
        c.host = "h".into();
        c.bootstrap = vec!["SET @a=1".into()];
        let mut f = ConnForm::from_stored(&c, Some("pw".into()));
        assert!(!f.is_dirty());
        assert_eq!(f.to_stored().unwrap().id, c.id);
        f.user = "other".into();
        assert!(f.is_dirty());
        f.mark_saved();
        assert!(!f.is_dirty());
        assert_eq!(f.to_stored().unwrap().bootstrap, vec!["SET @a=1"]);
    }

    /// The test config carries the typed password only for keychain and ask modes.
    #[test]
    fn test_config_password_rules() {
        let mut f = pg();
        f.password = "secret".into();
        assert_eq!(f.to_connect_config().unwrap().password.as_deref(), Some("secret"));
        f.pw_kind = PwKind::None;
        assert_eq!(f.to_connect_config().unwrap().password, None);
    }

    /// SQLite defaults need no password and no host.
    #[test]
    fn sqlite_defaults() {
        let mut f = ConnForm::new(DriverId::Sqlite, SafeLevel::Silent);
        f.database = "/tmp/data.sqlite3".into();
        let c = f.to_stored().unwrap();
        assert_eq!(c.name, "data");
        assert_eq!(c.password_mode, PasswordMode::NoPassword);
    }
}
