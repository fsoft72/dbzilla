//! Store tests: atomic files, settings, connections, tags, history, saved queries, secrets.

use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::SslMode;
use dbzilla_core::safemode::SafeLevel;
use dbzilla_core::url::parse_url;
use dbzilla_store::atomic::{atomic_write, load_json};
use dbzilla_store::connections::{ConnectionStore, PasswordMode, StoredConnection};
use dbzilla_store::history::{HistoryEntry, HistoryKind, HistoryStore};
use dbzilla_store::paths::AppPaths;
use dbzilla_store::queries::{QueryStore, QuerySort};
use dbzilla_store::secrets::{secret_key, MemorySecretStore, SecretStore};
use dbzilla_store::settings::Settings;
use dbzilla_store::tags::TagStore;
use std::fs;

fn paths() -> (tempfile::TempDir, AppPaths) {
    let dir = tempfile::tempdir().unwrap();
    let p = AppPaths::new(dir.path().to_path_buf());
    (dir, p)
}

fn conn(name: &str) -> StoredConnection {
    StoredConnection::new(name, DriverId::Postgres)
}

#[test]
fn atomic_write_replaces_and_leaves_no_temp_files() {
    let (dir, _) = paths();
    let f = dir.path().join("sub").join("a.json");
    atomic_write(&f, b"one").unwrap();
    atomic_write(&f, b"two").unwrap();
    assert_eq!(fs::read_to_string(&f).unwrap(), "two");
    let names: Vec<String> = fs::read_dir(f.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["a.json"]);
}

#[test]
fn corrupt_file_is_backed_up_and_defaults_returned_with_warning() {
    let (_d, p) = paths();
    fs::create_dir_all(p.root()).unwrap();
    fs::write(p.settings_file(), "{ not json").unwrap();
    let loaded = Settings::load(&p);
    assert_eq!(loaded.value, Settings::default());
    assert!(loaded.warning.as_deref().unwrap_or("").contains("settings.json"));
    assert!(p.root().join("settings.json.bak").exists());
    assert!(!p.settings_file().exists() || fs::read_to_string(p.settings_file()).is_ok());
    fs::write(p.settings_file(), "{ still bad").unwrap();
    let again = Settings::load(&p);
    assert!(again.warning.is_some());
    assert!(p.root().join("settings.json.bak1").exists(), "an existing backup is never overwritten");
}

