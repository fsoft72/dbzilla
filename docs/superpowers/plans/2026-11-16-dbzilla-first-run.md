# DBZilla First Run Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A native Rust + egui database client (SQLite, PostgreSQL, MySQL/MariaDB) in the Obsidian style, with pending changes, safe mode, SQL editor, structure/filters, import/export and an MCP server.

**Architecture:** Cargo workspace. UI-free crates (`core`, `drivers`, `store`, `session`, `mcp`) tested in isolation; `dbzilla-app` holds all egui code. UI talks to one `SessionActor` per connection through mpsc commands/events (SPEC 3.1, 25). Drivers are capability-driven (SPEC 5).

**Tech Stack:** Rust stable, eframe/egui 0.31, egui_extras, egui_dock (if compatible), tokio, rusqlite (bundled), tokio-postgres, mysql_async, keyring, serde, sqlparser, csv, rust_xlsxwriter, flate2, axum, proptest, insta, testcontainers-free Docker via `docker run` helper.

**Spec:** `SPEC.md` (behaviour), `docs/superpowers/specs/2026-11-16-dbzilla-design.md` (scope and decisions), skill `liwe3-rust-ui` + `~/.claude/skills/liwe3-rust-ui/references/guidelines.md` (look and feel).

## Global Constraints

- Dark Obsidian only; every colour/radius/size from `theme.rs` consts; no emoji or icon fonts in chrome.
- eframe/egui 0.31; `theme::apply` called once; main screens never animate (zero idle CPU).
- UI thread never awaits I/O; no `match driver` in UI code, use `Capabilities`.
- All data mutations go through the pending-change pipeline except SQL typed in the editor (SPEC 9).
- Defaults: page size 300, keep-alive 30 s, query timeout 300 s, console 1000 entries, import preview 100 rows / 100,000 chars, result tabs collapse above 100 statements, MCP port 51003, MCP max query 300 s.
- Safe mode levels: Silent, Alert 1, Alert 2, Safe 1, Safe 2; default Silent.
- Secrets only in the OS keychain, never in JSON, never in logs (console redacts `password`).
- Code and comments in English, doc comment on every function, UPPER_CASE consts, early-return exits, no em dash, files end with a newline.
- Exclusions: licensing, telemetry, auto-update, light theme, SSH, multi-window, ER, metrics, AI, user management, process list UI, backup/restore.
- One git commit per task on `main`, never push, no AI attribution, never bypass hooks. `CHANGES.md` updated every task.
- After 3 failed attempts on one point: log in `BLOCKERS.md`, move on.

## Review Focus

1. Identifiers/values with quotes, backticks, semicolons, unicode or SQL keywords must never break or inject (quoting + literal escaping, proptest in Task 1).
2. NULL vs empty string vs "DEFAULT" must stay distinct from grid edit to generated SQL (Task 4).
3. Tables with no primary key: UPDATE/DELETE refused unless key columns chosen; INSERT allowed (Task 4, 15).
4. Commit failure mid-batch must leave pending changes intact and the DB unchanged (transaction) (Task 4, 11, 15).
5. Huge cells (BLOB, 10 MB text) and 1M-row results must not freeze the UI or exhaust memory (Task 14, 16).
6. Statement splitter must not split inside strings, comments, `$$` bodies or MySQL `DELIMITER` blocks (Task 3).

## File Structure

```
Cargo.toml                      workspace + profiles (release panic=abort, dev deps opt-level 3)
crates/dbzilla-core/src/        lib.rs value.rs model.rs dialect.rs filter.rs changes.rs ddl.rs
                                splitter.rs safemode.rs url.rs caps.rs copyfmt.rs
crates/dbzilla-drivers/src/     lib.rs traits.rs sqlite.rs postgres.rs mysql.rs
crates/dbzilla-store/src/       lib.rs paths.rs atomic.rs settings.rs connections.rs history.rs
                                queries.rs secrets.rs
crates/dbzilla-session/src/     lib.rs actor.rs commands.rs gate.rs
crates/dbzilla-io/src/          lib.rs export.rs import.rs
crates/dbzilla-mcp/src/         lib.rs server.rs tokens.rs tools.rs
crates/dbzilla-app/src/         main.rs app.rs theme.rs glyphs.rs keymap.rs cli.rs
                                ui/{mod,widgets,welcome,connform,workspace,sidebar,tabs,grid,details,
                                    quicklook,sqleditor,results,structure,filters,dialogs,settings,
                                    console,palette,mcp_settings}.rs
                                assets/fonts/*
tests/ (per crate) docker helper in crates/dbzilla-drivers/tests/support/mod.rs
docs/screens/                   milestone screenshots
```

