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
- Task 7: PostgreSQL driver (tokio-postgres + rustls, binary value decoding, side-connection cancel, notices) tested against a Docker postgres:16-alpine.
- Task 8: MySQL/MariaDB driver (mysql_async + rustls, typed text protocol, KILL QUERY cancel, TLS modes) tested on mysql:8.4 and mariadb:11.8 containers.
