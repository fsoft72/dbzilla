//! Application side of the MCP server: connections, approvals and the activity log.

use crate::ui::connform::config_for;
use async_trait::async_trait;
use dbzilla_drivers::{driver_for, Conn};
use dbzilla_mcp::exec::{run_read_only, run_write};
use dbzilla_mcp::server::{Activity, ApprovalRequest, Backend, ConnInfo};
use dbzilla_store::connections::StoredConnection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// A connection opened through MCP, locked per connection so one slow query blocks only its own.
type SharedConn = Arc<tokio::sync::Mutex<Box<dyn Conn>>>;

/// Longest a client query may run, in seconds (SPEC 16).
pub const QUERY_TIMEOUT_SECS: u64 = 300;
/// How long the user has to answer an approval request, in seconds.
pub const APPROVAL_TIMEOUT_SECS: u64 = 120;
/// Maximum activity lines kept in memory.
pub const ACTIVITY_KEPT: usize = 200;

/// Resolves the password of a saved connection without user interaction.
pub type PasswordResolver = Arc<dyn Fn(&StoredConnection) -> Result<Option<String>, String> + Send + Sync>;

/// A write waiting for the user, with the channel carrying the answer.
pub struct PendingApproval {
    pub req: ApprovalRequest,
    pub answer: tokio::sync::oneshot::Sender<bool>,
}

/// State shared between the server tasks and the UI.
pub struct Shared {
    pub connections: Mutex<Vec<StoredConnection>>,
    pub activity: Mutex<Vec<Activity>>,
}

/// The backend handed to the MCP server.
pub struct AppBackend {
    pub shared: Arc<Shared>,
    resolver: PasswordResolver,
    conns: tokio::sync::Mutex<HashMap<String, SharedConn>>,
    approvals: Mutex<Sender<PendingApproval>>,
    activity_file: PathBuf,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl AppBackend {
    /// Creates the backend.
    pub fn new(shared: Arc<Shared>, resolver: PasswordResolver, approvals: Sender<PendingApproval>, activity_file: PathBuf, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { shared, resolver, conns: Default::default(), approvals: Mutex::new(approvals), activity_file, repaint }
    }

    /// The saved connection with an id.
    fn find(&self, id: &str) -> Result<StoredConnection, String> {
        self.shared.connections.lock().map_err(|e| e.to_string())?.iter().find(|c| c.id == id).cloned().ok_or_else(|| format!("unknown connection '{id}'"))
    }
}

#[async_trait]
impl Backend for AppBackend {
    async fn connections(&self) -> Vec<ConnInfo> {
        let open = self.conns.lock().await;
        let list = self.shared.connections.lock().map(|g| g.clone()).unwrap_or_default();
        list.into_iter().map(|c| ConnInfo { open: open.contains_key(&c.id), id: c.id, name: c.name, driver: c.driver }).collect()
    }

    async fn open(&self, id: &str) -> Result<(), String> {
        if self.conns.lock().await.contains_key(id) {
            return Ok(());
        }
        let c = self.find(id)?;
        let password = (self.resolver)(&c)?;
        let cfg = config_for(&c, password);
        let conn = driver_for(c.driver).map_err(|e| e.to_string())?.connect(&cfg).await.map_err(|e| format!("cannot connect: {e}"))?;
        self.conns.lock().await.insert(id.to_owned(), Arc::new(tokio::sync::Mutex::new(conn)));
        Ok(())
    }

    async fn disconnect(&self, id: &str) -> Result<(), String> {
        self.conns.lock().await.remove(id).map(|_| ()).ok_or_else(|| format!("the connection '{id}' is not open"))
    }

    async fn safe_level(&self, id: &str) -> dbzilla_core::safemode::SafeLevel {
        self.find(id).map(|c| c.safe_level).unwrap_or_default()
    }

    async fn run(&self, id: &str, sql: &str, write: bool) -> Result<Value, String> {
        let driver = self.find(id)?.driver;
        let conn = self.conns.lock().await.get(id).cloned().ok_or_else(|| format!("the connection '{id}' is not open"))?;
        let mut guard = conn.lock().await;
        let work = async {
            if write { run_write(guard.as_mut(), sql).await } else { run_read_only(guard.as_mut(), driver, sql).await }
        };
        match tokio::time::timeout(std::time::Duration::from_secs(QUERY_TIMEOUT_SECS), work).await {
            Ok(r) => r,
            Err(_) => {
                // the statement may still run on the server: drop this connection so the next call starts clean
                drop(guard);
                self.conns.lock().await.remove(id);
                Err(format!("the statement ran longer than {QUERY_TIMEOUT_SECS} seconds; the connection was closed, open it again"))
            }
        }
    }

