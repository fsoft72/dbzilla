# DBZilla - Design (scope of the first autonomous run)

Source of truth for behaviour: `SPEC.md` (requirement IDs FR-/NFR-). Source of truth for look and feel:
the `liwe3-rust-ui` skill ("Obsidian"). This document records only what was decided on top of them.

## Intent

A fast native desktop client for SQLite, PostgreSQL and MySQL/MariaDB written in Rust + egui, with
an embedded MCP server. Built unattended in one long run, so every milestone must be verifiable
without a human (build, clippy, tests, real DB containers, screenshots).

## Goals

- SPEC milestones M0-M5, the import/export core of M7, and the MCP server of M10.
- Obsidian look (dark only), the widget kit and navigation model from the skill.
- Pending-change pipeline, safe mode, SQL editor v1, structure/index/filters, history, saved queries.
- Tests against real SQLite files and real PostgreSQL / MySQL containers (Docker).

## Non-goals (deferred, do not start)

ER diagram, Metrics Board, AI assistant, SSH tunnel, multi-window/viewports, multi-caret editor,
split panes, query parameters, Vim mode, Tier 2/3 drivers, user management, process list UI,
backup/restore, themes (SPEC 14.4 discarded), light theme, Auto theme, keymap editor, passcode,
connection sources, import/export of connection files, packaging.

## Anti-goals

- No second accent hue, no light surfaces, no raw colour literals in screen code.
- No blocking I/O in `update()`. No `match driver` in UI code (capabilities only).
- No cloning of the reference product's branding.

## Decisions

| Topic | Decision |
|---|---|
| Name | DBZilla (crate prefix `dbzilla-`, app id `dbzilla`) |
| Theme | Dark Obsidian only, `theme::apply` once |
| Versions | eframe/egui 0.31 (skill pins it); `egui_extras`, `egui_dock` versions chosen to match 0.31 |
| Dock | `egui_dock` if a 0.31-compatible release exists, else a small hand-written tab strip |
| Layout | Obsidian rail (64px) + left sidebar (Items / Queries / History) + tab area + right Details panel + console |
| SQL editor | `TextEdit` + custom highlight `LayoutJob`, single caret |
| Secrets | `keyring` crate; in tests an in-memory fake injected through a trait, never in production |
| Test DBs | Docker containers (PostgreSQL, MySQL) on high ports, throwaway; SQLite temp files |
| Platforms | Verified on Linux only; code kept portable |
| Git | One commit per task on `main`, no push, no AI attribution lines, hooks never bypassed |
| Blockers | After 3 failed attempts: note in `BLOCKERS.md` and `CHANGES.md`, move on |
| Verification | Per milestone: `cargo build`, `cargo clippy -D warnings`, `cargo test`, screenshot in `docs/screens/` |

Conflict resolutions between SPEC and skill: look and feel follows the skill; behaviour follows
the SPEC; the Obsidian rail replaces the SPEC "database stack" strip and holds the main
navigation, while Items/Queries/History live in the left sidebar next to it.

## Approaches considered

1. **Cargo workspace per SPEC section 2** (`core`, `drivers`, `store`, `ui`, `mcp`, `app`). Clean
   boundaries, drivers behind traits and features, UI-free logic testable alone. Chosen.
2. Single crate with modules. Faster to start, but the SPEC wants driver features and a separate
   MCP bridge; refactoring later costs more than splitting now.
3. Follow the skill's two-crate layout only (`core` + `app`). Too coarse for three drivers + MCP.

Chosen: workspace of crates, UI only in `dbzilla-app`.

## Left to the builder

Exact crate versions, file naming inside crates, SQL tokenizer details, icon drawing, MCP transport
details (Streamable HTTP via `axum` with hand-rolled JSON-RPC if `rmcp` does not fit), error
message wording. Anything ambiguous is decided, written in `CHANGES.md`, and not asked.
