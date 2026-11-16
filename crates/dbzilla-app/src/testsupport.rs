//! Docker helpers for the database backed tests of the app crate.

use std::process::Command;

/// A running throwaway container and its mapped host port.
pub struct Container {
    pub port: u16,
}

/// True when the Docker daemon answers.
pub fn docker_available() -> bool {
    Command::new("docker").arg("info").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Removes every container carrying `label`.
pub fn cleanup(label: &str) {
    let Ok(out) = Command::new("docker").args(["ps", "-aq", "--filter", &format!("label={label}")]).output() else { return };
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect();
    if !ids.is_empty() {
        let _ = Command::new("docker").arg("rm").arg("-f").args(&ids).output();
    }
}

/// Starts a container publishing `inner_port` on a random localhost port; `None` without Docker.
pub fn start(label: &str, image: &str, env: &[(&str, &str)], inner_port: u16) -> Option<Container> {
    if !docker_available() {
        return None;
    }
    cleanup(label);
    let mut cmd = Command::new("docker");
    cmd.args(["run", "-d", "--rm", "--label", label, "-p", &format!("127.0.0.1::{inner_port}")]);
    for (k, v) in env {
        cmd.args(["-e", &format!("{k}={v}")]);
    }
    cmd.arg(image);
    let out = cmd.output().ok()?;
    assert!(out.status.success(), "docker run failed: {}", String::from_utf8_lossy(&out.stderr));
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let port_out = Command::new("docker").args(["port", &id, &format!("{inner_port}/tcp")]).output().ok()?;
    let text = String::from_utf8_lossy(&port_out.stdout).to_string();
    let port = text.lines().next()?.rsplit(':').next()?.trim().parse().ok()?;
    Some(Container { port })
}