    async fn approve(&self, req: ApprovalRequest) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sent = self.approvals.lock().map(|s| s.send(PendingApproval { req, answer: tx }).is_ok()).unwrap_or(false);
        if !sent {
            return false;
        }
        (self.repaint)();
        tokio::time::timeout(std::time::Duration::from_secs(APPROVAL_TIMEOUT_SECS), rx).await.ok().and_then(Result::ok).unwrap_or(false)
    }

    fn log(&self, entry: Activity) {
        let entry = Activity { detail: dbzilla_session::console::redact_sql(&entry.detail), ..entry };
        let line = serde_json::json!({"token":entry.token,"tool":entry.tool,"detail":entry.detail,"outcome":entry.outcome});
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        if let Ok(mut f) = opts.open(&self.activity_file) {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
        if let Ok(mut a) = self.shared.activity.lock() {
            a.insert(0, entry);
            a.truncate(ACTIVITY_KEPT);
        }
        (self.repaint)();
    }
}

/// What the running server was started with.
#[derive(Clone, PartialEq)]
struct Running {
    port: u16,
    approval_needed: bool,
}

/// Owns the server task, the tokens and the approvals waiting for the user.
pub struct McpHost {
    pub shared: Arc<Shared>,
    pub state: Option<Arc<dbzilla_mcp::server::McpState>>,
    pub pending: Vec<PendingApproval>,
    pub error: Option<String>,
    /// Secret of the token created last, shown once.
    pub new_secret: Option<String>,
    pub new_name: String,
    pub new_perm: dbzilla_mcp::perm::Permission,
    task: Option<(tokio::task::JoinHandle<()>, Running)>,
    /// Settings that failed to start: not retried until they change.
    failed: Option<Running>,
    approvals_rx: std::sync::mpsc::Receiver<PendingApproval>,
    approvals_tx: Sender<PendingApproval>,
    tokens_file: PathBuf,
    activity_file: PathBuf,
    resolver: PasswordResolver,
}

impl McpHost {
    /// Creates a stopped host.
    pub fn new(tokens_file: PathBuf, activity_file: PathBuf, resolver: PasswordResolver) -> Self {
        let (approvals_tx, approvals_rx) = std::sync::mpsc::channel();
        Self { shared: Arc::new(Shared { connections: Mutex::new(Vec::new()), activity: Mutex::new(Vec::new()) }), state: None, pending: Vec::new(), error: None, new_secret: None, new_name: String::new(), new_perm: dbzilla_mcp::perm::Permission::ReadOnly, task: None, failed: None, approvals_rx, approvals_tx, tokens_file, activity_file, resolver }
    }

    /// True while the server runs.
    pub fn running(&self) -> bool {
        self.task.is_some()
    }

    /// Starts or stops the server so it matches the settings.
    pub fn sync(&mut self, rt: &tokio::runtime::Handle, s: &dbzilla_store::settings::McpSettings, repaint: Arc<dyn Fn() + Send + Sync>) {
        let want = s.enabled.then_some(Running { port: s.port, approval_needed: s.approval_needed });
        if self.task.as_ref().map(|(_, r)| r) == want.as_ref() || (self.task.is_none() && self.failed.is_some() && self.failed == want) {
            return;
        }
        self.failed = None;
        if let Some((h, _)) = self.task.take() {
            h.abort();
        }
        if let Some(old) = self.state.take() {
            old.active.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        let Some(run) = want else { return };
        self.error = None;
        let tokens = match dbzilla_mcp::tokens::TokenStore::load(&self.tokens_file) {
            Ok(t) => t,
            Err(e) => {
                self.failed = Some(run);
                return self.error = Some(e);
            }
        };
        let backend = Arc::new(AppBackend::new(self.shared.clone(), self.resolver.clone(), self.approvals_tx.clone(), self.activity_file.clone(), repaint));
        let state = Arc::new(dbzilla_mcp::server::McpState { backend, tokens: Mutex::new(tokens), approval_needed: run.approval_needed, active: std::sync::atomic::AtomicBool::new(true) });
        match rt.block_on(dbzilla_mcp::server::serve(state.clone(), run.port)) {
            Ok(h) => {
                self.state = Some(state);
                self.task = Some((h, run));
            }
            Err(e) => {
                self.error = Some(format!("cannot listen on port {}: {e}", run.port));
                self.failed = Some(run);
            }
        }
    }

    /// Moves the approvals sent by the server into the pending list.
    pub fn poll(&mut self) {
        while let Ok(p) = self.approvals_rx.try_recv() {
            self.pending.push(p);
        }
        // requests nobody answered in time are dropped from the dialog
        self.pending.retain(|p| !p.answer.is_closed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::dialect::DriverId;
    use dbzilla_mcp::perm::Permission;
    use dbzilla_store::settings::McpSettings;
    use serde_json::json;

    /// POSTs a JSON-RPC call and returns the tool result text.
    fn call(port: u16, token: &str, tool: &str, args: Value) -> (bool, String) {
        let mut res = ureq::post(format!("http://127.0.0.1:{port}/mcp")).header("Authorization", format!("Bearer {token}")).send_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}})).unwrap();
        let v: Value = res.body_mut().read_json().unwrap();
        (v["result"]["isError"].as_bool().unwrap(), v["result"]["content"][0]["text"].as_str().unwrap().to_owned())
    }

