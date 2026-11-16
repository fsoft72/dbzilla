# Changes

## 2026-11-16
- Added design doc for the first autonomous run (docs/superpowers/specs).
- Added first-run implementation plan (docs/superpowers/plans).
- Task 1: workspace skeleton, Obsidian theme, empty window, screenshot helper (scripts/screenshot.sh).
- Task 2: core model, Value, Dialect quoting/literals (proptest round trip), Capabilities.
- Task 3: statement splitter (strings, comments, $$ bodies, MySQL DELIMITER, labels) and safe-mode classifier + level matrix.
- Task 4: filters to WHERE (all operators, ILIKE, Any, Raw) and pending ChangeSet to SQL with undo/redo.
- Task 5: connection URLs, DDL generation (SQLite rebuild for alter column), copy formats and Copy Script As.
- Task 6: driver traits and SQLite driver (streaming, cancel via interrupt, atomic batches, DB files list/create/rename/drop/switch).
- Task 7: PostgreSQL driver (tokio-postgres + rustls, binary value decoding, side-connection cancel, notices) tested against a Docker postgres:16-alpine.
- Task 8: MySQL/MariaDB driver (mysql_async + rustls, typed text protocol, KILL QUERY cancel, TLS modes) tested on mysql:8.4 and mariadb:11.8 containers.
- Task 9: store crate (atomic JSON with corrupt-file backup, settings with SPEC defaults, connections/groups/tags, JSONL history, saved queries with path-escape protection, keyring and memory secret stores). SslMode moved to core.
- Task 10: session actor (commands/events, streaming with back-pressure, cancel, timeout signal, keep-alive, reconnect), safe-mode gate, console redaction. Core gained paging builders; ConnectConfig Debug hides the password; Conn::cancel_handle takes &mut self.
- Task 11: app shell (rail + list pane + detail), Obsidian widget kit, painter icons, keymap, CLI, connection list (groups, tags, sort, search, context menu), connection form (validation, test, keychain/ask/command passwords), URL import, settings screen, toasts, workspace stub. Screenshots in docs/screens (scripts/screenshot.sh).
- Task 12: workspace screen (toolbar, status bar tinted by tag/status color, collapsible console, sidebar Items with fuzzy search and sections, egui_dock tabs with preview/pin semantics, Open Anything palette, SQLite end-to-end verified).
- Task 13: Data tab (virtualised grid with selection/drag/keyboard, sort cycle, hide columns, FK navigation, NULL/blob rendering, copy formats, Quick Look text/JSON/hex, paging with estimate/exact count, request routing per tab).
- Task 14: pending changes (inline cell editor, Set value, insert/duplicate/delete, undo/redo, row state colours, key columns, preview dialog, commit through the safe-mode gate, discard, Details panel with bulk edit). End-to-end tests with real session and SQLite cover commit, rollback and gate.
- Task 15: SQL editor (syntax highlight with cached layout, wrapped editor with line numbers and current-statement band, Run current/all, cancel, row limit injection, named result tabs, pinned results, Data/Message views, streaming results, beautify/minify, comment toggle, find/replace). 1M rows stream test keeps every frame fast.
