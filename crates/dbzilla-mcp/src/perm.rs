//! Token permissions (SPEC 16).

use dbzilla_core::safemode::StatementKind;
use serde::{Deserialize, Serialize};

/// What a token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    ReadOnly,
    ReadWrite,
    Full,
}

impl Permission {
    /// True when a statement of `kind` is allowed.
    pub fn allows(self, kind: StatementKind) -> bool {
        match (self, kind) {
            (_, StatementKind::ReadOnly) => true,
            (Permission::ReadOnly, _) => false,
            (Permission::ReadWrite, StatementKind::Write) => true,
            (Permission::ReadWrite, StatementKind::Ddl) => false,
            (Permission::Full, _) => true,
        }
    }

    /// Label shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Permission::ReadOnly => "Read Only",
            Permission::ReadWrite => "Read & Write",
            Permission::Full => "Full Access",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read only never writes, read-write never changes the schema, full does everything.
    #[test]
    fn matrix() {
        use StatementKind::*;
        assert!(Permission::ReadOnly.allows(ReadOnly) && !Permission::ReadOnly.allows(Write) && !Permission::ReadOnly.allows(Ddl));
        assert!(Permission::ReadWrite.allows(Write) && !Permission::ReadWrite.allows(Ddl));
        assert!(Permission::Full.allows(Ddl));
    }
}
