# SQL Server driver - design

For approval on 2026-11-17. Source of truth for behaviour: `SPEC.md` 1.1 (Tier 2), 5.2, 5.3, 5.4, 11.4.

## Intent
Add Microsoft SQL Server as a fourth driver, next to SQLite, PostgreSQL and MySQL/MariaDB. It is the first item of "Tier 2 drivers" in FEATURES.md and the one the user asked for. ArangoDB and the document-style model are explicitly out of this work.

## Goals
- New `DriverId::SqlServer`, with `Capabilities`, a `Dialect`, and a `Driver`/`Conn` implementation in `crates/dbzilla-drivers/src/mssql.rs`, behind a Cargo feature `mssql` (on by default, like the other three).
- Connection with host, port (default 1433), user, password, database and SSL modes. The "New connection" list offers "SQL Server". The `sqlserver://` and `microsoftsqlserver://` URL schemes of SPEC 6.3 are accepted.
- Browse: databases, schemas, tables, views, functions, procedures.
- Table structure: columns, primary key, foreign keys, indexes, triggers (read), DDL text.
- Data tab: paged `SELECT`, count, filters and sorting, row edits through pending changes and commit (`execute_batch`, atomic or not).
- SQL editor: statements stream through `query_stream` with back-pressure, timeout, and cancellation.
- Cancellation as in SPEC 5.3: a second connection runs `KILL <spid>`. This ends the session and rolls back its transaction, which is the documented behaviour.
- Process list with Kill (SPEC 11.4): running requests with session id, status, CPU time, elapsed time and text, from `sys.dm_exec_requests` joined to `sys.dm_exec_sql_text`. Kill uses `KILL <spid>` through the safe-mode gate, as for the other drivers.
- Integration tests on Docker, in the same style as `tests/postgres.rs` and `tests/mysql.rs`, skipped when Docker is missing.
- FEATURES.md, CHANGES.md and README (list of supported databases) updated.

## Non-goals
- User management and backup/restore for SQL Server (the SPEC does not list them for it).
- ArangoDB and any document-style model.
- Windows integrated authentication, Entra ID token login, Azure SQL specifics, named instances over SQL Browser. SQL login (user and password) only.
- Triggers editor for SQL Server: triggers are shown read-only, like SQLite today. `caps.triggers` read-only handling follows the SQLite precedent.
- Rename database through the UI if it needs more than `ALTER DATABASE ... MODIFY NAME`; if that is not simple, the capability stays off.
- Verifying on macOS and Windows (still an open FEATURES item).

## Anti-goals
- Requiring anything installed on the user's machine (ODBC manager, MS ODBC driver, OpenSSL). The driver must be self-contained like the others.
- Matching on `DriverId` in UI code. Behaviour differences go through `Dialect` and `Capabilities`, as the existing code rule says.

## Constraints
- Library: `tiberius` 0.13 (pure Rust TDS client, as named in SPEC 2). TLS must go through `rustls`, consistent with the other drivers; the default `native-tls` feature must not be pulled in. If tiberius 0.13 cannot do TLS through rustls with the `ring` provider already used in this workspace, stop and report before choosing another way (see Left to the builder).
- tiberius has no cancel token and no connection pool: cancellation needs the second connection above, and the driver must remember the spid of the working connection (`SELECT @@SPID`).
- No `LIMIT/OFFSET`. SQL Server paging is `ORDER BY ... OFFSET n ROWS FETCH NEXT m ROWS ONLY`, and `OFFSET/FETCH` is a syntax error without `ORDER BY`. Today `Dialect::limit_offset` returns a bare clause and is called from `crates/dbzilla-core/src/paging.rs`, `crates/dbzilla-io/src/export.rs` and `crates/dbzilla-core/src/copyfmt.rs` (the last one strips ` OFFSET 0` from the text, which does not work for SQL Server). These three callers must be adapted through the dialect, not special-cased on the driver id.
- Identifier quoting is `[name]` with `]` doubled. Booleans are `bit`; "now" is `GETDATE()`/`GETUTCDATE()` (SPEC 8.4); strings that may hold Unicode need the `N'...'` prefix.
- No `RETURNING`. Inserts that need generated keys use `OUTPUT INSERTED.*` or `SCOPE_IDENTITY()`; the existing pending-changes code must keep working with a dialect that has no `RETURNING`.
- Result values must map into the existing `Value` type, including `decimal`, `money`, `datetime2`, `datetimeoffset`, `uniqueidentifier`, `varbinary`, `xml`, `bit`.
- Integration tests use `mcr.microsoft.com/mssql/server` (needs `ACCEPT_EULA=Y`, a strong `SA_PASSWORD`, about 2 GB of RAM, x86 only, slow first start). Tests must wait for readiness and skip cleanly when Docker is missing.
- Pure logic (dialect, URL parsing, paging builders) goes in `dbzilla-core` with unit tests; driver code in `dbzilla-drivers`; UI changes in `dbzilla-app`.

