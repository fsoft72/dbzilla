//! Streamable HTTP MCP endpoint: JSON-RPC 2.0 over `POST /mcp` with Bearer tokens (SPEC 16).

use crate::perm::Permission;
use crate::tokens::TokenStore;
use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use dbzilla_core::dialect::DriverId;
use dbzilla_core::safemode::{classify, StatementKind};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// MCP protocol revision announced by the server.
pub const PROTOCOL_VERSION: &str = "2025-03-26";

/// A saved connection as seen by clients.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnInfo {
    pub id: String,
    pub name: String,
    pub driver: DriverId,
    pub open: bool,
}

/// A write that needs the user's approval.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub token: String,
    pub connection: String,
    pub sql: String,
    pub kind: StatementKind,
}

/// One line of the activity log.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub token: String,
    pub tool: String,
    pub detail: String,
    pub outcome: String,
}

/// What the application provides to the server.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Saved connections.
    async fn connections(&self) -> Vec<ConnInfo>;
    /// Opens (or reuses) a connection.
    async fn open(&self, id: &str) -> Result<(), String>;
    /// Closes a connection opened through MCP.
    async fn disconnect(&self, id: &str) -> Result<(), String>;
    /// Runs a statement: `write` selects execution instead of a result set.
    async fn run(&self, id: &str, sql: &str, write: bool) -> Result<Value, String>;
    /// Asks the user to approve a write; true when approved.
    async fn approve(&self, req: ApprovalRequest) -> bool;
    /// Records an activity line.
    fn log(&self, entry: Activity);
}

/// Shared server state.
pub struct McpState {
    pub backend: Arc<dyn Backend>,
    pub tokens: Mutex<TokenStore>,
    pub approval_needed: bool,
}

/// The router serving `/mcp`.
pub fn router(state: Arc<McpState>) -> Router {
    Router::new().route("/mcp", post(handle)).with_state(state)
}

/// Tool descriptions for `tools/list`.
fn tools() -> Value {
    let sql = json!({"type":"object","properties":{"connection_id":{"type":"string"},"sql":{"type":"string"}},"required":["connection_id","sql"]});
    let id = json!({"type":"object","properties":{"connection_id":{"type":"string"}},"required":["connection_id"]});
    json!([
        {"name":"list_connections","description":"List the saved connections","inputSchema":{"type":"object","properties":{}}},
        {"name":"list_open_connections","description":"List the connections that are currently open","inputSchema":{"type":"object","properties":{}}},
        {"name":"open_connection","description":"Open a saved connection","inputSchema":id},
        {"name":"disconnect","description":"Close a connection","inputSchema":id},
        {"name":"execute_read_query","description":"Run a read-only query (at most 1000 rows)","inputSchema":sql},
        {"name":"execute_query","description":"Run a statement; writes need the right token permission and may need approval","inputSchema":sql},
    ])
}

/// JSON-RPC success envelope.
fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

/// JSON-RPC error envelope.
fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// Tool result carrying text; `is_error` marks failures the model should see.
fn tool_result(text: String, is_error: bool) -> Value {
    json!({"content":[{"type":"text","text":text}],"isError":is_error})
}

/// Reads the Bearer token of a request.
fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")
}

/// HTTP entry point.
async fn handle(State(st): State<Arc<McpState>>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let Some(rec) = bearer(&headers).and_then(|s| st.tokens.lock().ok()?.verify(s).cloned()) else {
        return (StatusCode::UNAUTHORIZED, "a valid Bearer token is required").into_response();
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    if req.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let body = match method {
        "initialize" => ok(&id, json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"dbzilla","version":env!("CARGO_PKG_VERSION")}})),
        "ping" => ok(&id, json!({})),
        "tools/list" => ok(&id, json!({"tools":tools()})),
        "tools/call" => {
            let name = req.pointer("/params/name").and_then(Value::as_str).unwrap_or("").to_owned();
            let args = req.pointer("/params/arguments").cloned().unwrap_or(json!({}));
            let result = call_tool(&st, &rec.name, rec.permission, &name, &args).await;
            ok(&id, match result {
                Ok(v) => tool_result(v.to_string(), false),
                Err(e) => tool_result(e, true),
            })
        }
        other => rpc_err(&id, -32601, &format!("unknown method '{other}'")),
    };
    Json(body).into_response()
}

/// String argument of a tool call.
fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("the argument '{key}' is required"))
}

/// Runs one tool and logs it.
async fn call_tool(st: &McpState, token: &str, perm: Permission, tool: &str, args: &Value) -> Result<Value, String> {
    let result = dispatch(st, token, perm, tool, args).await;
    let detail = args.get("sql").or_else(|| args.get("connection_id")).and_then(Value::as_str).unwrap_or("").to_owned();
    st.backend.log(Activity { token: token.to_owned(), tool: tool.to_owned(), detail, outcome: result.as_ref().map_or_else(|e| format!("error: {e}"), |_| "ok".into()) });
    result
}

/// Tool implementations.
async fn dispatch(st: &McpState, token: &str, perm: Permission, tool: &str, args: &Value) -> Result<Value, String> {
    let list = |open_only: bool, conns: Vec<ConnInfo>| -> Value {
        Value::Array(conns.into_iter().filter(|c| !open_only || c.open).map(|c| json!({"id":c.id,"name":c.name,"driver":format!("{:?}", c.driver),"open":c.open})).collect())
    };
    match tool {
        "list_connections" => Ok(list(false, st.backend.connections().await)),
        "list_open_connections" => Ok(list(true, st.backend.connections().await)),
        "open_connection" => {
            let id = arg(args, "connection_id")?;
            st.backend.open(id).await?;
            Ok(json!({"opened":id}))
        }
        "disconnect" => {
            let id = arg(args, "connection_id")?;
            st.backend.disconnect(id).await?;
            Ok(json!({"closed":id}))
        }
        "execute_read_query" | "execute_query" => {
            let (id, sql) = (arg(args, "connection_id")?, arg(args, "sql")?);
            let conns = st.backend.connections().await;
            let info = conns.iter().find(|c| c.id == id).ok_or_else(|| format!("unknown connection '{id}'"))?;
            if !info.open {
                return Err(format!("the connection '{id}' is not open; call open_connection first"));
            }
            let kind = classify(sql, info.driver);
            if tool == "execute_read_query" && kind != StatementKind::ReadOnly {
                return Err("execute_read_query only runs read-only statements; use execute_query".into());
            }
            if !perm.allows(kind) {
                return Err(format!("this token ({}) is not allowed to run {kind:?} statements", perm.label()));
            }
            if kind != StatementKind::ReadOnly && st.approval_needed && !st.backend.approve(ApprovalRequest { token: token.to_owned(), connection: id.to_owned(), sql: sql.to_owned(), kind }).await {
                return Err("the user denied the statement".into());
            }
            st.backend.run(id, sql, kind != StatementKind::ReadOnly).await
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

/// Binds `127.0.0.1:port` and serves until the task is dropped.
pub async fn serve(state: Arc<McpState>, port: u16) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    Ok(tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    }))
}