#[test]
fn newer_schema_is_treated_as_unreadable() {
    let (_d, p) = paths();
    fs::create_dir_all(p.root()).unwrap();
    fs::write(p.settings_file(), r#"{"schema_version": 999}"#).unwrap();
    let loaded: dbzilla_store::atomic::LoadResult<Settings> = load_json(&p.settings_file(), 1);
    assert!(loaded.warning.is_some());
}

#[test]
fn settings_defaults_match_the_spec() {
    let s = Settings::default();
    assert_eq!(s.page_size, 300);
    assert_eq!(s.keep_alive_secs, 30);
    assert!(s.keep_alive);
    assert_eq!(s.query_timeout_secs, 300);
    assert_eq!(s.console_max_entries, 1000);
    assert_eq!(s.default_safe_level, SafeLevel::Silent);
    assert_eq!(s.mcp.port, 51003);
    assert!(s.mcp.approval_needed);
    assert!(s.alternating_rows);
    assert!(s.history_enabled);
    assert_eq!(s.editor_font_size, 12.0);
}

#[test]
fn settings_round_trip_and_tolerate_missing_and_unknown_fields() {
    let (_d, p) = paths();
    let s = Settings { page_size: 500, ..Settings::default() };
    s.save(&p).unwrap();
    assert_eq!(Settings::load(&p).value.page_size, 500);
    fs::write(p.settings_file(), r#"{"schema_version":1,"page_size":42,"future_option":true}"#).unwrap();
    let loaded = Settings::load(&p);
    assert!(loaded.warning.is_none());
    assert_eq!(loaded.value.page_size, 42);
    assert_eq!(loaded.value.keep_alive_secs, 30);
}

#[test]
fn connections_crud_duplicate_and_no_secrets_in_the_file() {
    let (_d, p) = paths();
    let mut store = ConnectionStore::load(&p).value;
    let mut c = conn("Prod");
    c.host = "db.example.com".into();
    c.user = "admin".into();
    c.password_mode = PasswordMode::Keychain;
    let id = c.id.clone();
    store.upsert(c);
    store.save(&p).unwrap();
    let raw = fs::read_to_string(p.connections_file()).unwrap();
    assert!(!raw.to_lowercase().contains("secret") && !raw.contains("\"password\""), "{raw}");
    let mut store = ConnectionStore::load(&p).value;
    assert_eq!(store.get(&id).unwrap().host, "db.example.com");
    let copy_id = store.duplicate(&id).unwrap();
    assert_ne!(copy_id, id);
    assert_eq!(store.get(&copy_id).unwrap().name, "Prod copy");
    assert_eq!(store.list().len(), 2);
    assert!(store.delete(&id));
    assert!(!store.delete(&id));
    assert_eq!(store.list().len(), 1);
}

#[test]
fn deleting_a_group_deletes_its_connections_and_reports_ids() {
    let (_d, p) = paths();
    let mut store = ConnectionStore::load(&p).value;
    let g = store.add_group("Work", None);
    let mut a = conn("A");
    a.group = Some(g.clone());
    let mut b = conn("B");
    b.group = Some(g.clone());
    let free = conn("Free");
    let (aid, bid) = (a.id.clone(), b.id.clone());
    store.upsert(a);
    store.upsert(b);
    store.upsert(free);
    let mut removed = store.delete_group(&g);
    removed.sort();
    let mut expected = vec![aid, bid];
    expected.sort();
    assert_eq!(removed, expected);
    assert_eq!(store.list().len(), 1);
    assert!(store.groups().is_empty());
}

#[test]
fn connection_spec_conversion_round_trips() {
    let spec = parse_url("postgresql://admin:pw@db:5433/shop?name=Shop&env=production&statusColor=FF0000&safeModeLevel=3&tLSMode=2").unwrap();
    let c = StoredConnection::from_spec(&spec);
    assert_eq!((c.name.as_str(), c.host.as_str(), c.port, c.database.as_str()), ("Shop", "db", Some(5433), "shop"));
    assert_eq!(c.tag.as_deref(), Some("production"));
    assert_eq!(c.safe_level, SafeLevel::Safe1);
    assert_eq!(c.ssl, SslMode::Required);
    let back = c.to_spec(Some("pw".into()));
    assert_eq!(back.password.as_deref(), Some("pw"));
    assert_eq!(back.user, "admin");
}

#[test]
fn tags_defaults_custom_and_colours() {
    let (_d, p) = paths();
    let mut tags = TagStore::load(&p).value;
    let names: Vec<String> = tags.all().iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, vec!["local", "testing", "development", "staging", "production"]);
    tags.add("qa", "112233").unwrap();
    assert!(tags.add("qa", "112233").is_err(), "duplicates are refused");
    assert!(tags.remove("production").is_err(), "default tags cannot be removed");
    tags.set_color("production", "ABCDEF");
    assert_eq!(tags.color("production").as_deref(), Some("ABCDEF"));
    tags.reset_color("production");
    assert_ne!(tags.color("production").as_deref(), Some("ABCDEF"));
    tags.remove("qa").unwrap();
    tags.save(&p).unwrap();
    assert_eq!(TagStore::load(&p).value.all().len(), 5);
}

fn entry(sql: &str, ts: i64) -> HistoryEntry {
    HistoryEntry { ts, sql: sql.into(), kind: HistoryKind::Editor, duration_ms: Some(3), ok: true, database: None, original_csv: None }
}

#[test]
fn history_append_search_newest_first_clear() {
    let (_d, p) = paths();
    let h = HistoryStore::new(p.history_dir(), true);
    h.append("c1", &entry("SELECT 1", 1)).unwrap();
    h.append("c1", &entry("SELECT * FROM Orders", 2)).unwrap();
    h.append("c2", &entry("SELECT 2", 3)).unwrap();
    let all = h.search("c1", "", 100).unwrap();
    assert_eq!(all.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![2, 1]);
    assert_eq!(h.search("c1", "orders", 100).unwrap().len(), 1, "search is case-insensitive");
    assert_eq!(h.search("c1", "", 1).unwrap().len(), 1);
    let raw = fs::read_to_string(p.history_dir().join("c1.jsonl")).unwrap();
    assert_eq!(raw.lines().count(), 2);
    h.clear("c1").unwrap();
    assert!(h.search("c1", "", 10).unwrap().is_empty());
    assert_eq!(h.search("c2", "", 10).unwrap().len(), 1);
}

#[test]
fn history_disabled_writes_nothing_and_huge_statements_are_capped() {
    let (_d, p) = paths();
    let off = HistoryStore::new(p.history_dir(), false);
    off.append("c1", &entry("SELECT 1", 1)).unwrap();
    assert!(!p.history_dir().join("c1.jsonl").exists());
    let on = HistoryStore::new(p.history_dir(), true);
    on.append("c1", &entry(&"x".repeat(2_000_000), 1)).unwrap();
    let got = on.search("c1", "", 1).unwrap();
    assert!(got[0].sql.len() < 1_100_000);
    assert!(got[0].sql.ends_with("[truncated]"));
}

#[test]
fn history_survives_a_corrupt_line() {
    let (_d, p) = paths();
    let h = HistoryStore::new(p.history_dir(), true);
    h.append("c1", &entry("SELECT 1", 1)).unwrap();
    let f = p.history_dir().join("c1.jsonl");
    let mut text = fs::read_to_string(&f).unwrap();
    text.push_str("garbage\n");
    fs::write(&f, text).unwrap();
    h.append("c1", &entry("SELECT 2", 2)).unwrap();
    assert_eq!(h.search("c1", "", 10).unwrap().len(), 2);
}

#[test]
fn saved_queries_folders_keywords_sorting() {
    let (_d, p) = paths();
    let q = QueryStore::new(p.queries_dir());
    q.save("zeta.sql", &[], "SELECT 1").unwrap();
    q.save("reports/monthly.sql", &["rev".into(), "month".into()], "SELECT sum(x) FROM t").unwrap();
    q.create_folder("empty").unwrap();
    let list = q.list(QuerySort::Name).unwrap();
    let paths_: Vec<&str> = list.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths_, vec!["empty", "reports", "reports/monthly.sql", "zeta.sql"]);
    let m = q.read("reports/monthly.sql").unwrap();
    assert_eq!(m.keywords, vec!["rev", "month"]);
    assert_eq!(m.sql, "SELECT sum(x) FROM t");
    assert_eq!(m.name, "monthly");
    q.rename("zeta.sql", "alpha.sql").unwrap();
    assert!(q.read("alpha.sql").is_ok() && q.read("zeta.sql").is_err());
    q.move_to("alpha.sql", "reports").unwrap();
    assert!(q.read("reports/alpha.sql").is_ok());
    q.delete("reports/alpha.sql").unwrap();
    assert!(q.read("reports/alpha.sql").is_err());
    assert_eq!(q.keyword_map().unwrap().get("rev").map(String::as_str), Some("SELECT sum(x) FROM t"));
}

#[test]
fn saved_queries_reject_paths_outside_the_root() {
    let (_d, p) = paths();
    let q = QueryStore::new(p.queries_dir());
    for bad in ["../x.sql", "/etc/passwd", "a/../../b.sql", "a/..", ""] {
        assert!(q.save(bad, &[], "x").is_err(), "{bad} must be refused");
        assert!(q.read(bad).is_err(), "{bad} must be refused");
        assert!(q.delete(bad).is_err(), "{bad} must be refused");
    }
}

#[test]
fn secrets_memory_store_and_key_format() {
    let s = MemorySecretStore::default();
    assert_eq!(s.get("k").unwrap(), None);
    s.set("k", "v").unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));
    s.delete("k").unwrap();
    assert_eq!(s.get("k").unwrap(), None);
    s.delete("k").unwrap();
    assert_eq!(secret_key("conn-1", "password"), "dbzilla/conn-1/password");
}
