# DBZilla

A fast native database client written in Rust with egui. It talks to SQLite, PostgreSQL and
MySQL/MariaDB and ships an embedded MCP server.

## Features

- Connections with groups, tags, safe mode levels, keychain passwords (or password commands).
- Data grid with streaming loads, inline editing and pending changes (preview, commit, discard, undo).
- Filters, quick filters, search in database, structure / index / DDL views and table operations.
- SQL editor with highlighting, context aware autocomplete, history and saved queries.
- Export (CSV, JSON, SQL with gzip, XLSX) and import (CSV, JSON, SQL dumps) as background jobs.
- MCP server (Streamable HTTP on `127.0.0.1:51003/mcp`) with Bearer tokens and Read Only,
  Read & Write or Full Access permissions, write approval and an activity log.
- `dbzilla-mcp-bridge`: stdio bridge for MCP clients without HTTP support
  (`DBZILLA_MCP_TOKEN=... dbzilla-mcp-bridge --port 51003`).

## Build and run

```sh
cargo build --release
./target/release/dbzilla [--data-dir DIR] [URL | FILE]
```

The OS keychain needs libdbus at build time on Linux. Examples of URLs:
`postgresql://user@host/db`, `mysql://root@127.0.0.1/shop`, `sqlite:///path/file.sqlite`.

## Tests

```sh
cargo test --workspace
```

PostgreSQL and MySQL tests start throwaway Docker containers and are skipped when Docker is not
available. Always run `cargo clippy --all-targets -- -D warnings` before committing.

## Layout

| Crate | Purpose |
|---|---|
| `dbzilla-core` | Values, dialects, filters, pending changes, DDL, safe mode (no I/O) |
| `dbzilla-drivers` | SQLite, PostgreSQL and MySQL drivers behind one trait |
| `dbzilla-store` | Settings, connections, history, saved queries, secrets |
| `dbzilla-session` | Per-connection actor with commands and events |
| `dbzilla-io` | Streaming export and transactional import |
| `dbzilla-mcp` | MCP server, tokens, permissions, stdio bridge |
| `dbzilla-app` | The egui application |

Not included yet: ER diagram, metrics, AI assistant, SSH tunnels, multi-window, Vim mode,
other databases, light theme, packaging. See `SPEC.md` and `CHANGES.md`.