    /// A real server on a saved SQLite connection: reads run, a write waits for the user and the log fills.
    #[test]
    fn host_serves_saved_connections_with_approval() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("d.sqlite3");
        rusqlite::Connection::open(&db).unwrap().execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut host = McpHost::new(dir.path().join("tokens.json"), dir.path().join("activity.jsonl"), Arc::new(|_| Ok(None)));
        let mut conn = StoredConnection::new("Local", DriverId::Sqlite);
        conn.id = "c1".into();
        conn.database = db.to_string_lossy().into_owned();
        host.shared.connections.lock().unwrap().push(conn);
        host.sync(rt.handle(), &McpSettings { enabled: true, port, approval_needed: true, auto_token_stdio: true }, Arc::new(|| {}));
        assert!(host.running(), "{:?}", host.error);
        let token = host.state.as_ref().unwrap().tokens.lock().unwrap().create("t", Permission::Full).unwrap();
        assert!(!call(port, &token, "open_connection", json!({"connection_id":"c1"})).0);
        let (err, text) = call(port, &token, "execute_read_query", json!({"connection_id":"c1","sql":"SELECT id FROM t"}));
        assert!(!err && text.contains("[[1]]"), "{text}");
        let writer = std::thread::spawn({
            let token = token.clone();
            move || call(port, &token, "execute_query", json!({"connection_id":"c1","sql":"DELETE FROM t"}))
        });
        let started = std::time::Instant::now();
        while host.pending.is_empty() {
            host.poll();
            assert!(started.elapsed().as_secs() < 10, "the approval never arrived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(host.pending[0].req.sql, "DELETE FROM t");
        host.pending.remove(0).answer.send(true).unwrap();
        let (err, text) = writer.join().unwrap();
        assert!(!err, "{text}");
        let left: i64 = rusqlite::Connection::open(&db).unwrap().query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        assert_eq!(host.shared.activity.lock().unwrap().len(), 3);
        assert!(std::fs::read_to_string(dir.path().join("activity.jsonl")).unwrap().lines().count() == 3);
        host.sync(rt.handle(), &McpSettings { enabled: false, port, approval_needed: true, auto_token_stdio: true }, Arc::new(|| {}));
        assert!(!host.running());
    }

    /// Approvals whose client gave up are removed from the pending list.
    #[test]
    fn abandoned_approvals_disappear() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = McpHost::new(dir.path().join("t.json"), dir.path().join("a.jsonl"), Arc::new(|_| Ok(None)));
        let (tx, rx) = tokio::sync::oneshot::channel();
        host.approvals_tx.send(PendingApproval { req: ApprovalRequest { token: "t".into(), connection: "c".into(), sql: "DELETE FROM t".into(), kind: dbzilla_core::safemode::StatementKind::Write }, answer: tx }).unwrap();
        host.poll();
        assert_eq!(host.pending.len(), 1);
        drop(rx);
        host.poll();
        assert!(host.pending.is_empty());
    }

    /// A port that cannot be bound is reported once and not retried every frame.
    #[test]
    fn failed_bind_is_not_retried_until_settings_change() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut host = McpHost::new(dir.path().join("t.json"), dir.path().join("a.jsonl"), Arc::new(|_| Ok(None)));
        let on = McpSettings { enabled: true, port, approval_needed: true, auto_token_stdio: true };
        host.sync(rt.handle(), &on, Arc::new(|| {}));
        assert!(host.error.is_some() && !host.running());
        host.error = None;
        host.sync(rt.handle(), &on, Arc::new(|| {}));
        assert!(host.error.is_none(), "same settings: no new attempt");
        drop(taken);
        host.sync(rt.handle(), &McpSettings { port, approval_needed: false, ..on }, Arc::new(|| {}));
        assert!(host.running(), "changed settings try again: {:?}", host.error);
    }
}

