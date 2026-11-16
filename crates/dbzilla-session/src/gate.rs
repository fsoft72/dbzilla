//! Safe mode gate (SPEC 9.3): decides whether statements may be sent right away.

use dbzilla_core::dialect::DriverId;
use dbzilla_core::safemode::{classify, SafeLevel};

/// What must happen before the statements are sent.
#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Allow,
    /// Show the statements and ask for confirmation.
    Confirm(Vec<String>),
    /// Show the statements and ask for the database password.
    Password(Vec<String>),
}

/// Applies the level matrix to every statement; the strictest requirement wins.
pub fn gate_check(level: SafeLevel, driver: DriverId, statements: &[String]) -> GateDecision {
    let kinds: Vec<_> = statements.iter().map(|s| classify(s, driver)).collect();
    if kinds.iter().any(|k| level.requires_password(*k)) {
        return GateDecision::Password(statements.to_vec());
    }
    if kinds.iter().any(|k| level.requires_confirm(*k)) {
        return GateDecision::Confirm(statements.to_vec());
    }
    GateDecision::Allow
}

/// Compares passwords in constant time; an empty stored password never matches.
pub fn verify_password(expected: &str, given: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..a.len().max(b.len()) {
        diff |= u64::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}
