# SQL Server driver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Microsoft SQL Server as the fourth driver (browse, structure, data, editor, cancel, process list).

**Architecture:** A new `mssql.rs` in `dbzilla-drivers` behind a Cargo feature `mssql`, built on `tiberius` with `rustls`. A new `DriverId::SqlServer` flows through `dbzilla-core` (capabilities, dialect, URL, process list, safe mode) and the UI, which only ever asks the dialect and capabilities, never the driver id.

**Tech Stack:** Rust, `tiberius` 0.13 (feature `rustls`, no default features), `tokio`, `rustls` 0.23 with `ring` (already in the workspace), Docker `mcr.microsoft.com/mssql/server:2022-latest` for integration tests.

**Spec:** `docs/superpowers/specs/2026-11-17-sqlserver-driver-design.md` (read it first; it also lists the calls already made).

## Global Constraints

- Branch `feature/sqlserver-driver`, one commit per task, never push, never `--no-verify`.
- No em dash anywhere; code and comments in English; every function has a doc comment; exit conditions at the top of functions; constants in UPPER_CASE.
- `tiberius` with `default-features = false` and feature `rustls` only. No `native-tls`, no OpenSSL, no ODBC. If the spike (Task 1) fails, stop and report.
- UI code must not match on `DriverId`; use `Dialect` and `Capabilities`. The existing `match` sites (`processes.rs`, `url.rs`, store, connform labels) get a `SqlServer` arm.
- Identifiers quoted `[name]` with `]` doubled; strings always `N'...'`; no `USE`, catalog queries use `[db].sys.*`; default port 1433.
- SQL Server integration tests run only when `DBZILLA_TEST_MSSQL=1` and Docker answers; otherwise they return early (pass).
- Update `CHANGES.md` in every task's commit, `FEATURES.md` and `README.md` in the last task.

## Review Focus

- A user query that already ends with `ORDER BY` and gets the editor row limit: must not get a second `ORDER BY` or a syntax error.
- Unicode text (`N'ciao àèì 日本'`) written through pending changes and read back unchanged.
- A table with no primary key and no sort: the Data tab page still works (`ORDER BY (SELECT NULL)`).
- Cancelling a long `WAITFOR DELAY` query: the call returns `DriverError::Cancelled`, the connection works on the next call and keeps the selected database.
- A wrong password or an unreachable host: a `DriverError::Connect` with a readable message, no panic or hang.
- `NULL`, `bit`, `decimal(38,10)`, `datetimeoffset` and `uniqueidentifier` values in a result map to the right `Value` variants.

---

### Task 1: TLS spike and Cargo feature (gate)

**Files:**
- Modify: `crates/dbzilla-drivers/Cargo.toml`, `crates/dbzilla-drivers/src/lib.rs`
- Create: `crates/dbzilla-drivers/src/mssql.rs` (empty module doc for now), `crates/dbzilla-drivers/tests/mssql.rs`
- Modify: `crates/dbzilla-drivers/tests/support/mod.rs`

**Interfaces:**
- Produces: Cargo feature `mssql = ["dep:tiberius", "dep:tokio-util", "dep:chrono", "dep:uuid", "dep:serde_json"]` in `default`; `support::mssql_enabled() -> bool` (true when `DBZILLA_TEST_MSSQL=1`).