---

### Task 1: Workspace skeleton, theme, empty window (M0)

**Files:** Create workspace `Cargo.toml`, all crate manifests (empty `lib.rs`), `dbzilla-app/src/{main.rs,app.rs,theme.rs}`, `assets/fonts/*` + `LICENSE.txt` (copy from the Paranoid app), `CHANGES.md` update.

**Interfaces:**
- Produces: `theme::apply(&egui::Context)`, all palette consts and `RADIUS_*`, `theme::semibold()`, `theme::parse_hex_color`, `theme::to_hex`; `App` implementing `eframe::App`.

- [ ] Step 1: Copy `templates/theme.rs` from the skill to `dbzilla-app/src/theme.rs`; promote `#323A4D` to `BUTTON_BORDER`. Add unit tests `parse_hex_color("#3F6BDB")`, `"3F6BDB"`, `"zz"` (None), `to_hex` round trip.
- [ ] Step 2: `main.rs` builds the viewport (app id `dbzilla`, title `DBZilla`, 1280x800, min 900x560), `App::new` calls `theme::apply`.
- [ ] Step 3: Verify `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Launch the app on `DISPLAY=:0`, take a screenshot to `docs/screens/m0.png`.
- [ ] Step 4: Commit `feat: workspace skeleton and Obsidian theme`.

### Task 2: Core model, Value, Dialect quoting

**Files:** `core/src/{value.rs,model.rs,dialect.rs,caps.rs}`; tests inline + `core/tests/quoting.rs` (proptest).

**Interfaces:**
- Produces: `enum Value` (SPEC 3.2, without geometry/uuid deps beyond `chrono`, `uuid`), `ColumnMeta`, `TableRef{database,schema,name}`, `ResultSet`, `TableStructure{columns,primary_key,foreign_keys,indexes,triggers,ddl}`, `Capabilities` (SPEC 5.2), `trait Dialect { fn quote_ident(&self,&str)->String; fn quote_table(&self,&TableRef)->String; fn literal(&self,&Value)->String; fn limit_offset(&self,u64,u64)->String; fn id(&self)->DriverId; fn types(&self)->&'static [&'static str]; fn keywords(&self)->&'static [&'static str]; }`, impls `SqliteDialect`, `PostgresDialect`, `MySqlDialect`, `enum DriverId{Sqlite,Postgres,MySql}`.

- [ ] Step 1: Tests: `quote_ident("a\"b")` is `"a""b"` (PG/SQLite), `` `a``b` `` (MySQL); `literal(Value::Text("O'Brien"))` is `'O''Brien'` (MySQL also escapes backslash); `literal(Value::Null)` is `NULL`; bytes as `X'..'` / `'\x..'` per dialect; proptest: for any string, the quoted identifier re-parsed with `sqlparser` is a single identifier.
- [ ] Step 2: Implement; Step 3: run tests; Step 4: commit.

### Task 3: Statement splitter and safe-mode classifier

**Files:** `core/src/{splitter.rs,safemode.rs}`.

**Interfaces:**
- Produces: `split_statements(sql:&str, dialect:DriverId)->Vec<Statement{text:String, start:usize, end:usize, label:Option<String>}>` (label from `-- name:|label:|tab:` comment directly above); `enum StatementKind{ReadOnly,Write,Ddl}`; `classify(sql:&str,dialect:DriverId)->StatementKind` (multi-statement or unparsable = Write); `enum SafeLevel{Silent,Alert1,Alert2,Safe1,Safe2}` with `fn requires_confirm(self,kind:StatementKind)->bool` and `fn requires_password(self,kind)->bool`.

- [ ] Step 1: Tests: semicolon in string/comment/`$$...$$` not split; MySQL `DELIMITER //` block; label extraction; `SELECT`, `EXPLAIN`, `SHOW`, `PRAGMA table_info(x)`, `WITH x AS (SELECT 1) SELECT` are ReadOnly; `WITH x AS (DELETE ...) SELECT` is Write; `EXPLAIN ANALYZE DELETE` is Write; level matrix per SPEC 9.3.
- [ ] Step 2: Implement with a hand-rolled tokenizer for splitting and `sqlparser` for classification; Step 3: pass; Step 4: commit.

