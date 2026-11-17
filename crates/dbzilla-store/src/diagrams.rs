//! Positions of the tables in the ER diagram, kept per connection, database and schema (SPEC 11.3).

use crate::atomic::{load_json, save_json};
use crate::StoreResult;
use dbzilla_core::diagram::DocLink;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Current file schema version.
pub const DIAGRAMS_VERSION: u32 = 1;

/// Table name to position (x, y) in diagram coordinates.
pub type Positions = BTreeMap<String, (f32, f32)>;

/// On-disk form: one set of positions per `server/database/schema` key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct DiagramFile {
    schema_version: u32,
    layouts: BTreeMap<String, Positions>,
    /// Links drawn by the user, per diagram key.
    links: BTreeMap<String, Vec<DocLink>>,
}

impl Default for DiagramFile {
    fn default() -> Self {
        Self { schema_version: DIAGRAMS_VERSION, layouts: BTreeMap::new(), links: BTreeMap::new() }
    }
}

/// Reads and writes `diagrams.json`.
#[derive(Debug, Clone)]
pub struct DiagramStore {
    path: PathBuf,
}

impl DiagramStore {
    /// A store backed by `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Key of a diagram: the server (see the caller), the database and the schema. It does not use the
    /// connection id, because connections opened from the command line get a new id at every start.
    pub fn key(server: &str, database: &str, schema: Option<&str>) -> String {
        format!("{server}/{database}/{}", schema.unwrap_or(""))
    }

    /// Saved positions for `key`; empty when there are none or the file is unusable.
    pub fn load(&self, key: &str) -> Positions {
        load_json::<DiagramFile>(&self.path, u64::from(DIAGRAMS_VERSION)).value.layouts.remove(key).unwrap_or_default()
    }

    /// Links drawn by the user for `key`; empty when there are none or the file is unusable.
    pub fn load_links(&self, key: &str) -> Vec<DocLink> {
        load_json::<DiagramFile>(&self.path, u64::from(DIAGRAMS_VERSION)).value.links.remove(key).unwrap_or_default()
    }

    /// Replaces the links saved for `key`, keeping the positions and the other diagrams.
    pub fn save_links(&self, key: &str, links: Vec<DocLink>) -> StoreResult<()> {
        let mut file = load_json::<DiagramFile>(&self.path, u64::from(DIAGRAMS_VERSION)).value;
        file.links.insert(key.to_owned(), links);
        save_json(&self.path, &file)
    }

    /// Replaces the positions saved for `key`, keeping the other diagrams.
    pub fn save(&self, key: &str, positions: Positions) -> StoreResult<()> {
        let mut file = load_json::<DiagramFile>(&self.path, u64::from(DIAGRAMS_VERSION)).value;
        file.layouts.insert(key.to_owned(), positions);
        save_json(&self.path, &file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(items: &[(&str, f32, f32)]) -> Positions {
        items.iter().map(|(n, x, y)| ((*n).to_owned(), (*x, *y))).collect()
    }

    #[test]
    fn missing_file_gives_no_positions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(DiagramStore::new(dir.path().join("diagrams.json")).load("c/db/").is_empty());
    }

    #[test]
    fn positions_round_trip_and_diagrams_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiagramStore::new(dir.path().join("diagrams.json"));
        let (a, b) = (DiagramStore::key("c1", "shop", Some("public")), DiagramStore::key("c1", "shop", Some("audit")));
        store.save(&a, pos(&[("orders", 10.0, 20.5)])).unwrap();
        store.save(&b, pos(&[("log", 1.0, 2.0)])).unwrap();
        assert_eq!(store.load(&a), pos(&[("orders", 10.0, 20.5)]));
        assert_eq!(store.load(&b), pos(&[("log", 1.0, 2.0)]));
        store.save(&a, pos(&[("orders", 99.0, 0.0)])).unwrap();
        assert_eq!(store.load(&a), pos(&[("orders", 99.0, 0.0)]));
        assert_eq!(store.load(&b), pos(&[("log", 1.0, 2.0)]));
    }

    #[test]
    fn links_round_trip_next_to_positions() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiagramStore::new(dir.path().join("diagrams.json"));
        let link = DocLink { from_table: "a".into(), from_column: "u".into(), to_table: "b".into(), to_column: "id".into() };
        store.save("k", pos(&[("a", 1.0, 2.0)])).unwrap();
        store.save_links("k", vec![link.clone()]).unwrap();
        store.save("k", pos(&[("a", 3.0, 4.0)])).unwrap();
        assert_eq!(store.load_links("k"), vec![link]);
        assert_eq!(store.load("k"), pos(&[("a", 3.0, 4.0)]));
        assert!(store.load_links("other").is_empty());
    }

    #[test]
    fn a_file_without_links_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagrams.json");
        std::fs::write(&path, r#"{"schema_version":1,"layouts":{"k":{"t":[1.0,2.0]}}}"#).unwrap();
        let store = DiagramStore::new(path);
        assert_eq!(store.load("k"), pos(&[("t", 1.0, 2.0)]));
        assert!(store.load_links("k").is_empty());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagrams.json");
        std::fs::write(&path, "{ not json").unwrap();
        let store = DiagramStore::new(path.clone());
        assert!(store.load("k").is_empty());
        assert!(dir.path().join("diagrams.json.bak").exists());
        store.save("k", pos(&[("t", 1.0, 1.0)])).unwrap();
        assert_eq!(store.load("k"), pos(&[("t", 1.0, 1.0)]));
    }
}
