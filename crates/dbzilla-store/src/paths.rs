//! Locations of the application files (SPEC 4).

use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Directory layout of the application data.
#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// Layout rooted at `root` (used by tests and `--data-dir`).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Platform default location, or `None` when no home directory can be determined.
    pub fn platform_default() -> Option<Self> {
        ProjectDirs::from("com", "dbzilla", "DBZilla").map(|d| Self::new(d.config_dir().to_path_buf()))
    }

    /// Root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `settings.json`.
    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// `connections.json` (no secrets).
    pub fn connections_file(&self) -> PathBuf {
        self.root.join("connections.json")
    }

    /// `diagrams.json` (table positions of the ER diagrams).
    pub fn diagrams_file(&self) -> PathBuf {
        self.root.join("diagrams.json")
    }

    /// `metrics.json` (metrics boards).
    pub fn metrics_file(&self) -> PathBuf {
        self.root.join("metrics.json")
    }

    /// `tags.json`.
    pub fn tags_file(&self) -> PathBuf {
        self.root.join("tags.json")
    }

    /// Directory of per-connection history files.
    pub fn history_dir(&self) -> PathBuf {
        self.root.join("history")
    }

    /// Directory of saved queries.
    pub fn queries_dir(&self) -> PathBuf {
        self.root.join("queries")
    }

    /// `session.json` (workspaces to reopen).
    pub fn session_file(&self) -> PathBuf {
        self.root.join("session.json")
    }

    /// `mcp_activity.jsonl`.
    pub fn mcp_activity_file(&self) -> PathBuf {
        self.root.join("mcp_activity.jsonl")
    }

    /// `mcp_tokens.json` (token metadata and hashes, never the token itself).
    pub fn mcp_tokens_file(&self) -> PathBuf {
        self.root.join("mcp_tokens.json")
    }
}
