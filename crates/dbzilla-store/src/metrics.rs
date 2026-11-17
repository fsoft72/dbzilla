//! Metrics boards (SPEC 11.2), kept together in `metrics.json`.

use crate::atomic::{load_json, save_json};
use crate::StoreResult;
use dbzilla_core::metrics::Board;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Current file schema version.
pub const METRICS_VERSION: u32 = 1;

/// On-disk form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct MetricsFile {
    schema_version: u32,
    boards: Vec<Board>,
}

impl Default for MetricsFile {
    fn default() -> Self {
        Self { schema_version: METRICS_VERSION, boards: Vec::new() }
    }
}

/// Reads and writes `metrics.json`.
#[derive(Debug, Clone)]
pub struct MetricsStore {
    path: PathBuf,
}

impl MetricsStore {
    /// A store backed by `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// All the boards; empty when there are none or the file is unusable.
    pub fn load(&self) -> Vec<Board> {
        load_json::<MetricsFile>(&self.path, u64::from(METRICS_VERSION)).value.boards
    }

    /// Replaces all the boards.
    pub fn save(&self, boards: &[Board]) -> StoreResult<()> {
        save_json(&self.path, &MetricsFile { schema_version: METRICS_VERSION, boards: boards.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::metrics::ObjectKind;

    #[test]
    fn missing_file_gives_no_boards() {
        let dir = tempfile::tempdir().unwrap();
        assert!(MetricsStore::new(dir.path().join("metrics.json")).load().is_empty());
    }

    #[test]
    fn boards_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetricsStore::new(dir.path().join("metrics.json"));
        let mut b = Board::new("b1", "Main");
        b.add(ObjectKind::Scoreboard, 5.0, 6.0);
        b.get_mut(1).unwrap().sql = "select count(*) from t".into();
        store.save(&[b.clone(), Board::new("b2", "Other")]).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], b);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.json");
        std::fs::write(&path, "{ not json").unwrap();
        let store = MetricsStore::new(path);
        assert!(store.load().is_empty());
        assert!(dir.path().join("metrics.json.bak").exists());
        store.save(&[Board::new("b", "B")]).unwrap();
        assert_eq!(store.load().len(), 1);
    }
}
