//! The MCP endpoint over real HTTP with a SQLite backed fake application.

use async_trait::async_trait;
use dbzilla_core::dialect::DriverId;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, SslMode};
use dbzilla_mcp::exec::{run_read_only, run_write};
use dbzilla_mcp::perm::Permission;
use dbzilla_mcp::server::*;
use dbzilla_mcp::tokens::TokenStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct App {
    path: String,
    conns: tokio::sync::Mutex<HashMap<String, Box<dyn Conn>>>,
    approve: bool,
    level: dbzilla_core::safemode::SafeLevel,
    approvals: Mutex<Vec<String>>,
    log: Mutex<Vec<Activity>>,
}

#[async_trait]
impl Backend for App {
    async fn connections(&self) -> Vec<ConnInfo> {
        vec![ConnInfo { id: "db1".into(), name: "Test".into(), driver: DriverId::Sqlite, open: self.conns.lock().await.contains_key("db1") }]
    }
    async fn open(&self, id: &str) -> Result<(), String> {
        if id != "db1" {
            return Err(format!("unknown connection '{id}'"));
        }
        let cfg = ConnectConfig { driver: DriverId::Sqlite, host: String::new(), port: None, user: String::new(), password: None, database: self.path.clone(), ssl: SslMode::Disabled, bootstrap: vec![] };
        let c = driver_for(DriverId::Sqlite).unwrap().connect(&cfg).await.map_err(|e| e.to_string())?;
        self.conns.lock().await.insert(id.into(), c);
        Ok(())
    }
    async fn disconnect(&self, id: &str) -> Result<(), String> {
        self.conns.lock().await.remove(id).map(|_| ()).ok_or_else(|| "not open".into())
    }
    async fn run(&self, id: &str, sql: &str, write: bool) -> Result<Value, String> {
        let mut g = self.conns.lock().await;
        let c = g.get_mut(id).ok_or("not open")?;
        if write { run_write(c.as_mut(), sql).await } else { run_read_only(c.as_mut(), DriverId::Sqlite, sql).await }
    }
    async fn approve(&self, req: ApprovalRequest) -> bool {
        self.approvals.lock().unwrap().push(req.sql);
        self.approve
    }
    async fn safe_level(&self, _id: &str) -> dbzilla_core::safemode::SafeLevel {
        self.level
    }
    fn log(&self, entry: Activity) {
        self.log.lock().unwrap().push(entry);
    }
}

struct Fixture {
    port: u16,
    app: Arc<App>,
    state: Arc<McpState>,
    secrets: HashMap<&'static str, String>,
    _dir: tempfile::TempDir,
}

async fn start(approval_needed: bool, approve: bool) -> Fixture {
    start_at(approval_needed, approve, dbzilla_core::safemode::SafeLevel::Silent).await
}

async fn start_at(approval_needed: bool, approve: bool, level: dbzilla_core::safemode::SafeLevel) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.sqlite3");
    rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1,'a'),(2,'b');").unwrap();
    let mut tokens = TokenStore::load(&dir.path().join("tokens.json")).unwrap();
    let secrets = HashMap::from([("ro", tokens.create("ro", Permission::ReadOnly).unwrap()), ("rw", tokens.create("rw", Permission::ReadWrite).unwrap()), ("full", tokens.create("full", Permission::Full).unwrap())]);
    let app = Arc::new(App { path: db.to_string_lossy().into_owned(), conns: Default::default(), approve, level, approvals: Default::default(), log: Default::default() });
    let state = Arc::new(McpState { backend: app.clone(), tokens: Mutex::new(tokens), approval_needed, active: std::sync::atomic::AtomicBool::new(true) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = state.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    Fixture { port, app, state, secrets, _dir: dir }
}

async fn rpc(f: &Fixture, token: Option<&str>, body: Value) -> (u16, Value) {
    let port = f.port;
    let token = token.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::post(format!("http://127.0.0.1:{port}/mcp")).config().http_status_as_error(false).build();
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let mut res = req.send_json(body).unwrap();
        let status = res.status().as_u16();
        let v = res.body_mut().read_json::<Value>().unwrap_or(Value::Null);
        (status, v)
    })
    .await
    .unwrap()
}

async fn call(f: &Fixture, who: &str, tool: &str, args: Value) -> (bool, Value) {
    let (_, v) = rpc(f, Some(&f.secrets[who]), json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}})).await;
    let r = &v["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("").to_owned();
    (r["isError"].as_bool().unwrap_or(true), serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[tokio::test]
async fn auth_and_protocol() {
    let f = start(false, true).await;
    assert_eq!(rpc(&f, None, json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await.0, 401);
    assert_eq!(rpc(&f, Some("dbz_wrong"), json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await.0, 401);
    let (s, v) = rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})).await;
    assert_eq!(s, 200);
    assert_eq!(v["result"]["serverInfo"]["name"], "dbzilla");
    let (_, v) = rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
    assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 6);
    let (_, v) = rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","id":3,"method":"nope"})).await;
    assert_eq!(v["error"]["code"], -32601);
    let (s, _) = rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
    assert_eq!(s, 202);
}

#[tokio::test]
async fn read_only_token_reads_but_never_writes() {
    let f = start(false, true).await;
    let (err, v) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"SELECT 1"})).await;
    assert!(err && v.as_str().unwrap().contains("not open"), "{v}");
    assert!(!call(&f, "ro", "open_connection", json!({"connection_id":"db1"})).await.0);
    let (err, v) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"SELECT id, name FROM t ORDER BY id"})).await;
    assert!(!err);
    assert_eq!(v["rows"], json!([[1, "a"], [2, "b"]]));
    assert_eq!(v["columns"], json!(["id", "name"]));
    for tool in ["execute_read_query", "execute_query"] {
        let (err, _) = call(&f, "ro", tool, json!({"connection_id":"db1","sql":"DELETE FROM t"})).await;
        assert!(err, "{tool} must refuse a write for a read-only token");
    }
    let (_, v) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"SELECT count(*) FROM t"})).await;
    assert_eq!(v["rows"], json!([[2]]));
}