### Task 4: Filters to WHERE and pending changes to SQL

**Files:** `core/src/{filter.rs,changes.rs}`; snapshot tests with `insta`.

**Interfaces:**
- Produces: `enum FilterOp` (`Eq,Ne,Lt,Gt,Le,Ge,In,NotIn,IsNull,IsNotNull,Between,NotBetween,Like,Contains,NotContains,HasPrefix,HasSuffix,ILike`), `struct Filter{enabled,column:FilterColumn,op,value:String}`, `enum FilterColumn{Column(String),Any,Raw}`, `to_where(d:&dyn Dialect, cols:&[ColumnMeta], filters:&[Filter], or:bool)->String`; `enum RowChange{Insert{values:Vec<(String,CellValue)>},Update{key:Vec<(String,Value)>,set:Vec<(String,CellValue)>},Delete{key:Vec<(String,Value)>}}`, `enum CellValue{Value(Value),Default,Null}`, `struct ChangeSet{table:TableRef,rows:Vec<RowChange>,...}` with `undo/redo`, `generate_sql(&ChangeSet,&dyn Dialect)->Result<Vec<String>,ChangeError>` where `ChangeError::NoKey` when UPDATE/DELETE has an empty key.

- [ ] Step 1: Snapshot tests per dialect: insert with Default/Null/empty string distinct, update only changed columns, delete by composite key, `Contains` escapes `%` and `_`, `IN` list parsing, `Any` expands to OR over text-castable columns, Raw passthrough, `ILike` only on PG; `NoKey` error test.
- [ ] Step 2: Implement; Step 3: pass; Step 4: commit.

### Task 5: Connection URLs, DDL generation, copy script, copy formats

**Files:** `core/src/{url.rs,ddl.rs,copyfmt.rs}`.

**Interfaces:**
- Produces: `parse_url(&str)->Result<ConnectionSpec,UrlError>`, `format_url(&ConnectionSpec,include_password:bool)->String` (SPEC 6.3, drivers sqlite/postgres/mysql/mariadb; options `name,statusColor,env,safeModeLevel,tLSMode`, table-open options `schema,table,column,operation,value,condition`); `enum StructuralOp` (CreateTable, RenameTable, DropTable{cascade}, Truncate{cascade,restart}, AddColumn, DropColumn, AlterColumn, AddIndex, DropIndex, AddForeignKey, DropForeignKey, CloneTable{with_data}) and `generate_ddl(&StructuralOp,&dyn Dialect)->Result<Vec<String>,DdlError>` (SQLite alter-column = table rebuild per SPEC 23, FK add unsupported); `copy_script(kind:ScriptKind,&TableStructure,&dyn Dialect)->String`; `format_rows(fmt:CopyFormat,cols,rows,&CsvOptions)->String` for Plain, JSON, HTML, Markdown, CSV, CSV with header, INSERT.

- [ ] Step 1: Round-trip tests for URLs (special chars in password, missing port, unknown scheme error); snapshot tests for DDL per dialect and for every copy format; Step 2: implement; Step 3: pass; Step 4: commit.

### Task 6: Driver traits and SQLite driver (M1)

**Files:** `drivers/src/{lib.rs,traits.rs,sqlite.rs}`, `drivers/tests/sqlite.rs`.

**Interfaces:**
- Produces: `trait Driver`, `trait Conn` (SPEC 5.1 subset: `server_version, list_databases, list_schemas, list_objects, table_structure, query_stream, execute_batch, estimate_count, cancel_handle, ping, set_database, create_database, rename_database, drop_database`), `struct ConnectConfig{driver,host,port,user,password:Option<String>,database,ssl:SslMode,bootstrap:Vec<String>,..}`, `type RowSink=tokio::sync::mpsc::Sender<RowBatch>`, `struct RowBatch{columns:Option<Vec<ColumnMeta>>,rows:Vec<Vec<Value>>}`, `struct QueryOpts{limit:Option<u64>,timeout:Duration}`, `struct QuerySummary{affected:Option<u64>,elapsed:Duration,messages:Vec<String>}`, `fn driver_for(DriverId)->Box<dyn Driver>`, `SqliteDriver` (rusqlite in `spawn_blocking`, interrupt-handle cancel, `PRAGMA table_xinfo/index_list/foreign_key_list`, triggers read-only).