- [ ] **Step 1:** Add `tiberius = { version = "0.13", default-features = false, features = ["rustls", "tds73", "chrono", "rust_decimal"], optional = true }` and the feature `mssql`. Add `pub mod mssql;` under `#[cfg(feature = "mssql")]` in `lib.rs`.
- [ ] **Step 2:** Run `cargo tree -p dbzilla-drivers -i rustls` and `cargo tree -p dbzilla-drivers | grep -ci openssl`. Expected: a single `rustls 0.23.x` and `0` OpenSSL lines. If tiberius pulls a second rustls major or any OpenSSL, stop and report.
- [ ] **Step 3: Write the spike test** `tests/mssql.rs::tls_spike_connects` (return early unless `support::mssql_enabled()`): starts the container with label `dbzilla-test-mssql`, image `mcr.microsoft.com/mssql/server:2022-latest`, env `ACCEPT_EULA=Y` and `MSSQL_SA_PASSWORD=Dbzilla_Pw1!`, inner port 1433, waits up to 240 s for a login, using tiberius directly with `EncryptionLevel::Required` and `trust_cert()`; asserts `SELECT 1` returns 1.
- [ ] **Step 4:** Run `DBZILLA_TEST_MSSQL=1 cargo test -p dbzilla-drivers --test mssql tls_spike_connects`. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver driver dependency and TLS spike`.

### Task 2: DriverId, capabilities, dialect, URL, store, safe mode

**Files:**
- Modify: `crates/dbzilla-core/src/dialect.rs`, `caps.rs`, `url.rs`, `safemode.rs`, `processes.rs`
- Modify: `crates/dbzilla-store/src/connections.rs`, `crates/dbzilla-app/src/ui/connform.rs` (label, default user `sa`, SSL labels), `connlist.rs` (monogram `MS`, New connection entry "SQL Server"), `cli.rs` (help URL example)
- Test: `crates/dbzilla-core/tests/quoting.rs`, `url.rs`, `crates/dbzilla-core/src/caps.rs` tests

**Interfaces:**
- Produces: `DriverId::SqlServer`; `pub struct MsSqlDialect` implementing `Dialect`; `Capabilities::for_driver(DriverId::SqlServer)`; `dialect_for(DriverId::SqlServer)`.

- [ ] **Step 1: Write failing tests** in `quoting.rs`: `[a]b]` quoting (`quote_ident("a]b") == "[a]]b]"`), `literal(&Value::Text("it's".into())) == "N'it''s'"`, bool literal `1`/`0`, `now_expr() == "GETDATE()"`, `cast_text("x") == "CAST(x AS NVARCHAR(MAX))"`, `insert_defaults("[t]") == "INSERT INTO [t] DEFAULT VALUES"`. In `url.rs`: `sqlserver://sa:pw@h/db` and `microsoftsqlserver://...` parse to `DriverId::SqlServer` with default port 1433. In `caps.rs`: SqlServer has `schemas`, `functions`, `procedures`, `process_list`, `triggers` true; `materialized_views`, `ilike`, `upsert_on_duplicate`, `replace_into`, `reorder_columns`, `rename_db` (design: stays off unless simple), `truncate_cascade`, `drop_cascade` false.
- [ ] **Step 2:** Run `cargo test -p dbzilla-core`. Expected: compile errors, then FAIL.
- [ ] **Step 3:** Add the enum variant, `MsSqlDialect` (types: `int, bigint, smallint, tinyint, bit, decimal, numeric, money, float, real, nvarchar, varchar, nchar, char, text, ntext, varbinary, image, date, time, datetime, datetime2, datetimeoffset, uniqueidentifier, xml`; keywords `COMMON_KEYWORDS` plus `TOP, EXEC, GO, IDENTITY, NOLOCK, OUTPUT`), caps arm, URL schemes, port constant `MSSQL_PORT = 1433`, URL rendering scheme `sqlserver`, store name `"SQL Server"` and SSL index mapping like MySQL's, safe mode with `sqlparser::dialect::MsSqlDialect`. Let the compiler list every non-exhaustive `match` and add the arm.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5:** Commit `feat: SqlServer driver id, capabilities and dialect`.

### Task 3: Paging, editor row limit and Copy Script As through the dialect

**Files:**
- Modify: `crates/dbzilla-core/src/dialect.rs`, `paging.rs`, `rowlimit.rs`, `copyfmt.rs`, `crates/dbzilla-io/src/export.rs`, `crates/dbzilla-app/src/ui/workspace.rs:36` (call site)
- Test: `crates/dbzilla-core/tests/paging.rs`, `rowlimit.rs`, `quoting.rs`

