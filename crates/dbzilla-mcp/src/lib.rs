//! Embedded MCP server: tokens, permissions and the Streamable HTTP endpoint.

pub mod exec;
pub mod perm;
pub mod server;
pub mod tokens;

/// Default port (SPEC 16).
pub const DEFAULT_PORT: u16 = 51003;
