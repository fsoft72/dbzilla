//! Saved connections and groups (SPEC 6.2, 6.4). Secrets never live here.

use crate::atomic::{load_json, save_json, LoadResult};
use crate::paths::AppPaths;
use crate::StoreResult;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::SslMode;
use dbzilla_core::safemode::SafeLevel;
use dbzilla_core::url::ConnectionSpec;
use serde::{Deserialize, Serialize};

/// Current connections schema version.
pub const CONNECTIONS_VERSION: u32 = 1;

/// How the password is obtained (SPEC FR-C-010).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum PasswordMode {
    #[default]
    Keychain,
    AskEveryTime,
    NoPassword,
    /// Shell command whose stdout is the password or token.
    Command(String),
}

/// A saved connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredConnection {
    pub id: String,
    pub name: String,
    pub driver: DriverId,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    /// Database name, or file path for SQLite.
    pub database: String,
    pub ssl: SslMode,
    pub safe_level: SafeLevel,
    pub status_color: Option<String>,
    pub tag: Option<String>,
    pub password_mode: PasswordMode,
    pub bootstrap: Vec<String>,
    pub group: Option<String>,
    pub row_limit: Option<u64>,
}

impl StoredConnection {
    /// A new connection with a fresh id and defaults.
    pub fn new(name: &str, driver: DriverId) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            driver,
            host: String::new(),
            port: None,
            user: String::new(),
            database: String::new(),
            ssl: SslMode::Preferred,
            safe_level: SafeLevel::Silent,
            status_color: None,
            tag: None,
            password_mode: PasswordMode::Keychain,
            bootstrap: vec![],
            group: None,
            row_limit: None,
        }
    }

    /// Builds a connection from a parsed URL (the password is not kept here).
    pub fn from_spec(spec: &ConnectionSpec) -> Self {
        let fallback = if spec.host.is_empty() { spec.database.rsplit('/').next().unwrap_or("connection").to_owned() } else { spec.host.clone() };
        let mut c = Self::new(spec.name.as_deref().unwrap_or(&fallback), spec.driver);
        c.host = spec.host.clone();
        c.port = spec.port;
        c.user = spec.user.clone();
        c.database = spec.database.clone();
        c.status_color = spec.status_color.clone();
        c.tag = spec.env.clone();
        if let Some(i) = spec.safe_level {
            c.safe_level = safe_level_from_index(i);
        }
        if let Some(i) = spec.tls_mode {
            c.ssl = ssl_from_index(spec.driver, i);
        }
        c.password_mode = if spec.password.is_some() { PasswordMode::Keychain } else { PasswordMode::NoPassword };
        c
    }

    /// Converts to a URL spec with the given password (fetched from the keychain by the caller).
    pub fn to_spec(&self, password: Option<String>) -> ConnectionSpec {
        let mut s = ConnectionSpec::new(self.driver);
        s.name = Some(self.name.clone());
        s.host = self.host.clone();
        s.port = self.port;
        s.user = self.user.clone();
        s.password = password;
        s.database = self.database.clone();
        s.status_color = self.status_color.clone();
        s.env = self.tag.clone();
        s.safe_level = Some(safe_level_to_index(self.safe_level));
        s.tls_mode = Some(ssl_to_index(self.driver, self.ssl));
        s
    }
}

/// Safe level index used in URLs: 0 Silent, 1 Alert 1, 2 Alert 2, 3 Safe 1, 4 Safe 2.
pub fn safe_level_from_index(i: u8) -> SafeLevel {
    match i {
        1 => SafeLevel::Alert1,
        2 => SafeLevel::Alert2,
        3 => SafeLevel::Safe1,
        4 => SafeLevel::Safe2,
        _ => SafeLevel::Silent,
    }
}

/// Inverse of [`safe_level_from_index`].
pub fn safe_level_to_index(l: SafeLevel) -> u8 {
    match l {
        SafeLevel::Silent => 0,
        SafeLevel::Alert1 => 1,
        SafeLevel::Alert2 => 2,
        SafeLevel::Safe1 => 3,
        SafeLevel::Safe2 => 4,
    }
}