**Interfaces:**
- Produces on `Dialect` (defaults keep today's behaviour):
  - `fn page_clause(&self, limit: u64, offset: u64, has_order_by: bool) -> String` default `self.limit_offset(limit, offset)`; SQL Server: `[ORDER BY (SELECT NULL) ]OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY` (the fallback `ORDER BY` only when `!has_order_by`).
  - `fn row_limit_clause(&self, n: u64, has_order_by: bool) -> String` default `format!("LIMIT {n}")`; SQL Server: same shape as above with offset 0.
  - `fn select_top(&self, quoted_table: &str, n: u64) -> String` default `SELECT * FROM {t} LIMIT {n};`; SQL Server `SELECT TOP {n} * FROM {t};`.
- Produces: `pub fn apply_row_limit_for(d: &dyn Dialect, sql: &str, limit: Option<u64>) -> String`; `apply_row_limit` stays and calls it with the default behaviour.

- [ ] **Step 1: Write failing tests:** `build_select(&MsSqlDialect, t, cols, &[], false, &[], 300, 600)` ends with `ORDER BY (SELECT NULL) OFFSET 600 ROWS FETCH NEXT 300 ROWS ONLY`; with a sort key it has exactly one `ORDER BY` and no `(SELECT NULL)`; `apply_row_limit_for(&MsSqlDialect, "SELECT * FROM t ORDER BY a", Some(50))` ends with `OFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY` and contains one `ORDER BY`; a query containing `TOP`, `OFFSET` or `FETCH` at top level is returned unchanged; PostgreSQL and MySQL outputs are unchanged (existing tests stay green).
- [ ] **Step 2:** Run `cargo test -p dbzilla-core`. Expected: FAIL.
- [ ] **Step 3:** Implement the three `Dialect` methods and route `paging.rs`, `export.rs`, `copyfmt.rs` (`ScriptKind::SelectTop100`) and `rowlimit.rs` through them; `top_level_words` already finds `ORDER`, add `TOP`, `OFFSET`, `FETCH` to the blockers for the SQL Server path only. Update `workspace.rs` to call `apply_row_limit_for` with the session dialect.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5:** Commit `feat: dialect hooks for paging, row limit and top-N scripts`.

### Task 4: Connection, version, browse (databases, schemas, objects)

**Files:**
- Modify: `crates/dbzilla-drivers/src/mssql.rs`, `lib.rs` (`driver_for` arm), `tests/support/mod.rs`
- Test: `crates/dbzilla-drivers/tests/mssql.rs`

**Interfaces:**
- Produces: `pub struct MsSqlDriver` (`new()`, implements `Driver`); private `MsConn { client: Option<Client<Compat<TcpStream>>>, cfg: ConnectConfig, current_db: String, spid: i32 }` implementing `Conn`; `fn map_err(e: tiberius::error::Error) -> DriverError`; `async fn open(cfg: &ConnectConfig) -> DriverResult<(Client, i32)>` (connects, runs `bootstrap`, reads `@@SPID`); `async fn ensure_client(&mut self) -> DriverResult<&mut Client>` (reconnects and re-selects `current_db` when `client` is `None`).
- Consumes: Task 1 spike dependency, Task 2 `DriverId::SqlServer`.

- [ ] **Step 1: Write failing tests** (all return early unless `mssql_enabled()`): `connects_and_reports_version` (`server_version()` contains `Microsoft SQL Server`), `wrong_password_is_a_connect_error` (`DriverError::Connect`), `lists_databases_schemas_and_objects` (create a database, a schema `app`, a table, a view, a function and a procedure; `list_objects(db, Some("app"))` returns each with the right `ObjectKind`), `ping_and_set_database`.
- [ ] **Step 2:** Run `DBZILLA_TEST_MSSQL=1 cargo test -p dbzilla-drivers --test mssql`. Expected: FAIL.
- [ ] **Step 3: Implement** connect with `EncryptionLevel` from `SslMode` (`Disabled` -> `NotSupported`; `Preferred` -> `On` falling back to `NotSupported` as MySQL does; `Required` and `Preferred` use `trust_cert()`; `VerifyCa` and `VerifyFull` verify), `list_databases` from `sys.databases`, `list_schemas` and `list_objects` from `[db].sys.schemas`, `[db].sys.objects` (types `U`, `V`, `FN/IF/TF`, `P`; skip system schemas `sys`, `INFORMATION_SCHEMA`), `set_database` (executes `USE [db]` only here), `ping` (`SELECT 1`). Share the container and readiness loop in `tests/support` as `start_mssql() -> Option<&'static Container>`.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver driver connect and browse`.

### Task 5: Table structure and estimated count

**Files:** Modify `mssql.rs`; Test `tests/mssql.rs`.

**Interfaces:**
- Consumes: `MsConn`, `ensure_client` (Task 4). Produces: `Conn::table_structure`, `Conn::estimate_count`, `Conn::trigger_functions` left at the default.

- [ ] **Step 1: Write failing tests:** `table_structure_reads_columns_keys_indexes_triggers` (table with identity PK, composite unique index, FK to another table with `ON DELETE CASCADE`, nullable column with a default, and a trigger; assert `columns[i].type_name` (e.g. `nvarchar(50)`, `decimal(10,2)`), `nullable`, `default`, `is_pk`, `fk.table`, `fk.on_delete == "CASCADE"`, `indexes`, `triggers[0].timing/event`, `ddl` starting `CREATE TABLE`); `estimate_count_uses_partition_stats` (`Some(n)` after inserting rows).
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3: Implement** from `sys.columns`, `sys.types`, `sys.key_constraints`, `sys.foreign_keys` and `sys.foreign_key_columns`, `sys.indexes` and `sys.index_columns`, `sys.triggers` and `sys.sql_modules`; assemble `ddl` from the structure (SQL Server has no `SHOW CREATE TABLE`); type names include length, precision and scale; identity goes in `extra["identity"]`; estimate from `sys.dm_db_partition_stats` (`row_count` where `index_id IN (0,1)`).
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver table structure and row estimate`.

### Task 6: query_stream and value mapping

**Files:** Modify `mssql.rs`; Test `tests/mssql.rs`.

**Interfaces:**
- Produces: `fn to_value(d: &tiberius::ColumnData<'_>) -> Value`; `Conn::query_stream`.

- [ ] **Step 1: Write failing tests:** `maps_types` (one row selecting `CAST(NULL AS int)`, `CAST(1 AS bit)`, `CAST(12.3456789012 AS decimal(38,10))`, `CAST('2020-01-02T03:04:05.123' AS datetime2)`, `SYSDATETIMEOFFSET()`, `NEWID()`, `0xDEADBEEF`, `N'ciao àèì 日本'`, `CAST(1.5 AS float)`, `CAST('<a/>' AS xml)`; assert `Null`, `Bool(true)`, `Decimal("12.3456789012")`, `DateTime`, `DateTimeTz`, `Uuid`, `Bytes`, `Text`, `F64`, `Text` respectively); `streams_in_batches_with_limit` (1000 rows, `batch_size 200`, `limit Some(450)` gives 450 rows in batches, first batch carries `columns`); `non_row_statement_reports_affected_rows` (`UPDATE` summary `affected == Some(n)`); `timeout_fails` (`WAITFOR DELAY '00:00:05'` with a 1 s timeout returns `DriverError::Query` or `Cancelled`, and the connection still works afterwards).
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3: Implement** with `Client::simple_query` consumed through `QueryStream::try_next`, sending `RowBatch` through `tx.send(..).await` for back-pressure; column metadata from `QueryStream::columns()` (`type_name` from the column type); `SET NOCOUNT OFF` once after connect; messages from info tokens go into `QuerySummary::messages`; timeout with `tokio::time::timeout` and a reconnect on expiry.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver query streaming and value mapping`.

### Task 7: execute_batch, database create, drop, rename

**Files:** Modify `mssql.rs`; Test `tests/mssql.rs`.

**Interfaces:**
- Produces: `Conn::execute_batch(&mut self, stmts: &[String], atomic: bool) -> DriverResult<Vec<u64>>`, `create_database`, `drop_database`, `rename_database`.

- [ ] **Step 1: Write failing tests:** `batch_returns_affected_counts`; `atomic_batch_rolls_back_on_error` (second statement fails, first row absent afterwards); `unicode_roundtrip` (insert `N'ciao àèì 日本'` with the dialect literal, read back equal); `creates_and_drops_database` (with `collation` option).
- [ ] **Step 2:** Run the tests. Expected: FAIL.
- [ ] **Step 3: Implement** `atomic` with `BEGIN TRANSACTION`/`COMMIT`/`ROLLBACK` on the same client, counts from `ExecuteResult::rows_affected()`; `create_database` with optional `COLLATE`; `drop_database` with `ALTER DATABASE [x] SET SINGLE_USER WITH ROLLBACK IMMEDIATE` first; `rename_database` through `ALTER DATABASE [a] MODIFY NAME = [b]`. If any of them needs more than that, set the capability off and return `DriverError::Unsupported`.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver batch execution and database management`.

### Task 8: Cancellation with KILL and reconnect

**Files:** Modify `mssql.rs`; Test `tests/mssql.rs`.

**Interfaces:**
- Produces: `Conn::cancel_handle` returning a `CancelToken` that opens a second connection from a clone of `cfg`, runs `KILL {spid}`, and sets a shared `Arc<AtomicBool>` read by `query_stream` to turn the broken-connection error into `DriverError::Cancelled`; after a cancel `client` is `None` and `ensure_client` reconnects (restoring `current_db`).

- [ ] **Step 1: Write failing test** `cancel_stops_waitfor_and_connection_recovers`: spawn `WAITFOR DELAY '00:00:30'`, cancel after 1 s from another task, assert `Cancelled` within 5 s, then `SELECT DB_NAME()` returns the previously selected database.
- [ ] **Step 2:** Run the test. Expected: FAIL.
- [ ] **Step 3:** Implement as above; the flag is cleared at the start of every `query_stream`.
- [ ] **Step 4:** Run the test. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver cancellation with KILL`.

### Task 9: Process list and Kill

**Files:** Modify `crates/dbzilla-core/src/processes.rs`, `crates/dbzilla-app/src/ui/process.rs` (only where `DriverId` is matched for labels and tests); Test `processes.rs` unit tests and `crates/dbzilla-drivers/tests/mssql.rs`.

**Interfaces:**
- Consumes: `processes::supported`, `list_sql`, `kill_sql`. Produces: SQL Server arms. `list_sql` returns `SELECT r.session_id, DB_NAME(r.database_id) AS database_name, s.login_name, s.host_name, r.status, r.cpu_time, r.total_elapsed_time, t.text AS query FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s ON s.session_id = r.session_id OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t WHERE s.is_user_process = 1 AND r.session_id <> @@SPID ORDER BY r.total_elapsed_time DESC`; `kill_sql` returns `KILL {id};` for a numeric id only.

- [ ] **Step 1: Write failing tests:** in `processes.rs`: `supported(DriverId::SqlServer)`, `kill_sql(SqlServer, "55") == Some("KILL 55;")`, `kill_sql(SqlServer, "5;DROP")` is `None`. Integration `process_list_shows_and_kills_a_running_request`: a second connection runs `WAITFOR DELAY '00:00:30'`; run `list_sql` through `query_stream`, find its `session_id` in column `ID_COLUMN`, run the kill statement, assert the waiting query fails.
- [ ] **Step 2:** Run both. Expected: FAIL.
- [ ] **Step 3:** Add the arms; make `process.rs` tests and labels compile with the new variant, with no refactor of the PostgreSQL and MySQL paths.
- [ ] **Step 4:** Run `cargo test --workspace` and the integration test. Expected: PASS.
- [ ] **Step 5:** Commit `feat: sqlserver process list and kill`.

### Task 10: UI wiring check, docs, final run

**Files:** Modify `FEATURES.md`, `README.md`, `CHANGES.md`; no new code unless a manual check fails.

- [ ] **Step 1:** Run `cargo clippy --workspace --all-targets` and `cargo test --workspace`. Expected: no warnings, all pass.
- [ ] **Step 2:** Start the app (`run` skill), against a Docker SQL Server create a connection "SQL Server", open the sidebar, a table Data tab (page 2 works, sort works), run `SELECT TOP 5 * FROM sys.objects` and a long `WAITFOR` then Cancel, open the process list and kill a session. Fix what fails in the owning task's file and commit it as a fix.
- [ ] **Step 3:** FEATURES.md: tick `SQL Server` under Tier 2 drivers (SQL Server done, others remain) and remove "SQL Server" from the process list "still missing" line. README: add SQL Server to supported databases. CHANGES.md: summary.
- [ ] **Step 4:** Commit `docs: SQL Server driver in FEATURES, README and CHANGES`.

---

## Self-review

- **Spec coverage:** goals (driver, connection and URL, browse, structure, data, editor, cancel, process list, tests, docs) map to Tasks 2 to 10; the TLS gate is Task 1; all nine "calls made" are placed (SSL in 4, literals and quoting in 2, `NOCOUNT` in 6, three-part names in 4, reconnect in 8, test gate in 1, process list pattern in 9, paging in 3).
- **Type consistency:** `page_clause`, `row_limit_clause`, `select_top`, `apply_row_limit_for`, `MsConn`, `ensure_client`, `to_value` are named identically wherever used.
- **Out of scope kept out:** no user management, backup, ArangoDB, Windows auth.