## Approaches
- **A. `tiberius` (chosen).** Pure Rust, no system dependency, fits the self-contained distribution. Cost: no cancel token, so cancellation uses a second connection and `KILL`; no native Windows auth in this scope.
- **B. `odbc-api`.** Broader (Windows auth, the official MS driver) but needs unixODBC and the MS ODBC driver installed on the user's machine. That conflicts with the packaging and installer work still open in FEATURES.md. Rejected.

Scope: a first delivery with the browse, edit, query, cancel and process list items above (chosen); user management and backup stay out.

## Plan of work
One branch `feature/sqlserver-driver`, small commits, no push. Suggested order, each step compiling and tested:
0. Spike, before any other work: add `tiberius` with rustls only, confirm that it builds in this workspace sharing the existing `rustls` (ring) version, and that it connects to a Docker SQL Server with `Required` TLS. If it does not, stop and report (hard gate for the whole project).
1. `DriverId::SqlServer`, `Capabilities`, `Dialect` (quoting, literals, types, keywords, `now_expr`, paging hook) with unit tests in `dbzilla-core`; adapt the three `limit_offset` callers.
2. `mssql.rs`: connect, version, databases, schemas, objects, ping, set database; Docker test container support in `tests/support`.
3. Table structure, estimate count, `query_stream` with value mapping, `execute_batch`.
4. Cancellation with the second connection, create and drop database.
5. Process list and Kill.
6. UI: connection list entry, connection form defaults (port 1433), URL schemes, capability-driven enable/disable, process list wired for SQL Server.
7. Docs: FEATURES.md, CHANGES.md, README.

## Calls made while writing (marked as mine, not agreed)
- **Process list and Kill:** the driver-neutral part is a small addition. The per-driver list query and the kill statement are chosen where they are chosen today for Postgres and MySQL (`crates/dbzilla-app/src/ui/process.rs` holds a `DriverId`). Follow that existing pattern for SQL Server; do not refactor Postgres and MySQL in this work.
- **Paging and the editor row limit:** changing the `Dialect` signature is allowed. `apply_row_limit` (`crates/dbzilla-core/src/rowlimit.rs`) needs a SQL Server variant too (`TOP n` or `OFFSET/FETCH` on a wrapper), in addition to the three callers named above. A user query that already has its own `ORDER BY` must keep working.
- **SSL modes:** map to the other drivers' `SslMode` semantics: `Disabled` no encryption, `Preferred` and `Required` encrypt without verifying the certificate, `VerifyCa` and `VerifyFull` verify it. Docker tests use `Required` (the container has a self-signed certificate).
- **Cancel:** the driver keeps the `ConnectConfig` to open the second connection. After `KILL` the session is gone: the connection reconnects itself on the next call, restoring the selected database, and the UI reports the cancel as it does for the others.
- **Database context:** catalog queries use three-part names (`[db].sys.tables`), never `USE`. Only `set_database` changes the current database.
- **Literals:** strings are always rendered `N'...'`.
- **Generated keys and row counts:** pending changes do not read back generated values today, so `RETURNING`/`OUTPUT` is not needed. `execute_batch` sets `SET NOCOUNT OFF` at the start of the session so affected-row counts are reliable.
- **Docker tests:** same detection as the other drivers (skip when Docker is missing). The SQL Server container is slow, so its tests are also skipped unless `DBZILLA_TEST_MSSQL=1` is set.

## Left to the builder
- Exact `Dialect` hook for paging and `SELECT TOP 100` in Copy Script As (for example a method returning the full paging clause, or a flag that makes the builders add `ORDER BY (SELECT NULL)`); keep the three callers consistent.
- The exact catalog queries (`sys.*` versus `INFORMATION_SCHEMA`), and how triggers, indexes and DDL text are rebuilt (SQL Server has no `SHOW CREATE TABLE`; the DDL may be assembled from the structure).
- The exact `Capabilities` values for SQL Server not stated here (schemas on, functions and procedures on, materialized views off, `ilike` off, upsert and replace modes off, `reorder_columns` off), to be confirmed against what the engine really allows.
- TLS: modes (`disable`, `require`, `verify`) map to tiberius `EncryptionLevel` and certificate trust; pick the mapping that matches the other drivers' `SslMode` semantics.
- If tiberius 0.13 cannot meet the rustls constraint, report it and propose the closest alternative; do not add OpenSSL or ODBC without asking.