/// TLS mode from the driver-specific enum index of the URL `tLSMode` option.
pub fn ssl_from_index(driver: DriverId, i: u8) -> SslMode {
    match (driver, i) {
        (_, 1) => SslMode::Disabled,
        (_, 2) => SslMode::Required,
        (DriverId::Postgres, 4) | (DriverId::MySql, 3) => SslMode::VerifyCa,
        (DriverId::Postgres, 5) | (DriverId::MySql, 4) => SslMode::VerifyFull,
        _ => SslMode::Preferred,
    }
}

/// Inverse of [`ssl_from_index`].
pub fn ssl_to_index(driver: DriverId, m: SslMode) -> u8 {
    match (driver, m) {
        (_, SslMode::Preferred) => 0,
        (_, SslMode::Disabled) => 1,
        (_, SslMode::Required) => 2,
        (DriverId::Postgres, SslMode::VerifyCa) => 4,
        (DriverId::Postgres, SslMode::VerifyFull) => 5,
        (_, SslMode::VerifyCa) => 3,
        (_, SslMode::VerifyFull) => 4,
    }
}

/// A folder of connections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub parent: Option<String>,
}

/// On-disk form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct ConnectionsFile {
    schema_version: u32,
    connections: Vec<StoredConnection>,
    groups: Vec<Group>,
}

impl Default for ConnectionsFile {
    fn default() -> Self {
        Self { schema_version: CONNECTIONS_VERSION, connections: vec![], groups: vec![] }
    }
}

/// The saved connections.
#[derive(Debug, Clone, Default)]
pub struct ConnectionStore {
    data: ConnectionsFile,
}

impl ConnectionStore {
    /// Loads `connections.json`.
    pub fn load(paths: &AppPaths) -> LoadResult<ConnectionStore> {
        let r: LoadResult<ConnectionsFile> = load_json(&paths.connections_file(), u64::from(CONNECTIONS_VERSION));
        LoadResult { value: ConnectionStore { data: r.value }, warning: r.warning }
    }

    /// Saves `connections.json` atomically.
    pub fn save(&self, paths: &AppPaths) -> StoreResult<()> {
        save_json(&paths.connections_file(), &self.data)
    }

    /// All connections in insertion order.
    pub fn list(&self) -> &[StoredConnection] {
        &self.data.connections
    }

    /// One connection by id.
    pub fn get(&self, id: &str) -> Option<&StoredConnection> {
        self.data.connections.iter().find(|c| c.id == id)
    }

    /// Inserts or replaces by id.
    pub fn upsert(&mut self, c: StoredConnection) {
        match self.data.connections.iter_mut().find(|x| x.id == c.id) {
            Some(slot) => *slot = c,
            None => self.data.connections.push(c),
        }
    }

    /// Removes a connection; false when it does not exist.
    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.data.connections.len();
        self.data.connections.retain(|c| c.id != id);
        self.data.connections.len() != before
    }

    /// Duplicates a connection under a new id named "<name> copy"; returns the new id.
    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let mut c = self.get(id)?.clone();
        c.id = uuid::Uuid::new_v4().to_string();
        c.name = format!("{} copy", c.name);
        let new_id = c.id.clone();
        self.data.connections.push(c);
        Some(new_id)
    }

    /// Creates a group and returns its id.
    pub fn add_group(&mut self, name: &str, parent: Option<String>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.data.groups.push(Group { id: id.clone(), name: name.to_owned(), icon: None, parent });
        id
    }

    /// All groups.
    pub fn groups(&self) -> &[Group] {
        &self.data.groups
    }

    /// Deletes a group, its sub-groups and every connection inside; returns the removed connection ids
    /// so the caller can delete their secrets.
    pub fn delete_group(&mut self, id: &str) -> Vec<String> {
        let mut doomed = vec![id.to_owned()];
        let mut i = 0;
        while i < doomed.len() {
            let current = doomed[i].clone();
            doomed.extend(self.data.groups.iter().filter(|g| g.parent.as_deref() == Some(current.as_str())).map(|g| g.id.clone()));
            i += 1;
        }
        let removed: Vec<String> = self.data.connections.iter().filter(|c| c.group.as_ref().is_some_and(|g| doomed.contains(g))).map(|c| c.id.clone()).collect();
        self.data.connections.retain(|c| !removed.contains(&c.id));
        self.data.groups.retain(|g| !doomed.contains(&g.id));
        removed
    }
}
