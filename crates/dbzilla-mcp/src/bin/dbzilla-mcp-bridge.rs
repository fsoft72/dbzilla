//! stdio to HTTP bridge: lets MCP clients that only speak stdio reach the DBZilla server.
//!
//! Usage: `DBZILLA_MCP_TOKEN=... dbzilla-mcp-bridge [--port 51003]`

use std::io::{BufRead, Write};

/// Reads JSON-RPC lines from stdin, forwards each to the server and prints the reply.
fn main() {
    let mut port = dbzilla_mcp::DEFAULT_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--port" {
            port = args.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| fail("--port needs a number"));
        }
    }
    let token = std::env::var("DBZILLA_MCP_TOKEN").unwrap_or_else(|_| fail("set DBZILLA_MCP_TOKEN to a token created in DBZilla"));
    let url = format!("http://127.0.0.1:{port}/mcp");
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap_or_else(|e| fail(&format!("cannot read stdin: {e}")));
        if line.trim().is_empty() {
            continue;
        }
        let mut res = match agent.post(&url).header("Authorization", format!("Bearer {token}")).header("Content-Type", "application/json").send(line.as_bytes()) {
            Ok(r) => r,
            Err(e) => fail(&format!("cannot reach DBZilla on port {port}: {e}. Is the MCP server enabled?")),
        };
        let status = res.status().as_u16();
        let body = res.body_mut().read_to_string().unwrap_or_default();
        if status == 401 {
            fail("the token was rejected");
        }
        if status == 202 || body.trim().is_empty() {
            continue;
        }
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", body.trim());
        let _ = out.flush();
    }
}

/// Prints an error and exits.
fn fail(msg: &str) -> ! {
    eprintln!("dbzilla-mcp-bridge: {msg}");
    std::process::exit(1);
}