#[tokio::test]
async fn read_write_and_full_permissions() {
    let f = start(false, true).await;
    call(&f, "full", "open_connection", json!({"connection_id":"db1"})).await;
    let (err, v) = call(&f, "rw", "execute_query", json!({"connection_id":"db1","sql":"INSERT INTO t VALUES (3,'c')"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["affected"], 1);
    let (err, _) = call(&f, "rw", "execute_query", json!({"connection_id":"db1","sql":"DROP TABLE t"})).await;
    assert!(err, "DDL needs full access");
    let (err, _) = call(&f, "full", "execute_query", json!({"connection_id":"db1","sql":"CREATE TABLE u (x INTEGER)"})).await;
    assert!(!err);
    let (_, v) = call(&f, "ro", "list_open_connections", json!({})).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert!(!call(&f, "full", "disconnect", json!({"connection_id":"db1"})).await.0);
    let (_, v) = call(&f, "ro", "list_open_connections", json!({})).await;
    assert!(v.as_array().unwrap().is_empty());
    let (_, v) = call(&f, "ro", "list_connections", json!({})).await;
    assert_eq!(v[0]["name"], "Test");
}

#[tokio::test]
async fn writes_wait_for_approval_and_denial_blocks_them() {
    let f = start(true, false).await;
    call(&f, "full", "open_connection", json!({"connection_id":"db1"})).await;
    let (err, v) = call(&f, "full", "execute_query", json!({"connection_id":"db1","sql":"DELETE FROM t"})).await;
    assert!(err && v.as_str().unwrap().contains("denied"), "{v}");
    assert_eq!(f.app.approvals.lock().unwrap().as_slice(), ["DELETE FROM t"]);
    let (_, v) = call(&f, "full", "execute_read_query", json!({"connection_id":"db1","sql":"SELECT count(*) FROM t"})).await;
    assert_eq!(v["rows"], json!([[2]]), "the denied delete never ran, and reads need no approval");
    assert_eq!(f.app.approvals.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn activity_is_logged_and_results_are_capped() {
    let f = start(false, true).await;
    call(&f, "ro", "open_connection", json!({"connection_id":"db1"})).await;
    call(&f, "ro", "execute_query", json!({"connection_id":"db1","sql":"DELETE FROM t"})).await;
    let (_, v) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 5000) SELECT x FROM c"})).await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1000);
    assert_eq!(v["truncated"], true);
    let log = f.app.log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert!(log[1].outcome.starts_with("error") && log[1].detail == "DELETE FROM t" && log[1].token == "ro");
}

#[tokio::test]
async fn stdio_bridge_forwards_requests() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let f = start(false, true).await;
    let token = f.secrets["ro"].clone();
    let port = f.port;
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dbzilla-mcp-bridge")).args(["--port", &port.to_string()]).env("DBZILLA_MCP_TOKEN", token).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#).unwrap();
        drop(stdin);
        let mut text = String::new();
        child.stdout.take().unwrap().read_to_string(&mut text).unwrap();
        child.wait().unwrap();
        text
    })
    .await
    .unwrap();
    assert_eq!(out.lines().count(), 1, "only the request with an id gets a reply: {out}");
    assert!(out.contains(r#""result":{}"#));
}

#[tokio::test]
async fn review_fixes_pragma_multi_statement_and_query_only_reset() {
    let f = start(false, true).await;
    call(&f, "full", "open_connection", json!({"connection_id":"db1"})).await;
    let (err, _) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"PRAGMA user_version(7)"})).await;
    assert!(err, "a PRAGMA with an argument is not read-only");
    let (err, _) = call(&f, "rw", "execute_query", json!({"connection_id":"db1","sql":"UPDATE t SET name='x'; DROP TABLE t"})).await;
    assert!(err, "a script containing DDL needs full access");
    let (_, v) = call(&f, "ro", "execute_read_query", json!({"connection_id":"db1","sql":"SELECT count(*) FROM t"})).await;
    assert_eq!(v["rows"], json!([[2]]));
    let (err, v) = call(&f, "rw", "execute_query", json!({"connection_id":"db1","sql":"INSERT INTO t VALUES (9,'z')"})).await;
    assert!(!err, "query_only was switched off again after the read: {v}");
}

#[tokio::test]
async fn stopped_server_refuses_open_connections() {
    let f = start(false, true).await;
    assert_eq!(rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await.0, 200);
    f.state.active.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(rpc(&f, Some(&f.secrets["ro"]), json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await.0, 503);
}

#[tokio::test]
async fn safe_mode_of_the_connection_applies_to_mcp() {
    use dbzilla_core::safemode::SafeLevel;
    let f = start_at(false, true, SafeLevel::Alert1).await;
    call(&f, "full", "open_connection", json!({"connection_id":"db1"})).await;
    let (err, _) = call(&f, "full", "execute_query", json!({"connection_id":"db1","sql":"DELETE FROM t"})).await;
    assert!(!err);
    assert_eq!(f.app.approvals.lock().unwrap().len(), 1, "Alert levels force the approval even when it is switched off");
    let g = start_at(false, true, SafeLevel::Safe2).await;
    call(&g, "full", "open_connection", json!({"connection_id":"db1"})).await;
    let (err, v) = call(&g, "full", "execute_query", json!({"connection_id":"db1","sql":"DELETE FROM t"})).await;
    assert!(err && v.as_str().unwrap().contains("password"), "{v}");
}
