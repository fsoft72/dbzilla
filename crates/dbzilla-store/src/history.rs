//! Per-connection query history stored as JSON lines (SPEC 10.13).

use crate::{StoreError, StoreResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Statements longer than this are cut when stored.
const MAX_SQL_BYTES: usize = 1_000_000;
/// Marker appended to cut statements.
const TRUNCATED: &str = "\n... [truncated]";

/// Where a history entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryKind {
    Editor,
    Commit,
    UserManagement,
}

/// One executed statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix time in milliseconds.
    pub ts: i64,
    pub sql: String,
    pub kind: HistoryKind,
    pub duration_ms: Option<u64>,
    pub ok: bool,
    pub database: Option<String>,
    /// Rows as they were before an UPDATE or DELETE (CSV), when that option is on.
    pub original_csv: Option<String>,
}

/// The history files.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    dir: PathBuf,
    enabled: bool,
}

impl HistoryStore {
    /// Store writing into `dir`; when `enabled` is false nothing is written.
    pub fn new(dir: PathBuf, enabled: bool) -> Self {
        Self { dir, enabled }
    }

    /// Path of a connection's file; ids must not contain path separators.
    fn file(&self, conn_id: &str) -> StoreResult<PathBuf> {
        if conn_id.is_empty() || conn_id.contains(['/', '\\']) || conn_id.contains("..") {
            return Err(StoreError::InvalidPath(conn_id.to_owned()));
        }
        Ok(self.dir.join(format!("{conn_id}.jsonl")))
    }

    /// Appends an entry (cutting huge statements).
    pub fn append(&self, conn_id: &str, entry: &HistoryEntry) -> StoreResult<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut entry = entry.clone();
        if entry.sql.len() > MAX_SQL_BYTES {
            let mut cut = MAX_SQL_BYTES;
            while !entry.sql.is_char_boundary(cut) {
                cut -= 1;
            }
            entry.sql.truncate(cut);
            entry.sql.push_str(TRUNCATED);
        }
        fs::create_dir_all(&self.dir)?;
        let mut opts = fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(self.file(conn_id)?)?;
        writeln!(f, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }

    /// Newest-first entries whose SQL contains `query` (case-insensitive), at most `limit`.
    pub fn search(&self, conn_id: &str, query: &str, limit: usize) -> StoreResult<Vec<HistoryEntry>> {
        let path = self.file(conn_id)?;
        if !Path::new(&path).exists() {
            return Ok(vec![]);
        }
        let needle = query.to_lowercase();
        let text = fs::read_to_string(path)?;
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
            .filter(|e| needle.is_empty() || e.sql.to_lowercase().contains(&needle))
            .rev()
            .take(limit)
            .collect())
    }

    /// Deletes a connection's whole history.
    pub fn clear(&self, conn_id: &str) -> StoreResult<()> {
        let path = self.file(conn_id)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}
