//! Connection tags and their colours (SPEC FR-C-014).

use crate::atomic::{load_json, save_json, LoadResult};
use crate::paths::AppPaths;
use crate::{StoreError, StoreResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current tags schema version.
pub const TAGS_VERSION: u32 = 1;
/// Default tags with their default colours (hex, no `#`).
const DEFAULT_TAGS: [(&str, &str); 5] = [("local", "3DD68C"), ("testing", "F2B84B"), ("development", "8FB1F7"), ("staging", "F2994A"), ("production", "F0707F")];

/// A tag with its colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub color: String,
    pub builtin: bool,
}

/// On-disk form: custom tags and colour overrides of the default ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TagStore {
    schema_version: u32,
    custom: Vec<(String, String)>,
    overrides: BTreeMap<String, String>,
}

impl Default for TagStore {
    fn default() -> Self {
        Self { schema_version: TAGS_VERSION, custom: vec![], overrides: BTreeMap::new() }
    }
}

impl TagStore {
    /// Loads `tags.json`.
    pub fn load(paths: &AppPaths) -> LoadResult<TagStore> {
        load_json(&paths.tags_file(), u64::from(TAGS_VERSION))
    }

    /// Saves `tags.json` atomically.
    pub fn save(&self, paths: &AppPaths) -> StoreResult<()> {
        save_json(&paths.tags_file(), self)
    }

    /// Default tags first, then custom tags.
    pub fn all(&self) -> Vec<Tag> {
        let defaults = DEFAULT_TAGS.iter().map(|(n, c)| Tag { name: (*n).to_owned(), color: self.overrides.get(*n).cloned().unwrap_or_else(|| (*c).to_owned()), builtin: true });
        let custom = self.custom.iter().map(|(n, c)| Tag { name: n.clone(), color: c.clone(), builtin: false });
        defaults.chain(custom).collect()
    }

    /// Colour of a tag.
    pub fn color(&self, name: &str) -> Option<String> {
        self.all().into_iter().find(|t| t.name == name).map(|t| t.color)
    }

    /// Adds a custom tag; duplicates and empty names are refused.
    pub fn add(&mut self, name: &str, color: &str) -> StoreResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Invalid("a tag needs a name".into()));
        }
        if self.all().iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
            return Err(StoreError::Invalid(format!("the tag '{name}' already exists")));
        }
        self.custom.push((name.to_owned(), color.to_owned()));
        Ok(())
    }

    /// Removes a custom tag; default tags cannot be removed.
    pub fn remove(&mut self, name: &str) -> StoreResult<()> {
        if DEFAULT_TAGS.iter().any(|(n, _)| *n == name) {
            return Err(StoreError::Invalid(format!("the default tag '{name}' cannot be removed")));
        }
        self.custom.retain(|(n, _)| n != name);
        Ok(())
    }

    /// Changes the colour of any tag.
    pub fn set_color(&mut self, name: &str, color: &str) {
        if let Some(slot) = self.custom.iter_mut().find(|(n, _)| n == name) {
            slot.1 = color.to_owned();
        } else if DEFAULT_TAGS.iter().any(|(n, _)| *n == name) {
            self.overrides.insert(name.to_owned(), color.to_owned());
        }
    }

    /// Restores the default colour of a default tag.
    pub fn reset_color(&mut self, name: &str) {
        self.overrides.remove(name);
    }
}