- [ ] Step 1: Integration tests on a temp file: create tables incl. FK + index, `list_objects`, `table_structure`, stream 10,000 rows in batches of 200 with back-pressure, cancel a long recursive CTE, `execute_batch(atomic=true)` rolls back on error, file path with spaces, nonexistent path error message.
- [ ] Step 2: Implement; Step 3: pass; Step 4: commit.

### Task 7: PostgreSQL driver with Docker tests (M5)

**Files:** `drivers/src/postgres.rs`, `drivers/tests/{support/mod.rs,postgres.rs}`.

- [ ] Step 1: `support::docker_db(image,env,port)` starts a throwaway container on a random high port, waits for readiness, removes it on drop; tests skip with a clear message if Docker is missing.
- [ ] Step 2: Tests mirror Task 6 plus: schemas listing, enum/array/json/uuid/timestamptz values map to `Value`, `pg_cancel_backend` via side connection cancels `pg_sleep(30)`, TLS mode `DISABLED/PREFERRED` handled, estimate count from `pg_class`, bootstrap commands run.
- [ ] Step 3: Implement `PostgresDriver` (tokio-postgres, rustls); Step 4: pass; Step 5: commit.

### Task 8: MySQL/MariaDB driver with Docker tests (M5)

**Files:** `drivers/src/mysql.rs`, `drivers/tests/mysql.rs`.

- [ ] Same shape as Task 7 with `mysql:8` and `mariadb:11` images: `KILL QUERY` cancel through a side connection, `information_schema` introspection, column reorder capability true, rename database unsupported, `LIMIT` handling, bootstrap. Commit.

### Task 9: Store (settings, connections, history, queries, secrets)

**Files:** `store/src/*`.

**Interfaces:**
- Produces: `atomic_write(path,&[u8])`, `Settings` (serde, `schema_version`, SPEC 14.7 defaults), `ConnectionStore{list,save,delete,duplicate,groups,tags}`, `StoredConnection{id,name,driver,host,port,user,database,ssl,safe_level,status_color,tag,password_mode:PasswordMode,bootstrap,group}`, `HistoryStore::{append,search,clear}` (JSONL per connection), `QueryStore` (plain `.sql` files with folders), `trait SecretStore{get,set,delete}` with `KeyringStore` and `MemorySecretStore` (tests only), corrupt-file backup `.bak` + defaults with a returned warning.

- [ ] Step 1: Tests with temp dirs: atomic write survives interrupted temp file, corrupt JSON falls back and keeps `.bak`, no secret ever appears in `connections.json`, history search, saved query folders. Step 2: implement; Step 3: pass; Step 4: commit.

### Task 10: Session actor and safe-mode gate

**Files:** `session/src/{actor.rs,commands.rs,gate.rs}`; tests with SQLite.

**Interfaces:**
- Produces: `enum Command` / `enum Event` (SPEC 25 subset: Connect, Disconnect, Reconnect, Reload, UseDatabase, LoadSchema, LoadStructure, FetchPage, CountExact, RunSql, Commit, Cancel, DbOp, Export/Import hooks later), `RequestId`, `SessionHandle{send(Command), events:Receiver<Event>}` calling a `repaint: Arc<dyn Fn()+Send+Sync>` after each event, `SafeModeGate::check(level,&[String],&dyn Fn()->Option<String> password)->GateDecision{Allow,NeedConfirm(Vec<String>),NeedPassword(Vec<String>)}`, keep-alive ping every 30 s, query timeout 300 s emitting `Event::Timeout`, console line event per statement with password redaction.

- [ ] Step 1: Tests: page fetch emits batches then `QueryDone`; cancel; commit error keeps nothing applied and emits `CommitError`; gate matrix; console redaction of `password`. Step 2: implement; Step 3: pass; Step 4: commit.

### Task 11: App shell, widget kit, icons, welcome and connection form (M0/M1)

**Files:** `app/src/{glyphs.rs,keymap.rs,cli.rs}`, `ui/{mod,widgets,welcome,connform,dialogs,settings}.rs`.

