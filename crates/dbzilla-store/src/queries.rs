//! Saved queries: plain `.sql` files with folders (SPEC 10.12).

use crate::{StoreError, StoreResult};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Prefix of the optional first line carrying the autocomplete keywords.
const KEYWORDS_HEADER: &str = "-- keywords:";
/// Extension of query files.
const EXT: &str = "sql";

/// Sort order of the Queries tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySort {
    Name,
    Created,
    Modified,
}

/// One item of the tree listing.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryEntry {
    /// Path relative to the root, always with `/` separators.
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub modified_secs: i64,
    pub created_secs: i64,
}

/// A saved query.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedQuery {
    pub path: String,
    pub name: String,
    pub keywords: Vec<String>,
    pub sql: String,
}

/// The saved queries directory.
#[derive(Debug, Clone)]
pub struct QueryStore {
    root: PathBuf,
}

impl QueryStore {
    /// Store rooted at `root` (created on first write).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Resolves a relative path, refusing anything that could leave the root.
    fn resolve(&self, rel: &str) -> StoreResult<PathBuf> {
        let p = Path::new(rel);
        if rel.is_empty() || p.is_absolute() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(StoreError::InvalidPath(rel.to_owned()));
        }
        Ok(self.root.join(p))
    }

    /// Like `resolve` but makes sure files end in `.sql`.
    fn resolve_file(&self, rel: &str) -> StoreResult<PathBuf> {
        self.resolve(rel)?;
        let rel = if Path::new(rel).extension().is_some_and(|e| e == EXT) { rel.to_owned() } else { format!("{rel}.{EXT}") };
        self.resolve(&rel)
    }

    /// Writes a query with its keywords.
    pub fn save(&self, rel: &str, keywords: &[String], sql: &str) -> StoreResult<()> {
        let path = self.resolve_file(rel)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = String::new();
        if !keywords.is_empty() {
            text.push_str(&format!("{KEYWORDS_HEADER} {}\n", keywords.join(", ")));
        }
        text.push_str(sql);
        text.push('\n');
        crate::atomic::atomic_write(&path, text.as_bytes())?;
        Ok(())
    }

    /// Reads a query.
    pub fn read(&self, rel: &str) -> StoreResult<SavedQuery> {
        let path = self.resolve_file(rel)?;
        let text = fs::read_to_string(&path)?;
        let (keywords, body) = match text.split_once('\n') {
            Some((first, rest)) if first.starts_with(KEYWORDS_HEADER) => {
                (first[KEYWORDS_HEADER.len()..].split(',').map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()).collect(), rest)
            }
            _ => (vec![], text.as_str()),
        };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(SavedQuery { path: rel.to_owned(), name, keywords, sql: body.strip_suffix('\n').unwrap_or(body).to_owned() })
    }

    /// Creates a folder.
    pub fn create_folder(&self, rel: &str) -> StoreResult<()> {
        fs::create_dir_all(self.resolve(rel)?)?;
        Ok(())
    }

    /// Deletes a query or a folder (recursively).
    pub fn delete(&self, rel: &str) -> StoreResult<()> {
        let path = if self.resolve(rel)?.is_dir() { self.resolve(rel)? } else { self.resolve_file(rel)? };
        if path.is_dir() { fs::remove_dir_all(path)? } else { fs::remove_file(path)? }
        Ok(())
    }

    /// Renames or moves a query or folder; never overwrites.
    pub fn rename(&self, from: &str, to: &str) -> StoreResult<()> {
        let src = if self.resolve(from)?.is_dir() { self.resolve(from)? } else { self.resolve_file(from)? };
        let dst = if src.is_dir() { self.resolve(to)? } else { self.resolve_file(to)? };
        if dst.exists() {
            return Err(StoreError::Invalid(format!("{to} already exists")));
        }
        if let Some(dir) = dst.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::rename(src, dst)?;
        Ok(())
    }

    /// Moves an item into `folder`, keeping its name.
    pub fn move_to(&self, rel: &str, folder: &str) -> StoreResult<()> {
        let name = Path::new(rel).file_name().ok_or_else(|| StoreError::InvalidPath(rel.to_owned()))?.to_string_lossy().into_owned();
        self.rename(rel, &format!("{folder}/{name}"))
    }

    /// Lists folders and queries.
    pub fn list(&self, sort: QuerySort) -> StoreResult<Vec<QueryEntry>> {
        let mut out = Vec::new();
        if self.root.exists() {
            self.walk(&self.root, "", &mut out)?;
        }
        match sort {
            QuerySort::Name => out.sort_by_key(|e| e.path.to_lowercase()),
            QuerySort::Created => out.sort_by_key(|e| (!e.is_folder, std::cmp::Reverse(e.created_secs), e.path.to_lowercase())),
            QuerySort::Modified => out.sort_by_key(|e| (!e.is_folder, std::cmp::Reverse(e.modified_secs), e.path.to_lowercase())),
        }
        Ok(out)
    }

    /// Recursive directory walk.
    fn walk(&self, dir: &Path, prefix: &str, out: &mut Vec<QueryEntry>) -> StoreResult<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let file_name = e.file_name().to_string_lossy().into_owned();
            if file_name.starts_with('.') {
                continue;
            }
            let meta = e.metadata()?;
            let rel = if prefix.is_empty() { file_name.clone() } else { format!("{prefix}/{file_name}") };
            let secs = |t: std::io::Result<SystemTime>| t.ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            if meta.is_dir() {
                out.push(QueryEntry { path: rel.clone(), name: file_name, is_folder: true, modified_secs: secs(meta.modified()), created_secs: secs(meta.created().or(meta.modified())) });
                self.walk(&e.path(), &rel, out)?;
            } else if e.path().extension().is_some_and(|x| x == EXT) {
                let name = e.path().file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(QueryEntry { path: rel, name, is_folder: false, modified_secs: secs(meta.modified()), created_secs: secs(meta.created().or(meta.modified())) });
            }
        }
        Ok(())
    }

    /// Autocomplete keyword to SQL text map.
    pub fn keyword_map(&self) -> StoreResult<HashMap<String, String>> {
        let mut map = HashMap::new();
        for e in self.list(QuerySort::Name)?.into_iter().filter(|e| !e.is_folder) {
            let q = self.read(&e.path)?;
            for k in q.keywords {
                map.insert(k, q.sql.clone());
            }
        }
        Ok(map)
    }
}
