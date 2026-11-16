//! Shared helpers for Docker-backed integration tests.
#![allow(dead_code)]

use dbzilla_core::model::Row;
use dbzilla_drivers::{Conn, QueryOpts, QuerySummary, RowBatch};
use std::process::Command;
use std::time::Duration;
use tokio::sync::mpsc;

/// A running throwaway container with its mapped host port.
pub struct Container {
    pub id: String,
    pub port: u16,
}

/// True when the Docker daemon answers.
pub fn docker_available() -> bool {
    Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Removes every container carrying `label`.
pub fn cleanup(label: &str) {
    let out = Command::new("docker").args(["ps", "-aq", "--filter", &format!("label={label}")]).output();
    let Ok(out) = out else { return };
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect();
    if ids.is_empty() {
        return;
    }
    let _ = Command::new("docker").arg("rm").arg("-f").args(&ids).output();
}

/// Starts a container publishing `inner_port` on a random localhost port. None when Docker is missing.
pub fn start(label: &str, image: &str, env: &[(&str, &str)], inner_port: u16, args: &[&str]) -> Option<Container> {
    if !docker_available() {
        return None;
    }
    cleanup(label);
    let mut cmd = Command::new("docker");
    cmd.args(["run", "-d", "--rm", "--label", label, "-p", &format!("127.0.0.1::{inner_port}")]);
    for (k, v) in env {
        cmd.args(["-e", &format!("{k}={v}")]);
    }
    cmd.arg(image).args(args);
    let out = cmd.output().ok()?;
    assert!(out.status.success(), "docker run failed: {}", String::from_utf8_lossy(&out.stderr));
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let port_out = Command::new("docker").args(["port", &id, &format!("{inner_port}/tcp")]).output().ok()?;
    let text = String::from_utf8_lossy(&port_out.stdout).to_string();
    let port = text.lines().next()?.rsplit(':').next()?.trim().parse().ok()?;
    Some(Container { id, port })
}

/// Default options for tests.
pub fn opts() -> QueryOpts {
    QueryOpts { limit: None, timeout: Duration::from_secs(60), batch_size: 200 }
}

/// Runs a query collecting every batch.
pub async fn collect(conn: &mut Box<dyn Conn>, sql: &str, o: QueryOpts) -> (Vec<RowBatch>, QuerySummary) {
    let (tx, mut rx) = mpsc::channel(4);
    let consumer = tokio::spawn(async move {
        let mut v = Vec::new();
        while let Some(b) = rx.recv().await {
            v.push(b);
        }
        v
    });
    let summary = conn.query_stream(sql, o, tx).await.unwrap_or_else(|e| panic!("query failed: {sql}: {e}"));
    (consumer.await.unwrap(), summary)
}

/// All rows of all batches.
pub fn rows_of(batches: &[RowBatch]) -> Vec<Row> {
    batches.iter().flat_map(|b| b.rows.clone()).collect()
}