**Interfaces:**
- Produces: widget kit per skill table (`heading, muted, field_label, primary_button, danger_button, ico_button, card, input, secret_input, removable_chip, icon_badge, modal, draw_toasts`), `Toasts`, `Env`, `enum Nav`, `enum View`, `Ico` enum with at least Search, Close, Plus, Table, View, Function, Database, Key, Lock, Eye, EyeOff, Copy, Refresh, Play, Stop, Commit, Discard, Preview, Settings, Folder, Filter, Chevron, Export, Import, Bolt; `Action` enum + `KeyMap` (defaults per SPEC 18.2 for implemented actions), CLI parsing of a URL or sqlite file argument (FR-C-021 without the single-instance socket).

- [ ] Step 1: Unit tests: `Nav` application order, `KeyMap` conflict detection, CLI arg to `ConnectionSpec`. Step 2: build welcome (list, groups, tags, search, context menu, Copy as URL, Import from URL), connection form (all SPEC 6.2 fields for the three drivers, password modes Keychain/Ask/None/Command, SSL modes, bootstrap, Test/Connect/Save), settings window (General, Table, Editor, Security default safe mode; no themes). Step 3: build, clippy, screenshot `docs/screens/m1-welcome.png`; Step 4: commit.

### Task 12: Workspace layout and sidebar (M1)

**Files:** `ui/{workspace,sidebar,tabs,console,palette}.rs`.

- [ ] Rail + sidebar (Items/Queries/History, fuzzy search, schema menu when `caps.schemas`, `+` menu, item context menu, pin) + tab strip (`egui_dock` if compatible else own) with preview-tab semantics (SPEC 7.6) + status bar with status colour and popover + console log (1000 entries, clear) + Open Anything (`Mod+P`) over objects and actions. Unit tests for the preview-tab decision function and fuzzy matcher. Screenshot against a seeded SQLite file `docs/screens/m1-workspace.png`. Commit.

### Task 13: Table Data tab (M1)

**Files:** `ui/{grid,quicklook}.rs`.

- [ ] `egui_extras::TableBuilder` virtualized grid fed by streamed batches; paging (limit/offset, `Mod+Left/Right`), sort cycle desc/asc/none, multi-column sort, hide column, selection (row, range), NULL/empty/BLOB rendering, FK arrow opening the referenced table filtered, estimated count with exact Count button, Quick Look (text, JSON raw/tree, hex for BLOB) with truncation of cells above 64 KiB in the grid. Pure functions with tests: `next_sort_state`, `page_bounds`, `display_cell(&Value)->CellView`. Verify with a 1,000,000-row SQLite table that the UI thread stays responsive (frame time logged below 16 ms median). Screenshot `docs/screens/m1-data.png`. Commit.

### Task 14: Pending changes UI and Details sidebar (M2)

**Files:** `ui/{details,grid}.rs` extensions, toolbar Discard/Preview/Commit control.

- [ ] Inline cell edit, insert/duplicate/delete rows, Set Value menu (EMPTY, NULL, DEFAULT, NOW()), undo/redo, row state colours, read-only badge for tables without a key (with key-column chooser), Details panel with per-column fields and multi-row edit, Preview popover (generated SQL), Commit through `SafeModeGate` (modals for confirm/password), Discard reloads tab and clears undo, History append on commit, "Log original values" option. Tests: a headless test of the edit-to-SQL path using `ChangeSet` and a real SQLite file (edit, preview text equals expectation, commit, reload shows data). Screenshot. Commit.

### Task 15: SQL editor (M3)

**Files:** `ui/{sqleditor,results}.rs`, highlight module.

- [ ] Highlighted `TextEdit` via cached `LayoutJob` (per-dialect keywords), line numbers, current-statement highlight, Run Current / Run All, row-limit menu (appends `LIMIT` only to limit-less `SELECT`), cancel, result tabs per statement with `-- name:` labels and 100-statement collapse, pin, Data/Message views (editable-result rule: only when single table with key), streaming append, copy formats on results, `Mod+/` comment toggle, find, font size, beautify with `sqlformat`. Tests: limit injection function, tab naming, comment toggle function. Commit.

### Task 16: History, saved queries, autocomplete (M3)

**Files:** sidebar tabs, `ui/sqleditor.rs` completion popup.

- [ ] History tab (search, run, copy, save as, add to queries, clear), Queries tab (folders, create from editor, keywords, rename, delete, auto-save), context-aware completion (after `FROM/JOIN` tables, after `alias.` columns, select list columns; keywords; saved query keywords) with options from settings. Pure function tests `completion_context(sql,cursor)->Context`. Commit.

