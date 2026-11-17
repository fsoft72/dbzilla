//! Backup and restore choices remembered per connection (SPEC 12.3), kept in `backup.json`.

use crate::atomic::{load_json, save_json};
use crate::StoreResult;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Current file schema version.
pub const BACKUP_VERSION: u32 = 1;

/// What the backup and restore windows remember for one connection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupPrefs {
    /// Options of the dump tool.
    pub backup_options: String,
    /// Options of the restore tool.
    pub restore_options: String,
    /// Directory searched before the `PATH` for the tools; empty means the `PATH` only.
    pub tool_dir: String,
    pub gzip: bool,
}

/// On-disk form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct BackupFile {
    schema_version: u32,
    connections: BTreeMap<String, BackupPrefs>,
}

impl Default for BackupFile {
    fn default() -> Self {
        Self { schema_version: BACKUP_VERSION, connections: BTreeMap::new() }
    }
}

/// Reads and writes `backup.json`.
#[derive(Debug, Clone)]
pub struct BackupStore {
    path: PathBuf,
}

impl BackupStore {
    /// A store backed by `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The choices of a connection; the defaults when it has none.
    pub fn load(&self, connection_id: &str) -> BackupPrefs {
        load_json::<BackupFile>(&self.path, u64::from(BACKUP_VERSION)).value.connections.get(connection_id).cloned().unwrap_or_default()
    }

    /// Saves the choices of a connection, keeping those of the others.
    pub fn save(&self, connection_id: &str, prefs: &BackupPrefs) -> StoreResult<()> {
        let mut file = load_json::<BackupFile>(&self.path, u64::from(BACKUP_VERSION)).value;
        file.schema_version = BACKUP_VERSION;
        file.connections.insert(connection_id.to_owned(), prefs.clone());
        save_json(&self.path, &file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(BackupStore::new(dir.path().join("backup.json")).load("c1"), BackupPrefs::default());
    }

    #[test]
    fn choices_are_kept_per_connection() {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path().join("backup.json"));
        let a = BackupPrefs { backup_options: "--clean".into(), tool_dir: "/opt/pg17/bin".into(), gzip: true, ..Default::default() };
        store.save("a", &a).unwrap();
        store.save("b", &BackupPrefs { restore_options: "--force".into(), ..Default::default() }).unwrap();
        assert_eq!(store.load("a"), a);
        assert_eq!(store.load("b").restore_options, "--force");
    }
}
