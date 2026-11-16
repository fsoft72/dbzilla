//! Crash-safe JSON files: atomic writes and tolerant loading (NFR-07).

use crate::StoreResult;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Outcome of loading a file: the value plus an optional warning to show the user.
#[derive(Debug, Clone)]
pub struct LoadResult<T> {
    pub value: T,
    pub warning: Option<String>,
}

/// Writes `bytes` to a temp file next to `path`, syncs it and renames it over `path`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Serialises `value` as pretty JSON (with a trailing newline) and writes it atomically.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> StoreResult<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    atomic_write(path, text.as_bytes())?;
    Ok(())
}

/// Moves a bad file to the first free `.bak`, `.bak1`, `.bak2`... name.
fn backup(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let dir = path.parent()?;
    for i in 0.. {
        let suffix = if i == 0 { String::new() } else { i.to_string() };
        let target = dir.join(format!("{name}.bak{suffix}"));
        if !target.exists() {
            return fs::rename(path, &target).ok().map(|_| target);
        }
    }
    None
}

/// Loads a JSON file. A missing file gives the default silently; an unreadable, corrupt or too new
/// file is moved aside as a backup and the default is returned with a warning.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path, max_version: u64) -> LoadResult<T> {
    if !path.exists() {
        return LoadResult { value: T::default(), warning: None };
    }
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let fail = |reason: String| {
        let saved = backup(path).map(|b| b.display().to_string()).unwrap_or_else(|| "(backup failed)".into());
        LoadResult { value: T::default(), warning: Some(format!("{file} could not be used ({reason}). It was saved as {saved} and defaults are in use.")) }
    };
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return fail(e.to_string()),
    };
    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(j) => j,
        Err(e) => return fail(e.to_string()),
    };
    let version = json.get("schema_version").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if version > max_version {
        return fail(format!("it was written by a newer version, schema {version}"));
    }
    match serde_json::from_value(json) {
        Ok(value) => LoadResult { value, warning: None },
        Err(e) => fail(e.to_string()),
    }
}