### Task 17: Structure, Index, table operations (M4)

**Files:** `ui/structure.rs`, dialogs.

- [ ] Structure grid (edit/insert/delete column, data type dropdown from dialect, default menu, NOT NULL, Primary field, FK popover disabled when `!caps.add_fk_after_create`, drag reorder only when `caps.reorder_columns`), Index tab, Triggers popover, DDL view, create/rename/clone/truncate/drop table dialogs, views and functions editors (DDL tab, read-only materialized views), all via `StructuralOp` into the pending pipeline and commit. Tests: structural ops end to end on SQLite (create table in GUI model, commit, introspect equals). Commit.

### Task 18: Filters and Search in Database (M4)

**Files:** `ui/filters.rs`, search dialog.

- [ ] Filter bar (column/operator/value, + and -, checkbox, Apply, Apply All, OR menu, Clear, SQL popover, quick filters from header/cell menus, settings menu, column filter), Search in Database with pause/resume and result list opening the table with an Any-column filter. Tests: filter state to `Vec<Filter>` conversion, search iterator pause/resume logic. Commit.

### Task 19: PostgreSQL and MySQL in the UI, safe mode, database list (M5)

**Files:** `ui/{connform,dialogs}.rs`, session wiring.

- [ ] Wire PG/MySQL end to end (SSL modes, password modes incl. Ask and Command, bootstrap, keep-alive/timeout reconnect-or-wait dialog), Safe Mode toolbar button and level change with password, database list (`Mod+K`: open, new, rename when supported, drop with typed-name confirmation), Copy as URL / Import from URL, cancel via side connection. Docker-backed end-to-end test (headless session) for edit and commit on both servers. Screenshots of PG and MySQL workspaces. Commit.

### Task 20: Export and Import (M7 core)

**Files:** `io/src/{export.rs,import.rs}`, `ui/dialogs.rs` wizards, notifications bell.

**Interfaces:**
- Produces: `export_table(...)` to CSV (all SPEC 12.2 options), JSON array of objects, SQL (structure/drop/data, gzip), XLSX; `ImportPlan` + `preview_csv(path,&CsvImportOptions)->Preview(100 rows)`, `match_columns(strategy,...)`, `run_import(...)` batched 500 rows in a transaction with `InsertMode` (StopOnError/UpdateOnDuplicate/Replace per caps), progress events, cancel; SQL dump import preview 100,000 chars.

- [ ] Step 1: Round-trip tests on SQLite and Postgres: export CSV/JSON/SQL then import into a fresh table gives equal data; NULL vs empty string preserved; CSV with delimiter/quote variants and CRLF; gzip file readable. Step 2: implement; wizards in UI with background jobs and notifications; Step 3: pass; Step 4: commit.

### Task 21: MCP server (M10 subset)

**Files:** `mcp/src/*`, `ui/mcp_settings.rs`, stdio bridge bin `dbzilla-mcp`.

**Interfaces:**
- Produces: Streamable HTTP at `http://127.0.0.1:51003/mcp` with Bearer tokens (constant-time compare); tools `list_connections, list_open_connections, open_connection, disconnect, execute_read_query, execute_query` (SPEC 16); token store (name, permission Read Only/Read & Write/Full Access, connection scope, shown once, revoke/delete) in the keychain trait; every query through `SafeModeGate` + `classify`; approval request channel to the UI ("Approve AI Agent Query", Allow/Deny); activity log `mcp_activity.jsonl`; 300 s max duration; bind localhost only.

- [ ] Step 1: Tests: permission matrix (read-only token cannot `execute_query`, writes refused for ReadOnly, scoped tokens only see allowed connections), revoked token 401, constant-time compare helper, approval deny path, `execute_read_query` refuses a DML statement. Step 2: implement with `axum` and hand-rolled JSON-RPC if `rmcp` does not fit; Step 3: pass; Step 4: manual client check with `curl` against a SQLite connection; Step 5: settings UI (enable, port, tokens, activity window); Step 6: commit.

### Task 22: Polish and final verification

- [ ] Panels toggles/shortcuts from SPEC 18.2 that exist, tooltips with shortcuts, destructive confirms audit, README with run instructions, `CHANGES.md` final, `docs/screens/` refreshed. Run `cargo build --release`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` (with Docker), and launch the release binary once. Request a whole-branch review via a fresh reviewer. Commit.
