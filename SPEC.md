# SPEC — Native Multi-Database GUI Client in Rust + egui

> **Audience:** an LLM coding agent that will implement the application.
> **Source of truth:** the public TablePlus documentation (https://tableplus.com/docs, ~70 pages read in full). This document turns that documentation into an implementable specification for a **similar** application written in **Rust with egui/eframe**.
> **Legend**
> - **[DOC]** behaviour taken directly from the reference documentation.
> - **[DESIGN]** decision added by this spec because the documentation is silent or the egui/Rust stack requires it.
> - Requirement IDs (`FR-xxx`, `NFR-xxx`) are stable; reference them in commits and tests.
> - `Mod` = `Cmd` on macOS, `Ctrl` on Windows/Linux. In egui this is `Modifiers::COMMAND`.

---

## 0. Instructions to the implementing agent

1. **Do not clone branding.** Use a neutral product name (placeholder: `RowLens`; change freely). Do not reuse the reference product's name, logo, icons, theme names as marketing, or copy documentation text verbatim into the UI.
2. **Build in the milestones of §22.** Each milestone must compile, pass tests and be demoable before the next starts. Do not scaffold everything at once.
3. **Keep UI and I/O strictly separated** (§3). The UI thread never awaits a database call. No blocking I/O in `update()`.
4. **Everything that mutates data goes through the pending-change pipeline** (§9) except SQL typed in the query editor.
5. **Drivers are capability-driven** (§5). Never `match driver { ... }` inside UI code; ask `driver.capabilities()`.
6. **Prefer small crates behind traits** so drivers and features can be added without touching UI code.
7. When a requirement says "reference default", use exactly that value unless the user changes it in settings.
8. Write tests as you go (§21). A milestone is not done without them.

---

## 1. Product overview

A fast, native, cross-platform (Windows/Linux/macOS) desktop client to manage many databases at once. Core promises **[DOC]**:

- Direct connection from the user's machine to the database. **Nothing is sent to any vendor server**; credentials stay on the device, passwords in the OS keychain.
- Browse and edit table data like a spreadsheet, with **pending changes** that are only sent after explicit **Commit**.
- Powerful SQL editor with autocomplete, formatting, parameters, streaming results.
- Safe-by-default tooling for production databases (safe-mode levels, SQL preview, tags/colors).
- Import/export, backup/restore, ER diagram, metrics board, user management, process list.
- Optional AI assistant and an embedded MCP server.

### 1.1 Supported databases

Reference list **[DOC]**: PostgreSQL, Amazon Redshift, MySQL, MariaDB, SingleStore, Microsoft SQL Server, Cassandra, ClickHouse, BigQuery, DynamoDB (beta), Elasticsearch, libSQL, Cloudflare D1, MongoDB, Snowflake, Redis, SQLite, DuckDB, Oracle, CockroachDB, Greenplum, Vertica. Wire-compatible databases (Percona, YugabyteDB…) work through the base driver.

**[DESIGN] Scope for this implementation**

| Tier | Drivers | Milestone |
|---|---|---|
| 1 (must) | SQLite, PostgreSQL, MySQL/MariaDB | M1–M3 |
| 2 (should) | DuckDB, Microsoft SQL Server, CockroachDB/Redshift/Greenplum (Postgres-protocol), ClickHouse | M8 |
| 3 (optional, document-style) | Redis, MongoDB | M9 |
| Out of scope | Oracle, Snowflake, BigQuery, DynamoDB, Cassandra, Elasticsearch, libSQL, D1, Vertica | — (design the trait so they can be added) |

### 1.2 Non-goals

- Licensing/trial limits, license server, payment, update server, beta channel, MDM deployment, crash-report upload. (The reference free tier limits to 2 tabs/windows/filters and 1 metrics board — **do not implement**.)
- iOS/Android.
- JavaScript plugin system, custom scripts, multi-window, light/Auto themes, multi-caret editing, split panes and any team/sharing feature (shared connection sources, shared query folders). Removed from scope for good.

---

## 2. Technology stack **[DESIGN]**

| Concern | Choice | Notes |
|---|---|---|
| Language / edition | Rust stable, edition 2021+ | `#![forbid(unsafe_code)]` in app crates where possible |
| UI | `eframe` + `egui` (latest stable) | glow or wgpu backend; enable `persistence` feature |
| Tables | `egui_extras::TableBuilder` (virtualized rows) | custom selection layer on top |
| Docking / tabs | `egui_dock` | tabs, workspaces |
| Charts | `egui_plot` | line, bar, pie (pie painted manually) |
| Async runtime | `tokio` (multi-thread, 2–4 workers) | UI ↔ runtime via channels |
| SQLite | `rusqlite` (bundled) wrapped in `spawn_blocking` | |
| PostgreSQL | `tokio-postgres` + `postgres-native-tls` or `rustls` | |
| MySQL/MariaDB | `mysql_async` | |
| MSSQL (T2) | `tiberius` | |
| DuckDB (T2) | `duckdb` crate | |
| ClickHouse (T2) | `clickhouse` or HTTP via `reqwest` | |
| Redis / Mongo (T3) | `redis`, `mongodb` | |
| SSH tunnel | `russh` + `russh-keys` (+ parse `~/.ssh/config` with `ssh2-config`) | built-in native client, no external `ssh` process |
| TLS | `rustls` (+ `rustls-native-certs`) | client cert/key/CA file pickers |
| Keychain | `keyring` crate | Keychain / Credential Manager / Secret Service |
| Config | `serde`, `serde_json`, `directories` | JSON files, atomic writes |
| SQL parsing (statement boundaries) | `sqlparser` | dialect per driver; fall back to a hand-rolled tokenizer for unterminated input |
| SQL formatting | `sqlformat` (or own formatter) | indent width/type options |
| Syntax highlight | custom `LayoutJob` with own tokenizer, or `syntect` | cache per text hash |
| CSV | `csv` | |
| JSON | `serde_json` | |
| Excel export | `rust_xlsxwriter` | |
| Compression | `flate2` (gzip) | |
| HTTP (LLM, MCP-HTTP) | `reqwest`, `axum` | |
| MCP | `rmcp` (official Rust SDK) or hand-rolled JSON-RPC over stdio + Streamable HTTP | |
| Errors / logging | `thiserror`, `anyhow`, `tracing` | |
| Encryption for connection export | `argon2` + `aes-gcm` | |
| Tests | `cargo test`, `insta` (snapshots), `testcontainers` (PG/MySQL), `proptest` | |

**Workspace layout (cargo workspace):**

```
app/                 # bin: eframe entry, wiring
crates/
  core/              # domain types, Value, ChangeSet, SQL builders, safe-mode classifier (no UI, no I/O)
  drivers/           # trait + sqlite, postgres, mysql, ... (feature-gated)
  ssh/               # tunnel
  editor/            # SQL editor widget (highlight, completion popup)
  ui/                # egui views: welcome, workspace, grid, structure, filters, wizards...
  store/             # settings, connections, history, saved queries, keychain
  mcp/               # MCP server
  llm/               # assistant providers
  tests-support/
```

---

## 3. Architecture **[DESIGN]**

### 3.1 Threading and messaging

```
 UI thread (egui)                    Tokio runtime (workers)
 ───────────────                     ───────────────────────
 AppState ──Command──► mpsc ───────► SessionActor (1 per open connection)
    ▲                                    │  owns driver connection(s)
    └────── Event ◄─── mpsc ◄────────────┘  streams RowBatch, Progress, Done, Error
 (every send is followed by ctx.request_repaint())
```

- One **SessionActor** per open connection (per workspace). It owns: the primary connection, optionally a **side connection** (process list, metrics, schema introspection) so the UI stays usable while a long query runs **[DOC: process list "uses its own connection"]**.
- Commands carry a `RequestId`; events echo it. Cancelling = send `Cancel(RequestId)`; actor calls driver-level cancel (see §5.3).
- Results **stream**: the actor sends `RowBatch(Vec<Row>)` events (e.g. every 200 rows or 50 ms). The grid appends and repaints as they arrive **[DOC: Streaming results]**.
- State shared with UI (schema cache, settings) is `Arc<RwLock<_>>`; UI reads only.

### 3.2 Core domain types (sketch)

```rust
pub enum Value {
    Null, Bool(bool), I64(i64), U64(u64), F64(f64), Decimal(String),
    Text(String), Bytes(Vec<u8>), Json(serde_json::Value),
    Date(chrono::NaiveDate), Time(chrono::NaiveTime),
    DateTime(chrono::NaiveDateTime), DateTimeTz(chrono::DateTime<chrono::FixedOffset>),
    Uuid(uuid::Uuid), Array(Vec<Value>), Geometry(String /*WKT or hex*/), Other(String),
}

pub struct ColumnMeta { pub name: String, pub type_name: String, pub nullable: bool,
    pub default: Option<String>, pub is_pk: bool, pub fk: Option<ForeignKeyRef>,
    pub ordinal: usize, pub extra: Map<String,String> }

pub struct ResultSet { pub columns: Vec<ColumnMeta>, pub rows: Vec<Row>,
    pub affected: Option<u64>, pub elapsed: Duration, pub messages: Vec<Message> }

pub struct TableRef { pub database: Option<String>, pub schema: Option<String>, pub name: String }
```

### 3.3 Application model

```
App
     └─ Workspaces[]                (one per open database; "database stack" on the far left)
         ├─ Session (connection handle, db name, schema cache, safe mode, pending ChangeSets)
         └─ Dock<Tab>               (tabs)
              Tab = TableData | TableStructure | TableIndex | ViewDdl | FunctionEditor
                  | SqlEditor | SavedQueryEditor | ErDiagram | MetricsBoard | ...
```

- A **Window** hosts one or more **Workspaces**; a **Workspace** is an open database; a **Tab** is an open object **[DOC: Multi Tabs / Workspaces / Windows]**.

---

## 4. Persistence and data locations **[DESIGN]**

Use `directories::ProjectDirs`. Files (all JSON, written atomically via temp+rename, versioned with `"schema_version"`):

| File | Content |
|---|---|
| `settings.json` | all preferences (§16) |
| `connections.json` | local connection source (no secrets) |
| `tags.json` | tag names + colors |
| `history/<connection_id>.jsonl` | query history |
| `queries/` | saved queries (`*.sql`, folders allowed) |
| `metrics_boards/<id>.json` | metrics boards |
| `themes/*.json` | custom themes |
| `mcp_activity.jsonl` | MCP request log |
| `session.json` | last-session workspaces for "reopen at startup" |

**Secrets** (DB password, SSH password, SSH key passphrase, API keys, MCP tokens, custom-agent env) → OS keychain via `keyring`, key = `<app>/<connection_id>/<field>`. **Never** in JSON.

---

## 5. Driver abstraction

### 5.1 Trait

```rust
#[async_trait]
pub trait Driver: Send + Sync {
    fn id(&self) -> DriverId;
    fn capabilities(&self) -> &Capabilities;
    fn dialect(&self) -> Dialect;                       // quoting, params, keywords, formatter

    async fn connect(&self, cfg: &ConnectConfig) -> Result<Box<dyn Conn>>;
}

#[async_trait]
pub trait Conn: Send {
    async fn server_version(&mut self) -> Result<String>;
    async fn list_databases(&mut self) -> Result<Vec<DatabaseInfo>>;
    async fn list_schemas(&mut self, db: &str) -> Result<Vec<String>>;
    async fn list_objects(&mut self, db:&str, schema:Option<&str>) -> Result<Objects>; // tables, views, mat-views, functions, procedures, sequences, types
    async fn table_structure(&mut self, t:&TableRef) -> Result<TableStructure>;        // columns, pk, fks, indexes, triggers, ddl
    async fn query_stream(&mut self, sql:&str, opts:QueryOpts, tx: RowSink) -> Result<QuerySummary>;
    async fn execute_batch(&mut self, stmts:&[String], atomic: bool) -> Result<Vec<u64>>; // used by commit
    async fn estimate_count(&mut self, t:&TableRef) -> Result<Option<u64>>;
    async fn cancel_handle(&self) -> Box<dyn CancelToken>;
    async fn ping(&mut self) -> Result<()>;
    async fn set_database(&mut self, db:&str) -> Result<()>;
    async fn create_database(&mut self, req:&NewDatabase) -> Result<()>;
    async fn rename_database(&mut self, from:&str, to:&str) -> Result<()>;
    async fn drop_database(&mut self, name:&str) -> Result<()>;
    async fn process_list(&mut self, db:Option<&str>) -> Result<ResultSet>;      // if supported
    async fn kill(&mut self, ids:&[String]) -> Result<()>;
    async fn users(&mut self) -> Result<Vec<DbUser>>;                             // if supported
}
```

`RowSink` is a bounded `tokio::sync::mpsc::Sender<RowBatch>`; the driver must `await` on send (back-pressure) so huge results do not exhaust memory.

### 5.2 Capabilities struct (drive all UI enable/disable decisions)

```rust
pub struct Capabilities {
    pub schemas: bool,                     // schema menu in sidebar
    pub databases_list: bool,
    pub create_db: bool, pub rename_db: bool, pub drop_db: bool,
    pub encoding_collation_on_create_db: bool,
    pub functions: bool, pub procedures: bool, pub views: bool, pub materialized_views: bool,
    pub sequences: bool, pub user_types: bool,
    pub triggers: bool, pub index_tab: bool,
    pub alter_column: bool, pub reorder_columns: bool,           // MySQL/MariaDB/ClickHouse
    pub add_fk_after_create: bool,                               // false for SQLite
    pub truncate_options: TruncateOpts,                          // cascade, restart identity
    pub drop_table_options: DropOpts,                            // cascade
    pub backup_restore: Option<BackupKind>,                      // Postgres/MySQL families
    pub process_list: bool, pub user_management: bool,
    pub ssl_modes: Vec<SslMode>,
    pub filter_operators: Vec<FilterOp>,                         // incl. ILIKE on PG
    pub insert_conflict_modes: Vec<InsertMode>,                  // StopOnError, UpdateOnDuplicate, Replace
    pub estimated_count: bool,
    pub query_timeout_supported: bool,                           // false for SQLite/DuckDB/…
    pub result_paging_native: bool,                              // BigQuery-style "Load more"
    pub document_model: bool,                                    // Mongo/Redis/Elastic/Dynamo: Data/Json/Tree tabs
    pub transactions: bool,
}
```

Reference values **[DOC]**: rename-db unsupported on MySQL/MariaDB/Redis/Cassandra; index tab unsupported on Redis/Cassandra/Snowflake/BigQuery/ClickHouse/Elasticsearch; triggers on MySQL/MariaDB/PostgreSQL/Redshift; column reorder on MySQL/MariaDB/ClickHouse; process list on PostgreSQL(+Greenplum/Redshift)/MySQL/MariaDB/SQL Server; user management on PostgreSQL/MySQL/MariaDB; backup/restore on PostgreSQL/MySQL/MariaDB and PG-compatible; SQLite cannot add/remove FKs on existing tables; materialized views for PostgreSQL/Redshift/Cassandra and are read-only.

### 5.3 Cancellation

| Driver | Mechanism |
|---|---|
| PostgreSQL | second connection: `SELECT pg_cancel_backend(pid)` **[DOC]** (or protocol-level `CancelToken`) |
| MySQL/MariaDB | second connection: `KILL QUERY <id>` **[DOC]** |
| SQL Server | `KILL <spid>` — ends session and rolls back its transaction **[DOC]** |
| SQLite | `Connection::get_interrupt_handle().interrupt()` |
| Others | best effort; otherwise drop connection and reconnect |

### 5.4 Dialect object

Provides: identifier quoting, string literal escaping, `LIMIT/OFFSET` syntax, `RETURNING` support, type list for the "data_type" dropdown, keyword list for highlight/autocomplete/format, default parameter style, `SELECT top 100` template, `CREATE/DROP/TRUNCATE` templates (for **Copy Script As** §8.10).

---

## 6. Connections

### 6.1 Welcome window **[DOC]**

- Left column: **Create Connection**, **Backup Database**, **Restore Database**. Right: the saved-connection list (groups as folders, sources as sections).
- Search box (`Mod+F`) filters by name.
- Double-click a connection opens it. Right-click menu: **Connect, New, Edit…, Duplicate, Copy as URL, Sort By (Tag | Name | Driver), Import Connection, Export Connections, Delete**.
- "New" submenu: Connection…, Group…, Import Connection > From URL… / From File….
- **FR-C-001** The welcome window is shown at start unless "Reopen closed workspaces at startup" (§16) is on and a session exists.

### 6.2 Connection form **[DOC]**

Top: **Name**, **Status Color**, **Tag**, **Safe Mode**.
Body (driver-dependent): Host, Port (default per driver), User, Password, Database (or file path for SQLite/DuckDB), SSL section, Over SSH section, Bootstrap commands.
Buttons: **Test**, **Connect**, **Save** (save without connecting).

- **FR-C-010 Password storage menu**: `Store in keychain` (default) | `Ask every time` | `No password` | `Command Line` (run a shell command at connect time; stdout is the password/token; for PostgreSQL, MySQL, MariaDB, Redshift; for SQL Server Entra-ID token mode, "Get from CLI").
- **FR-C-011 Bootstrap commands**: free-text SQL executed right after the connection is established, one statement per line/`;`.
- **FR-C-012 SSL**: mode per driver. PostgreSQL: `PREFERRED, DISABLED, REQUIRED, ALLOW, VERIFY-CA, VERIFY-FULL`. MySQL: `PREFERRED, DISABLED, REQUIRED, VERIFY-CA, VERIFY-IDENTITY`. File pickers: **Key…**, **Cert…**, **CA Cert…**.
- **FR-C-013 SSH tunnel** (built-in client): server, port, user, password (same storage menu as DB password) or **Use SSH key** with private-key file (not `.pub`). If "Use SSH key" is ticked and key path empty → use `~/.ssh/config` (IdentityFile, ProxyJump, User, Port…). Local random port forwarded to remote host:port; DB driver connects to `127.0.0.1:<local>`.
- **FR-C-014 Connection tags**: defaults `local, testing, development, staging, production` + "NO TAG". Users add/remove custom tags and change colors; default tags can't be removed (only recolored; "Default Color" resets).
- **FR-C-015 Status color** paints the workspace status bar so prod/staging are distinguishable.
- **FR-C-016** Test connection reports success/failure with the server error text and server version.

### 6.3 Connection URL **[DOC]**

```
driver://user:password@host:port/database?option=value&option=value
driver+ssh://ssh_user:ssh_password@ssh_host:ssh_port/user:password@host:port/database?option=value
```

All values URL-encoded; omit port for the driver default.
Schemes: `postgresql|postgres, mysql, mariadb, sqlserver|microsoftsqlserver, redshift, cockroach, greenplum, vertica, oracle, clickhouse, cassandra, redis, mongodb, elasticsearch, duckdb, d1|cloudflared1, sqlite`; (import-only: `snowflake, bigquery, libsql`). Implement for the drivers you ship; unknown scheme → friendly error.

Options:

| Option | Meaning |
|---|---|
| `name` | connection name |
| `statusColor` | hex without `#` |
| `env` | tag name |
| `windowMode` | `isolated` (new window, default) / `tabbed` |
| `tLSMode` | SSL mode (enum index) |
| `usePrivateKey` | use SSH key |
| `safeModeLevel` | 0–2 |
| `advancedSafeModeLevel` | 0 or 1 |
| `driverVersion` | driver version |
| `lazyload` | lazy-load schema items |

**Open a table with a filter** (extra options): `schema`, `table` (`view`/`item` aliases), `column`, `operation` (`operator` alias), `value`, `condition` (`raw`/`query` aliases; a raw SQL WHERE used instead of column/operation/value). The app connects, opens the table in a new tab and applies the filter.

- **FR-C-020** Implement **Copy as URL** (includes passwords and options) and **Import from URL…** (fills the form, lets user review).
- **FR-C-021 [DESIGN]** Register the app as handler for its own URL scheme(s) where the OS allows, and accept the URL (or a `.sqlite/.db/.sqlite3` file path) as a **CLI argument**: `app "postgresql://postgres@127.0.0.1/acme?schema=public&table=orders&column=status&operation=Contains&value=shipped"`. A second invocation forwards to the running instance (single-instance via local socket).
- **FR-C-022** Double-clicking / "Open with" a SQLite file opens it directly.

### 6.4 Saved connections, groups, sorting **[DOC]**

- Edit (from list, or via click on status bar → popover with **Edit / Reconnect / Disconnect**), Duplicate, Delete.
- **Groups**: create (name + optional icon), nest, drag connection in/out, edit, delete (deleting a group deletes its connections — confirm!).
- **Keep connection alive**: ping every **30 s** (default on). **Query time-out**: default **300 s**; checked together with keep-alive; on expiry show "The server is taking too long to respond" with **Reconnect** / **Wait**; not applicable to SQLite, DuckDB, BigQuery, libSQL, D1, Snowflake.
- **Switch connection**: `Mod+Shift+K` opens the connection list from inside a workspace; default opens in a **new workspace in the same window**.

### 6.5 Import/export connection files **[DOC]**

- Own file extension (placeholder `.rowlens-connection`). Export scopes: all (in the clicked section), group, single connection.
- Options: **Include databases password**, **Include servers (SSH) password**; **file password** to encrypt (`argon2` + `aes-gcm`).
- Group names/icons included. **SSH and SSL private-key files are NOT included** — user re-selects after import.
- Double-click a file in the OS opens the import dialog.

### 6.6 Command-line integration — see FR-C-021.

### 6.7 Reopen last session **[DOC]**

Setting "Reopen closed workspaces at startup": on quit, persist open workspaces (connection id + database + open tab descriptors, no secrets); restore at launch.

---

## 7. Workspace window layout

### 7.1 Regions **[DOC: The Interface]**

```
┌ Menu bar ───────────────────────────────────────────────────────────────────┐
│ Toolbar: [Left] [Connection] [Discard|Preview|Commit] [SafeMode] [Database] │
│          [SQL] ····· [Status bar] ····· [Notifications] [Reload] [Metrics]  │
│          [Command] [Panels]                                                 │
├────────┬───────────────┬──────────────────────────────────┬─────────────────┤
│Database│ Left sidebar  │ Tabs (dock, splittable)          │ Right sidebar   │
│stack   │ Items|Queries │  ┌────────────────────────────┐  │ Details|Assistant│
│(icons) │ |History      │  │ content (grid / editor …)  │  │ row editor       │
│        │ search, +     │  └────────────────────────────┘  │                  │
│        │ schema menu   │  bottom bar: Data|Structure|Index│                  │
├────────┴───────────────┴──────────────────────────────────┴─────────────────┤
│ Console log (collapsible)                                                    │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **FR-W-001** All panels toggle: left `Mod+0`, right `Space` (when grid focused), console `Mod+Ctrl+C` (use a free key on non-mac), query-results pane `Ctrl+\``.
- **FR-W-002** Menu bar (in-app egui `TopBottomPanel`; optionally native via `muda`). See §18.1 for full menu content.

### 7.2 Toolbar **[DOC]**

Default items left→right: **Left**, **Connection** (`Mod+Shift+K`), **Discard / Preview / Commit** (3-part control, disabled when no pending changes), **Safe Mode** (icon shows level), **Database** (`Mod+K`), **SQL File** (`Mod+E`), **Status** (status bar), **Notifications**, **Reload** (`Mod+R`), **Metrics**, **Command** (`Mod+P`), **Panels**.
Optional items: **Connect** (menu: open DB / open connection / open connection in new window), **User** (PG/MySQL/MariaDB), **Workspaces** (show all, `Mod+Shift+\`), flexible space.
- **FR-W-010** Toolbar is customizable (right-click → Customize Toolbar…, also in Open Anything): drag items in/out, restore default set.
- **FR-W-011 Status bar** shows connection + database, tag (e.g. `PRODUCTION`), progress bar while a query runs, colored with the status color. Click → popover (server, host, database, user, TLS; edit status color; Edit/Reconnect/Disconnect). If dropped, click reconnects.
- **FR-W-012 Notifications** bell: lists running/finished exports and imports (progress, stop, "Open Folder"), update notices (omit). 

### 7.3 Left sidebar **[DOC]**

Three tabs: **Items** (tables, views, functions, procedures, …), **Queries** (saved), **History**.
- Search box at top of each tab; `Mod+Shift+F` focuses it. Default **fuzzy** matching; option "Contains".
- Search-box settings menu: Matching strategy, **Search in Database…**, **Settings** submenu:
  `Show recently section`, `Show function section`, `Show sequence section`, `Show user defined type section`, `Show table columns` (+ sort A-Z / ordinal), `Load system schemas`, `Lazy load item in schema`.
- Bottom: **schema menu** (when `caps.schemas`), **+** button (new table; arrow menu: view, materialized view, function/procedure, schema, group).
- Item context menu: Open in new tab, Open structure, Item overview, Show diagram, Copy name, **Copy Script As** (Creation / Drop / Truncate / Select top 100), Export / Import, Clone…, Truncate…, Delete…, **Pin to top / Unpin from top**.
- Keyboard: `Space` opens selected item, `Delete`/`Mod+Delete` deletes selected items, `Alt+Delete` truncates selected tables.
- **Database stack** (far-left strip) appears when ≥2 workspaces share a window: one icon per open DB; settings: show database name / show connection name; context menu: close, close others, **Move tab to new window**.

### 7.4 Right sidebar **[DOC]**

Tabs **Details** (row editor) and **Assistant** (§15). Details: one field per column for the selected row; editing several selected rows sets the field on all. Field button → quick values (`NULL, DEFAULT, EMPTY, NOW()`; for text/JSON: `PRETTY`, `MINIFY`; `Add a file…`). Search box ("Search for field…") with fuzzy/contains; setting **Auto Pretty JSON**; setting **Enable LLM Chat**.

### 7.5 Console log **[DOC]**

Bottom panel logging **every statement sent to the server**, including background introspection, each with timestamp. Keeps latest **1,000** entries; very large statements replaced by a short note. Options: *Enable syntax highlighting*, *Clear*.

### 7.6 Tabs, workspaces, windows **[DOC]**

- Opening an object: if the current tab has pending changes or is a SQL editor → keep it and open the object in an empty/other clean table-like tab, else new tab. If the current tab is clean → replace it ("preview tab" behaviour). **Double-click** the item to pin (keep) the tab; context menu **Open in new tab** always opens a new one.
- Tab context menu: New Tab, Close Tab, Close other Tabs, Close Tabs to the Right, Close all Tabs, **Rename Tab**.
- `Mod+T` new tab (default: SQL editor; setting "Command+T to open": SQL Query Editor | Empty tab), `Mod+W` close, `Mod+]`/`Mod+[` next/prev tab, `Mod+1…9` jump.
- Opening another **database** → new workspace in the same window by default (`Mod+K`, double-click; or "Open in new Tab"/"Open in current Tab" to replace). Opening another **connection** → new workspace in same window; "Open in new Window" or `Mod+N`.
- Workspace switching: `Mod+Shift+]`/`[`, overview `Mod+Shift+\`.

### 7.7 Open Anything (command palette) **[DOC]**

`Mod+P` / toolbar **Command**. Fuzzy search over: databases, schemas, tables/views/materialized views, functions/procedures, saved queries, **app features**.
- `Enter` opens; `Mod+Enter` opens table in new tab; `↑/↓` or `Ctrl+P/N` move; `Tab` completes keyword; `Esc` clears, `Esc` again closes.
- Anchors: `^prefix`, `suffix$`, `^exact$`.
- Empty query lists features: Diagram, Metrics Board, Backup Database, Restore Database, Open Connection (`Mod+Shift+K`), Open Database (`Mod+K`), Cancel Running Queries (`Mod+.`), Database User Management, Workspace Overview (`Mod+Shift+\`), Customize Toolbar, MCP Server, MCP Activity, Settings (`Mod+,`).

### 7.8 Quick Look **[DOC]**

Popover/window to inspect one cell: `Mod+Enter` on selected cell, middle-click, or context menu. `Esc` closes. Modes: long text; **JSON** editor with **Raw/Tree**, **PRETTY/MINIFY**; **BLOB** preview (image, PDF, hex); spatial values on a **Map** (optional; raw/WKT acceptable), "Open in Window". Binary quick-edit menu: **Save as…, Add a file…, Edit hex…**.

---

## 8. Table views

### 8.1 Tabs per table **[DOC]**

Bottom bar of a table tab: **Data**, **Structure**, **Index** (and **DDL** for views). For document stores (`caps.document_model`): **Data / Json / Tree**.
Shortcuts: Data `Ctrl+Mod+[`, Structure `Ctrl+Mod+]`.

### 8.2 Data tab — paging

- Default page size **300 rows**; page-settings button between arrows → **Limit** and **Offset** + **Go**; next/prev `Mod+→/←`.
- Row count: for large tables show **estimated** count; click → **Count** for exact. Setting: *Estimate count when table has more than n rows* (default on).
- Status line: rows shown, elapsed time.
- **FR-D-001** Fetch pages via `SELECT … FROM t [WHERE filters] [ORDER BY sort] LIMIT n OFFSET m` with identifiers quoted per dialect.

### 8.3 Grid behaviour

- Virtualized (only visible rows laid out). Alternating row colors (setting, default on). Auto-hide scrollbars (setting).
- Column header: click cycles sort **descending → ascending → none** **[DOC]**; context menu: Sort Ascending/Descending/Remove sort, **Sort multiple Columns**, **Filter with column**, **Hide this column**, **Show all columns**. Column resize/reorder (UI-level only).
- Selection: single, multi-row (Shift/Ctrl), **rectangular cell range** by drag; arrow keys move; `Return`/`Tab`/double-click edits; `Esc` deselects; `Mod+↑/↓` first/last row; `Mod+A`.
- NULL rendered distinct (dim italic `NULL`); empty string distinct from NULL; long text truncated with ellipsis; BLOB shown as `(N bytes)`.
- Foreign-key cells show an **arrow**; click opens the referenced table filtered to the referenced row **[DOC]**.
- **FR-D-010** Row state colors: *new*, *modified*, *deleted* backgrounds (theme-defined).

### 8.4 Editing rows **[DOC]** (all pending until commit, §9)

| Action | Gesture |
|---|---|
| Insert row | `+ Row`, `Mod+I`, double-click empty area below last row |
| Duplicate row(s) | context menu Duplicate, `Mod+D`, or copy/paste rows |
| Edit cell | double-click (inline), or edit in right sidebar |
| Set special value | context menu **Set Value** / `Alt+Enter` / `Alt+click`: `EMPTY, NULL, DEFAULT, NOW()` (`GETDATE()`/`GETUTCDATE()` on SQL Server), **Date Picker…**, **Add a file…** (loads file into binary column) |
| Delete row(s) | `Delete` key or context menu |
| Copy range / paste into range | `Mod+Shift+C` / `Mod+Shift+V` |
| Open row in sidebar | `Space` |
| Undo / redo | `Mod+Z` / `Mod+Shift+Z` |

- **FR-D-020 [DESIGN]** Cell editors by type: text (single/multi-line), integer/float validation, bool toggle, date/time pickers, enum dropdown (PG enums/MySQL ENUM), JSON → Quick Look editor, bytes → file/hex.
- **FR-D-021 [DESIGN]** A table with no primary key and no unique key is **read-only for UPDATE/DELETE** unless the user chooses "key columns" in a dialog; INSERT is always allowed. Show a visible badge explaining why.

### 8.5 Copy formats **[DOC]**

`Copy Rows` (`Mod+C`), `Copy Rows As`, `Copy Selected Cells` (`Mod+Shift+C`), `Copy Selected Cells As`, `Copy All Column Values`. "As" formats: **Plain Text, JSON, HTML, Markdown Table, CSV, CSV with Header, INSERT Statement**. CSV defaults come from settings (§16.4). Same menu also: Refresh, Paste rows, Quick Filter, Import, **Export current page…**.

### 8.6 Structure tab **[DOC]**

Grid where each row is a column of the table; attributes (driver dependent): `column_name, data_type, is_nullable, column_default, foreign_key, extra…`. Top fields: **Name** (rename table), **Primary** (primary-key column list with autocomplete). Search field `Mod+F`. Bottom bar: **+ Column**, **DDL**, **Triggers**.

| Operation | Details |
|---|---|
| Insert column | `+ Column`, `Mod+I`, context New Column, double-click empty area. Set name/type/nullability/default. |
| Edit column | double-click attribute inline, or `Space` for the right-sidebar form (bulk edit). Only if `caps.alter_column`. |
| Data type | dropdown from the dialect's type list. |
| Default | menu: `EMPTY`, `NULL`, common defaults (`CURRENT_TIMESTAMP`, `NOW()` …), **Advanced…** (expression; sequence on PG). |
| NOT NULL | set `is_nullable` to `NO`/`YES`. |
| Primary key | edit **Primary** field (add/remove column names). |
| Foreign key | arrow in `foreign_key` → *Create a foreign key on column X* → popover: Referenced Table, Referenced Columns, **On Update / On Delete** (`NO ACTION, RESTRICT, CASCADE, SET NULL, SET DEFAULT`). Existing FKs listed (`→ customers(id)`) → view/change/**Delete**. Disabled when `!caps.add_fk_after_create` (SQLite: view only). |
| Reorder columns | drag handle (MySQL, MariaDB, ClickHouse only). |
| Delete column | `Delete` / context menu. |
| Copy definitions | Copy Rows As / Copy Selected Cells As (formats of §8.5). |
| DDL | `DDL` button shows `CREATE TABLE`; also **Copy Script As > Creation**. |

### 8.7 Index tab **[DOC]**

Rows = indexes: `index_name, is_unique, column_name` (+ algorithm, condition, comment when supported). Create (`+ Index`, `Mod+I`), edit inline (multi-column: comma separated), delete. Hidden when `!caps.index_tab`.

### 8.8 Triggers **[DOC]**

From Structure tab bottom **Triggers** button → popover list + editor: name, timing (`BEFORE/AFTER`), event (`INSERT/UPDATE/DELETE`), statement body in an SQL editor. `+ Trigger` (`Mod+I`), edit, delete. Only when `caps.triggers`.

### 8.9 Table operations **[DOC]**

| Op | Behaviour |
|---|---|
| **Create** | `+` in sidebar → name, columns, optional primary key → `Mod+S` commits `CREATE TABLE` |
| **Rename** | edit Name in Structure tab → commit |
| **Clone…** | name default `<table>_clone`; checkbox **Duplicate table content** (default on) |
| **Truncate…** | dialog with driver options (Cascade, Restart identity); `Alt+Delete` skips dialog |
| **Delete…** | dialog with options (Cascade); `Delete` key skips dialog |
| **Encoding** | menu Connection > View Using Encoding re-decodes text |
| **Item overview** | clicking *Tables* header / context menu shows list of tables with metadata |

All of these queue into the pending-change pipeline and execute on commit.

### 8.10 Copy Script As

`Creation`, `Drop`, `Truncate`, `Select top 100` → clipboard (dialect templates).

### 8.11 Views, functions, procedures **[DOC]**

- **View**: sits in *Tables* section; tabs Data / Structure / **DDL**. Create (`New > View`): Name + `SELECT` in DDL editor. Edit: DDL tab; rename via Name field. **Materialized views** (PG/Redshift/Cassandra) are **read-only**. Delete with confirmation.
- **Function / Procedure**: *Functions* section (hidden unless "Show function section" or when objects exist). Create via `New > Function/Procedure` with a `CREATE FUNCTION/PROCEDURE` template in an SQL editor; click item to open definition; edit; commit with `Mod+S`; delete. Not available on SQLite.

### 8.12 Filters **[DOC]**

Filter bar toggled by `Mod+F` / **Filters** button (shows *Applied* when active).

Each filter row: `[checkbox] [column ▾] [operator ▾] [value] [+] [−]`.
- Column menu: any column, **Any column** (search all), **Raw SQL** (free `WHERE` fragment, e.g. `total > 100 AND status = 'paid'`). `Mod+←/→` switch column/operator.
- Operators (SQLite base): `=, <>, <, >, <=, >=`; `IN, NOT IN` (list `1, 2, 3`); `IS NULL, IS NOT NULL`; `BETWEEN, NOT BETWEEN` (`a AND b`); `LIKE`; `Contains, Not contains, Has prefix, Has suffix`. PostgreSQL adds `ILIKE` and case-insensitive variants of the four text operators.
- `+`/`Mod+I` clones the current filter; `−`/`Mod+Shift+I` removes. Checkbox (`Mod+B`) includes it; **Apply** (Enter) applies the current; **Apply All** (`Mod+Return`) applies all checked, combined with **AND** by default; dropdown **Apply All Checked Filters with OR**.
- Extra buttons: **Clear**, **SQL** (popover with generated SQL for current and all-checked), **Export** (export filtered rows), `Esc` hides (twice if value non-empty).
- Quick filters: header context "Filter with column"; cell context **Quick Filter** (`= value`, `Contains`, `Has prefix`, `IN`, `IS NULL`…); FK arrow.
- **Filter settings** button: *Default Filter Column Sort* (By Name | By Ordinal), *Default Filter Column* (Primary key if exists | Any column | Raw SQL), *Default Filter Operator* (`=` | `Contains`), *Default Filter State* (Remember per table | Always show | Always hide), *Default Table Sort* (none | PK asc | PK desc).
- **Column filter** (`Alt+Mod+F`): choose visible columns (comma-separated text or *Add a column* menu), **Apply**, **Clear**.
- **FR-F-001** Filter → SQL is built by `core::filter::to_where(dialect, &[Filter]) -> String` (pure, unit-tested; always quote identifiers, escape/param values; `Any column` expands to `OR` over text-castable columns).

### 8.13 Search in Database **[DOC]**

`Tools > Search in Database…` or sidebar settings. Inputs: value; match mode `Contains, Not contains, =, Has prefix, Has suffix`; scope **Search all items** / **Select items** (table picker). **Start Search** iterates tables sequentially, checking every column; results list tables with match counts, grouped by schema; **Pause/Resume**; clicking a result opens the table with an *Any column* filter for the term; **Done** closes. Warn that it adds server load.

---

## 9. Pending changes: preview, commit, discard, safe mode

### 9.1 Model **[DESIGN]**

```rust
pub struct PendingChanges { pub tabs: HashMap<TabId, ChangeSet>, pub structural: Vec<StructuralOp> }
pub enum RowChange { Insert{values}, Update{key, set}, Delete{key} }
pub enum StructuralOp { CreateTable, AlterTable, DropTable, CreateIndex, … }
```

- Each data tab has an undoable `ChangeSet` (rows highlighted by state). Structural ops generate DDL through the dialect.
- `generate_sql(&PendingChanges, &dyn Dialect) -> Vec<Statement>` is **pure** and fully unit-tested (golden/snapshot tests per driver).
- UPDATE/DELETE use the primary key (or user-selected key) in `WHERE`; include only changed columns in `SET`.

### 9.2 Buttons **[DOC]**

- **Commit** (`Mod+S`; toolbar right part; `Edit > Commit`): sends statements to the server, in one transaction when `caps.transactions` and statements are DML (best effort for DDL). On success clear pending and reload the tab; on error show the error, keep pending.
  If no pending changes and a SQL editor tab is active, `Mod+S` saves the query instead.
- **Preview** (`Mod+Shift+P`): shows generated SQL; popover has **Commit**, **Discard**, **Open in Window**. SQL-editor queries never appear here.
- **Discard** (`Mod+Shift+Delete`): throw away all pending changes, **reload the tab**, **clear undo history**.
- Toolbar control is disabled when nothing is pending.
- Every committed statement is appended to **History** and the **Console log**.

### 9.3 Safe mode **[DOC]**

Per connection level (default **Silent**; default for new connections configurable in Settings > Security):

| Level | Behaviour |
|---|---|
| Silent Mode | send without warnings |
| Alert Mode 1 | show statements, confirm with **Send** |
| Alert Mode 2 | as Alert 1 except `SELECT/EXPLAIN/SHOW` |
| Safe Mode 1 | require **database password** before sending |
| Safe Mode 2 | as Safe 1 except `SELECT/EXPLAIN/SHOW` |

- Toolbar **Safe Mode** button shows the level; changing level while in Safe Mode 1/2 requires the password.
- **Applies to**: commit, SQL-editor runs, database rename/drop, backup/restore, kill-query in process list, MCP agent queries, metrics-board connection objects.
- **Biometric/OS unlock** (Settings > Security): optionally use OS auth instead of typing the password **[DESIGN: platform-specific, optional]**.
- **FR-S-001** `core::safemode::classify(sql) -> StatementKind {ReadOnly, Write, Ddl}` using `sqlparser`; unknown/multi-statement → treated as Write. Read-only = `SELECT`, `EXPLAIN` (non-ANALYZE), `SHOW`, `DESCRIBE`, `PRAGMA` reads, `WITH … SELECT` (no DML CTE).

---

## 10. SQL editor

### 10.1 Opening **[DOC]**

Toolbar **SQL File**, `Mod+E`, or `Mod+T`. When no SQL editor is focused, `Mod+Return` opens a new one. Editor tab has: text area, bottom bar, results below (collapsible `Ctrl+\``).

### 10.2 Running **[DOC]**

- **Run Current** (`Mod+Return`): runs selection, else the statement under the cursor (highlighted — setting *Highlight current Query*, default on). **Run All** (`Mod+Shift+Return`). Bottom-right run button with dropdown: Run Current / Run All, **Default** action, **Change Shortcut Keys…**.
- Statement splitting by `;` using dialect-aware tokenizer (strings, comments, `$$` bodies, `DELIMITER` for MySQL).
- **Row limit menu** next to run: *No limit*, 100 … 500,000; appends `LIMIT` to `SELECT`s lacking one; saved per connection. For MongoDB the control is a syntax menu (Mongo Shell JS vs Mongo Command).
- **Cancel**: run button becomes `Cancel` (`Mod+.`); for PG/MySQL/MariaDB/SQL Server it opens the **process list** to choose what to kill; for others cancels directly.
- Streaming results (§10.9). Queries go through safe-mode. Every run is saved to history.
- **Return on Error** setting (default off): when running several statements, continue after an error unless on.

### 10.3 Editing features

- Syntax highlighting per dialect (theme colors for comments, numbers, strings, keywords, current-query bg, selection bg, error highlight).
- Line numbers + **fold** arrows per statement and indentation-based sub-blocks; fold state remembered for saved-query files.
- Toggle line comment `Mod+/`, block comment `Alt+Mod+/`.
- Find `Mod+F`, Find & Replace `Alt+Mod+F`, Next `Mod+G`, Prev `Mod+Shift+G`, Use Selection for Find, Jump to Selection `Mod+J`.
- Transformations: upper/lower/capitalize.
- Font size `Mod+=` / `Mod+-`.
- Show invisible characters (off by default), Wrap lines (on by default).
- Key binding: **Sublime (default)** or **Vim** .
- **Auto-save tabs** on close/reopen: *Do not save* | *Only first SQL tab (default)* | *Save all tabs*; saved on run/format/close.

### 10.5 Autocomplete **[DOC]**

Suggest: databases, tables, views, columns, functions, procedures, SQL keywords, and **keywords bound to saved queries**. `↑/↓` or `Ctrl+N/P` navigate, `Return`/`Tab` accept (setting *Complete Key*: Enter or Tab (both, default) | Enter only | Tab only), `Esc` toggles list.
Options (gear menu): Tables, Functions, Keywords, **Disable all suggestions**, *Automatically add a whitespace*, *Automatically prefix schemas* (`public.customers`), *Automatically uppercase keywords*, *Automatically insert closing braces and quotes*.
Mongo shell syntax: collections + shell methods.
**[DESIGN]** Context-aware: after `FROM/JOIN` → tables; after `alias.` → that table's columns; in `SELECT` list → columns of tables in `FROM`. Source of names: cached schema (loaded lazily on connect, refreshed on `Reload`).

### 10.6 Formatting **[DOC]**

- **Beautify** (`Mod+I`) formats selection, or the statement under cursor; keywords uppercase (unless "Automatically UPPERCASE keywords" off).
- **Uglify/minify** (`Mod+Shift+I`).
- Options: **Indent with** Tab|Spaces, **Indent width** 2/4/6/8, **Indent type** Standard | Tabular Left | Tabular Right (tabular uses 10-space alignment, ignores indent width).

### 10.8 Result tabs **[DOC]**

- Multiple statements → one result tab per statement: `Query 1`, `Query 2`…; if **> 100 statements**, single tab.
- Name a tab with a comment directly above the statement: `-- name: Recent orders` (also `label:`, `tab:`).
- **Pin** result tab to survive re-runs; otherwise replaced.
- Setting *Result view mode*: **Split results into Tabs** (default) | **Append all results in one Tab**.

### 10.9 Streaming / async loading **[DOC]**

Rows appear as soon as first arrive; UI never freezes; user may scroll, switch tabs, write more queries during loading. Drivers with paged APIs show **Load more**.

### 10.10 Results view **[DOC]**

Result bar: view switch **Data | Message | Chart**, row count, execution time (incl. latency), search button, **Export…**.
- **Data**: editable like a table when the result maps to one table with key (same pending/commit rules); context menu: Quick Look, Set Value, Duplicate, Delete, copy formats (`Copy Rows` default CSV; `CSV include field names`; `SQL Insert Statement`), Copy All Column Values.
- **Message**: server messages, affected rows, errors, PG `RAISE NOTICE` (also toast notification). Export saves text.
- **Chart**: style Line | Bar | Pie; pick x-axis column and one-or-more y columns (pie: label+value); **Add to Metrics Board**; export as PNG.
- **Chart-from-comment**: `-- tableplus barchart x: month, y: revenue` (implement as `-- chart bar x: …`, and also accept the reference forms `-- tableplus barchart|linechart|piechart`, `-- barchart|linechart|piechart`; pie uses `label:`/`value:`; repeat `y:` for several series).
- **Search/replace in results**: pane with column (or Any), operator (Contains/Not contains/Has prefix/Has suffix), **Match Case**, Replace field, replace one/all; filters already-loaded rows; commit with `Mod+S`.
- **Export**: from result bar or context; when multiple result tabs: *Export Result* / *Export Results from All Tabs*; formats and options as §12.

### 10.11 Query parameters **[DOC]**

Off by default (gear → *Query params options* → *Enable query params*; also Settings). When enabled and the SQL contains placeholders, a **Variables** dialog asks for each value (`Apply` = `Mod+Return`), values are substituted **literally** (user adds quotes for strings), remembered for next time, **Cleanup** removes unused. Formats: Colon `:id`, Percent `%id%`, Question mark `?`, Dollar braces `${id}`, Dollar `$id`, Double braces `{{id}}`. Option *Allow searching for variables in quoted strings* (default: ignore placeholders inside quoted strings).
**[DESIGN]** Warn in the dialog that values are inserted textually (SQL-injection risk is the user's own).

### 10.12 Saved queries ("Queries" tab) **[DOC]**

- Stored as plain `.sql` files; supports **folders**, drag-and-drop move, rename, delete; sort by Name / Date Created / Date Modified.
- Create: `+` or `New > File…` (editor window: **Name**, **Keywords**, query), `Mod+S`; from editor: select → **Add to Queries** (statement under cursor if no selection) or run-menu **Add to Queries** (whole content); from History: **Add to Queries**.
- **Keyword binding**: typing a keyword in the editor offers the saved query in autocomplete; accept inserts it.
- Context menu: Run, Copy, Edit/Rename…, Import… (run via Import SQL wizard), Open file in Tab, Insert into SQL Editor, Show in file manager, Delete.
- Double-click opens in its own tab (or focuses existing). Auto-save edits back to file (setting *Automatically save queries while editing*, default on).

### 10.13 History **[DOC]**

Per-connection. Records: editor queries, statements generated by committed GUI changes, statements run by User Management. Search field. Double-click inserts into new SQL tab. Context menu: Run, Copy, **Save As…** (.sql), Import…, Insert into SQL Editor, Show in file manager, **Add to Queries**, Delete, **Clear all History**.
Setting **Log original values when updating or deleting data**: on commit of UPDATE/DELETE also log the pre-change rows (as CSV) next to the statements (enables manual undo).
**[DESIGN]** A global policy flag `history_enabled` (default true) for managed environments; when false, the tab shows "Disabled".

---

## 11. Database-level operations

### 11.1 Database list **[DOC]**

`Mod+K` / toolbar **Database**: searchable list; **Open** (double-click) opens in new workspace; context: *Open in new Tab*, *Open in current Tab*, *Copy name*, **New…**, **Rename…**, **Drop…**.
- SQLite: lists `.sqlite/.sqlite3/.db` files in the same folder as the connection's file; *New* creates empty `<name>.sqlite3`; *Drop* deletes the file.
- **New database** dialog: name, encoding, collation (if `caps.encoding_collation_on_create_db`).
- **Rename**: only if `caps.rename_db`; cannot rename the currently open DB.
- **Drop**: type the DB name to confirm; cannot drop the currently open DB.
- Safe mode applies.

### 11.2 Metrics board **[DOC]**

A canvas of live-data objects driven by SQL.
- Board list in left sidebar: New, Rename, Duplicate, Delete, **Import/Export Metrics Board** (JSON).
- Toolbar: **Status** (start/stop refresh), **Add**, **Lock**, **Grid** (show canvas grid + connections), **Database** (back to workspace).
- Objects: **Data table**, **Scoreboard** (Score + Detail columns), **Bar chart**, **Line chart** (x + one or more y), **Pie chart** (label + value), **Exporter** (query → CSV/JSON/SQL/Excel), **Importer** (CSV/JSON/SQL → table), **Input field**, **Dropdown** (fixed list or query-loaded), **Button**, **Connection** (switch connection+database for connected objects; that connection's safe mode applies).
- Object settings: Label, SQL, **Refresh rate** = interval 1 s … 30 min | **Refresh on Event** | **Do not auto refresh** (manual refresh button).
- Variables: `$name` in queries (strings inserted as typed → quote them); Input/Dropdown have a *Name* (default `input`) and submit mode (Enter / End Editing / Event; dropdown: Value Change / Event); wire objects by dragging from a connection point; Buttons emit events to connected objects.
- Opened via toolbar **Metrics**, `Tools > Show Metrics Board`, Open Anything, or **Add to Metrics Board** from a result chart.
- **[DESIGN]** Canvas implemented with `egui::Scene`/custom painting; objects are `Area`s; connections drawn as bezier curves.

### 11.3 ER diagram **[DOC]**

- Open: `Tools > Show Diagram`, sidebar item → *Show Diagram*, Open Anything → Diagram. Opens as its own tab; **Workspace** button returns.
- Tables as boxes with columns + types; line per FK between columns; **tables linked by FKs share a color**, unlinked are uncolored. One schema at a time (Schema menu).
- Interaction: drag canvas to scroll, drag table to move, **Zoom In/Out/Reset**, **Sort** by Name|Column Count, **Rearrange** (reload + auto layout), search (`Mod+F`, ↑/↓, Enter jumps).
- Editing (goes to pending changes, `Mod+S` commits; toolbar **Undo/Redo**): click table → settings (Name, Color, Structure editor, Index editor, Delete table); **Add Table** (`untitled_table_N`); **create FK** by dragging from a table's edge crosshair to another table → popover (columns, referenced columns, On Update/On Delete; counts must match else line removed); right-click line → **Delete** FK; right-click table → *Copy column name*, *Delete table*.
- **[DESIGN]** Auto-layout: simple force-directed or layered layout (`petgraph` + custom), persisted positions per schema.

### 11.4 Process list **[DOC]**

Sheet over the workspace, opened by `Tools > Process List`, `Mod+.` or Open Anything "Cancel Running Queries"; uses **its own connection** so it works while the workspace is busy.
- PostgreSQL: active (non-idle) sessions from `pg_stat_activity` (`pid, datname, usename, application_name, client_addr, query_start, state, query`), newest first.
- MySQL/MariaDB: `information_schema.processlist`, per-database; left list of databases with search.
- SQL Server: running requests (session id, status, CPU time, elapsed, text).
- Features: sortable columns, **Refresh**, multi-select (`Mod`/`Shift`), **Kill** (PG `SELECT pg_cancel_backend(pid);` — cancels, session stays; MySQL `KILL QUERY id;`; MSSQL `KILL spid;` — ends session & rolls back), context menu copy (CSV/JSON/HTML/Markdown/plain), Quick Look, export current page; **Dismiss**. Safe mode confirms the kill statement. Unsupported drivers show a message.

### 11.5 User management **[DOC]** (PG, MySQL, MariaDB)

Window: user list (left) + tabs (right); **New user**, context New/Delete; **Apply** executes generated statements (also recorded in History).
- General: Username, Password, and for MySQL/MariaDB **Domain/host** (`localhost`, `%`).
- **PostgreSQL**: *Global Privileges* = role attributes (Can create databases, Superuser, Replication, Bypass RLS); *Table Privileges* = tree of schemas/tables/columns with **Available ↔ Granted** lists.
- **MySQL/MariaDB**: *Global Privileges* grouped (Database & Tables, Views & Procedures, Admin) with **Check all / Uncheck all**; *Database Privileges* per DB with Available ↔ Granted; *Resource* limits: **Max updates / Max connections / Max questions** per hour. Warn that granting all globals ≈ root.
- Update password/privileges and **Apply**; delete user via context menu + **Apply**.
- **[DESIGN]** Implement as a diff: compute `CREATE/ALTER USER`, `GRANT`, `REVOKE`, `DROP USER` from original vs edited model; show in SQL preview before apply.

### 11.6 Encoding **[DOC]**

`Connection > View Using Encoding` re-decodes text columns for tables displayed incorrectly (use `encoding_rs`).

---

## 12. Import / Export / Backup / Restore

### 12.1 Import **[DOC]**

Entry points: `File > Import > From CSV… | From JSON… | From SQL Dump…`, or table context menu **Import** (destination preselected). Requires a selected database. Progress window with **Import in Background** → job appears in Notifications (progress, stop).

**CSV wizard** (preview first **100 rows** below, mapping above):
- Delimiter (`,` `;` Tab, custom), Quote (`"`, `'`, custom), **Encoding** (utf8 default, macOSRoman, windowsCP1252, …), **First line is header**, **Table** (destination), **Create new table** (name from file; *detect column types* or *all text columns*), **Matching strategy** (by name & order | by name | by order | case-insensitive name variants), **Insert option** (PG/MySQL/MariaDB): `STOP ON ERROR | UPDATE ON DUPLICATE | REPLACE` (REPLACE only MySQL/MariaDB).
- Per-column mapping: click header cell → choose destination column, data type (new table) or **Do not import**.
**JSON wizard**: same as CSV, previews first 100 records.
**SQL dump wizard**: previews first **100,000 characters**; choose Encoding; **Import** executes statements on the current DB. (Full `pg_dump`/`mysqldump` restores use §12.3.)
Implementation: run in a transaction where possible; batch inserts (e.g. 500 rows); progress events; cancel supported.

### 12.2 Export **[DOC]**

`File > Export > Export Tables…` (`Mod+Shift+E`) or context menu; select ≥1 items, switch schema, check items across schemas; format tabs **CSV | JSON | SQL | EXCEL**; **Export…** → choose location.
- **CSV options**: Delimiter, Swap (quote char), Line break (`\n`/`\r\n`/`\r`), Decimal separator, *Convert NULL to EMPTY*, *Convert line break to space*, *Put field names in the first row*.
- **JSON**: array of objects.
- **SQL**: per item choose *Structure*, *Drop statement*, *Data*; **Compress using Gzip** (`.gz`).
- **EXCEL**: `.xlsx` workbook.
- CSV/JSON/Excel produce a **folder with one file per table/view**; SQL produces a single `.sql`.
- **File name** field with **Customize…** patterns (connection name, database, table, tag, date/time parts).
- **Export this Table with Column Selection…**: choose columns; for SQL: include structure, content, `DROP TABLE IF EXISTS`.
- **Export current page…** (grid) exports only visible page; **Export** from filters exports filtered rows; result export per §10.10.
- Background jobs with notifications; **Open Folder** when done.

### 12.3 Backup & Restore **[DOC]** (PG, MySQL, MariaDB, PG-compatible: Redshift, CockroachDB, Greenplum, Vertica)

Not for SQLite (copy file or export as SQL). Entry: welcome buttons, `File > Backup…/Restore…`, Open Anything.
- **Backup window**: choose connection (left) and database; choose **dump-tool version** (e.g. `PostgreSQL 17.0`, `MySQL 8.0`); **options** (`Add option…` menu + free text); file name with **Customize** + **Gzip** toggle; **Start backup…**; live tool output; cancel.
  PG default option `--format=custom` (compressed; don't also gzip). Common: `--data-only --schema-only --clean --create --no-owner`.
- **Restore window**: connection, target DB (or **New database**), tool version, options. PG default `--single-transaction`; others `--clean --create --exit-on-error --no-owner --data-only --schema-only`. `pg_restore` reads non-plain archives; plain `.sql` → Import SQL wizard.
- Remembers tool version and options **per connection**. Safe mode confirmation. Notification when finished if the app is in background.
- **[DESIGN]** The reference bundles dump/restore binaries per server version. This implementation **spawns external tools** (`pg_dump`, `pg_restore`, `mysqldump`, `mysql`) found on `PATH` or in a user-configured directory per version; surface a clear "tool not found" error with install hint. Stream stdout/stderr to the window. Pass passwords via env (`PGPASSWORD`, `MYSQL_PWD`) never via argv.

---

## 13. Document-style databases (Tier 3, optional)

For Redis/MongoDB/Elasticsearch/DynamoDB tabs are **Data | Json | Tree** (grid, raw JSON text, collapsible tree of the same documents); edits in any stay pending until commit. Mongo SQL-editor syntax menu: *Mongo Shell (JavaScript)* | *Mongo Command*. Implement only after Tier 1–2 are stable; reuse `Value::Json` and the Quick Look JSON editor.

---

## 14. Settings (Preferences) **[DOC]**

Window with tabs: **General**, **Table**, **Editor**, **Fonts & Themes**, **Keymap**, **Security**, **AI Agent**, **MCP**, (About). Open `Mod+,`.

### 14.1 General / Application

Reopen closed workspaces at startup; Language (follow OS); *Command+T to open* (SQL editor | Empty tab); **CSV** defaults (Delimiter Tab/`,`/`;`; Swap quote `Quote if needed`/`"`/`'`/space; Line break `\n`/`\r\n`/`\r`; Decimal `.`/`,` — warn if `,` used for both delimiter and decimal); **Connections**: *Query time out* (seconds, empty = 300), *Keep connection alive* (default on, 30 s).

### 14.2 Table

Auto-hide table scrollers; *Alternating row background colors* (default on); *Estimate count when table has more than n rows* (+ threshold); *Automatically convert PostGIS geometry to WKT*.

### 14.3 Editor

Auto-save queries while editing (default on); Automatically UPPERCASE keywords; Automatically insert closing braces and quotes; **Complete Key**; **Indent with** + width; **Indent type**; **Key binding** (Sublime/Vim); Query params (enable, allow in quoted strings, format).

### 14.4 Fonts & Themes

- Scope switch **SQL Editor | Data Table | Application** each with fonts (default system monospace), size **9–100 (default 12)**, line height (editor 1.0–5.0), row padding (table 9–100, default 12; left sidebar padding 0–5).
- **Themes**: sections *System* (Dark), *Built-in*, *Custom*, one per **Git theme repository**. Actions: Duplicate, Delete (custom only), Export (JSON) / Export all, Import (file/folder), **Add theme… > File or folder… / Git…** (URL + branch/tag, update/delete repo, info).
- Colors editable per part (hex input + color well + Reset): SQL syntax (comments, numbers, strings, keywords, current query bg, selection bg, error), Data Table (deleted/modified/new row bg, cell selection border/bg, grid), Application (window, sidebar, tabs, notifications). **Restore Defaults**.
- Ship ≥ 2 dark themes in JSON format; themes map to `egui::Visuals` + a custom `AppColors` struct.

### 14.5 Keymap

Remappable: select next/prev pane, tab, workspace; show table data/structure; toggle line/block comment; increase/decrease font; run current/all. UI: click shortcut → "Type shortcut…" → press combo; Delete clears (`Not Set`); Esc cancels; **Reset** per row; **Restore Defaults**. A shortcut must include `Mod` or `Ctrl` unless it's a function key; reject conflicts.

### 14.6 Security

*Default Safe Mode for new connection*; optional OS-auth unlock for Safe Mode; **Passcode** (master password at startup and after being in the background: auto-lock 1 min | 5 min | 1 h | 5 h; change/disable requires current passcode) — implement with `argon2` hash in settings; OS-biometric optional.

### 14.7 Defaults table

| Setting | Default |
|---|---|
| Page size | 300 |
| Keep-alive interval | 30 s |
| Query timeout | 300 s |
| Console log entries | 1000 |
| CSV/JSON import preview | 100 rows |
| SQL import preview | 100,000 chars |
| Max statements before single result tab | 100 |
| Row limit menu | none … 100 … 500,000 |
| Font size | 12 |
| Safe mode (new connections) | Silent |
| MCP port | 51003 (32000–61000) |
| MCP query max duration | 300 s |

---

## 15. AI assistant (LLM) **[DOC]**

### 15.1 UI

Right sidebar **Assistant** tab (setting *Enable Assistant Tab on the Right Sidebar*, default on). Chat box "Ask anything…", Enter = send, Shift+Enter = newline, stop button, provider + model buttons, **@ Add context** (pick tables/views → their `CREATE` DDL is added to the conversation), per-message hover actions (copy, resend, delete), chat menu (New Chat, Clear Chat, **History** with search, Settings). SQL code blocks have **Copy** and **Run** (inserts into the SQL editor and runs through safe mode).

### 15.2 Providers (Settings > AI Agent)

API-key providers: OpenAI, Anthropic, Google AI, OpenRouter, DeepSeek (key in keychain; model list fetched from provider). **Ollama** (`http://localhost:11434`, no real key). **GitHub Copilot** (device-code login). **Codex CLI** (detect executable, status/version/path, sign-in). **Custom OpenAI-compatible** (name, host, sub-path e.g. `/v1`, API key, model). **Custom ACP agent** (command, args, env stored in keychain, *Test Agent*). One provider can be flagged **Default Vendor**.
**[DESIGN]** Phase the work: M10 = OpenAI-compatible + Anthropic + Ollama via `reqwest` streaming (SSE); other providers later.

### 15.3 Database access for the model

The assistant may query the current DB itself via tool calls: **read-only queries run directly** (safe mode still applies); **any other statement requires user approval** in an "Approve SQL" panel (shows SQL and target DB; Approve/Cancel), safe mode still applies after.

### 15.4 Privacy

Requests go directly from the machine to the chosen provider. The provider receives messages, added-table DDL, connection metadata when requested, and **results of executed queries (including data rows)**. Show this notice in settings; recommend local models for sensitive data.

---

## 16. MCP server **[DOC]**

Built-in MCP server so external agents can use the app's connections.

- **Settings > MCP**: *Enable MCP Server* (starts at every launch); status "Running on port N, K clients"; **Preferred Port** (default 51003, range 32000–61000); **Allow remote access (TLS)** (HTTPS; **Export Certificate…** self-signed `.pem`); **Approval needed for unsafe query** (default on); **Auto-generate Access Token for the stdio bridge** (default on).
- **Transports**: Streamable HTTP at `http(s)://host:port/mcp` with `Authorization: Bearer <token>`; plus a tiny **stdio bridge binary** (`<app>-mcp`) that forwards to the running server (starts the app in background if needed; env `…_MCP_ACCESS_TOKEN`).
- **Tokens**: name, permissions **Read Only** (list/open connections, read-only queries) | **Read & Write** | **Full Access**; *Connection Access* = **Allow All** or **Limited** (selected connections/groups); shown **once** at creation; list shows name, prefix, permissions, status, created, last used; **Revoke** (stays listed as Revoked), **Delete**. Stored in keychain.
- **Tools**: `list_connections`, `list_open_connections`, `open_connection` (tab or new window), `disconnect` (connection or one DB), `execute_read_query` (read-only; marked read-only hint), `execute_query` (needs Read&Write/Full; marked destructive hint). Max duration **300 s**; queries run in the connection's workspace and appear in its console log.
- **Resources**: saved queries, per-connection query history (unless disabled), metrics boards.
- **Query approval**: on Silent-mode connections with approval enabled, show **Approve AI Agent Query** sheet (Allow/Deny) for writes/DDL; on other safe modes the safe-mode prompt applies. Bring window to front.
- **Activity window** (Settings > MCP > Activity or Open Anything "MCP Activity"): **Activity** view (status, time, category, token, client address, connection, action, query; search; time range Last 24 h/7 d/30 d/All; token filter; **Export CSV**; **Clear** older than 30/7 days or all) and **Client** view (connected clients; **Disconnect**).
- **Client setup helpers**: one-click "Add to …" for popular coding agents by writing their MCP config (optional/stretch); **Manual Setup…** shows stdio command or HTTP endpoint.
- **[DESIGN]** Implement with `rmcp`; route every tool call through the same `SafeModeGate` and `SessionActor` as the GUI so rules cannot diverge. Query classification via §9.3 `classify`.

---

## 18. Menus, shortcuts, commands

### 18.1 Menu bar **[DOC]**

| Menu | Items (shortcut) |
|---|---|
| **App** | About, Settings (`Mod+,`), Hide, Quit (`Mod+Q`) |
| **File** | Open… (`Mod+O`), Open Recent, Save As… (`Mod+Shift+S`), New Workspace (`Mod+N`), New SQL Viewer (`Mod+Shift+O`), Backup…, Restore…, Export > Export Tables… (`Mod+Shift+E`) / Export this Table with Column Selection…, Import > From CSV/JSON/SQL Dump…, Close |
| **Edit** | Undo/Redo, **Commit** (`Mod+S`), **Discard** (`Mod+Shift+Delete`), **Preview** (`Mod+Shift+P`), Add Row (`Mod+I`), Duplicate Rows (`Mod+D`), Cut/Copy/Copy Selected Cells/Paste/Paste to Selected Cells, Delete, Select All, Toggle Line/Block Comment, Font size ±, Find submenu (Find, Find and Replace, Next, Previous, Use Selection, Jump to Selection, Find Items in left sidebar `Mod+Shift+F`), Transformations |
| **View** | Toggle Left Sidebar (`Mod+0`), Right Sidebar (`Space`), Bottom/console, Query Results Pane (`Ctrl+\``), Full Screen |
| **Tools** | Process List (`Mod+.`), Show Diagram, Show Metrics Board, User Management, Search in Database… |
| **Connection** | New…, Edit…, Open a Database (`Mod+K`), Open a Connection (`Mod+Shift+K`), Run Current/All Query, View Using Encoding, Reload Workspace (`Mod+R`), Reload Current Tab (`Alt+Mod+R`), Reconnect (`Mod+Shift+R`), Disconnect (`Alt+Mod+.`) |
| **Theme** | built-in/installed themes, More themes, Import theme from Git…, Import from file/folder…, Theme Settings |
| **Navigate** | Open Anything (`Mod+P`), New/Close Tab, Show Table Data/Structure, Go Back/Forward (`Ctrl+Mod+←/→`), Next/Prev Tab, Tab at Index 1–9, Next/Prev Workspace |
| **Help** | Changelog link, Enable SSH log, Report bug (`Mod+?`) |

### 18.2 Global shortcut reference **[DOC]**

(Implement as an `Action` enum + `KeyMap`; every action is searchable in Open Anything.)

| Context | Shortcut | Action |
|---|---|---|
| App | `Mod+,` `Mod+N` `Mod+O` `Mod+Shift+O` `Mod+Q` | settings, connection list/new workspace, open file, new SQL viewer, quit |
| Workspace | `Mod+P` `Mod+K` `Mod+Shift+K` `Mod+E` `Mod+T` `Mod+W` | open anything, open DB, open connection, new SQL editor, new tab, close tab |
| Workspace | `Mod+S` `Mod+Shift+P` `Mod+Shift+Delete` `Mod+Z` `Mod+Shift+Z` | commit, preview, discard, undo, redo |
| Workspace | `Mod+R` `Alt+Mod+R` `Mod+Shift+R` `Alt+Mod+.` `Mod+.` `Mod+Shift+E` | reload workspace, reload tab, reconnect, disconnect, process list, export |
| Navigation | `Mod+]` `Mod+[` `Mod+1…9` `Mod+Shift+]/[` `Mod+Shift+\` `Ctrl+Mod+←/→` `Ctrl+Mod+[ ]` | tabs, workspaces, overview, history back/forward, data/structure |
| Panels | `Mod+0` `Space` `Ctrl+Mod+C` `Ctrl+\`` `Mod+Shift+F` `Ctrl+Mod+F` | left, right, console, results, sidebar search, fullscreen |
| Sidebar | `Space` `Delete` `Alt+Delete` | open, delete, truncate |
| Table data | `Mod+I` `Mod+D` `Mod+C` `Mod+Shift+C` `Mod+V` `Mod+Shift+V` `Delete` `Return/Tab` `Space` `Mod+Return` `Alt+Return` `Mod+↑/↓` `Mod+←/→` `Mod+F` `Alt+Mod+F` `Esc` | add, duplicate, copy rows, copy cells, paste, paste cells, delete, edit cell, row detail, Quick Look, quick edit menu, first/last row, prev/next page, filters, column filter, deselect |
| SQL editor | `Mod+Return` `Mod+Shift+Return` `Mod+.` `Mod+I` `Mod+Shift+I` `Mod+/` `Alt+Mod+/` `Mod+=` `Mod+-` `Mod+F` `Alt+Mod+F` `Mod+G` `Mod+Shift+G` `Mod+Shift+S` `Esc` `Ctrl+N/P` | run current, run all, cancel, beautify, uglify, comments, font, find/replace, next/prev, save SQL, autocomplete toggle, suggestion nav |

**[DESIGN]** On Windows/Linux, avoid collisions with OS/desktop shortcuts: provide per-platform default maps and let the Keymap UI override.

---

## 19. UX and visual requirements **[DESIGN]**

- Dense, professional, keyboard-first. Dark themes only.
- Tooltips on every toolbar button including shortcut text **[DOC]**.
- Destructive actions (drop DB, delete group, truncate, delete table) always confirm; DB drop requires typing the name **[DOC]**.
- Production-tagged or Safe-Mode connections show a persistent colored status bar.
- Errors: toast + console log + inline message in result "Message" view; never panic on DB errors.
- Accessibility: all actions reachable by keyboard; minimum 4.5:1 contrast in built-in themes; font size scalable 9–100.
- Localization: English only in v1; string table centralized for later i18n.

---

## 20. Non-functional requirements

| ID | Requirement |
|---|---|
| NFR-01 | UI stays ≥ 60 FPS while a 1,000,000-row result streams (repaint throttled, batches appended off-thread). |
| NFR-02 | Opening a table with 1M+ rows shows first page in < 1 s on LAN (estimate count + `LIMIT`). |
| NFR-03 | Memory: result sets capped by row-limit setting; grid stores rows in a compact column-oriented or `Arc<[Value]>` layout; BLOBs > 1 MB are lazy. |
| NFR-04 | No UI-thread blocking > 16 ms in `update()`; heavy work (highlight, format, layout) cached/offloaded. |
| NFR-05 | Credentials never logged, never in URLs shown in UI (mask), never written to JSON. Console log redacts `password`. |
| NFR-06 | All network/DB I/O is cancellable; app exits cleanly closing connections and SSH tunnels. |
| NFR-07 | Crash-safe config writes (atomic rename); corrupt config → backup and defaults, with a visible warning. |
| NFR-08 | Binary size reasonable: drivers behind Cargo features. |
| NFR-09 | Works offline (no telemetry, no auto-update in v1). |
| NFR-10 | MCP/HTTP listeners bind `127.0.0.1` unless remote access explicitly enabled; tokens compared in constant time. |

---

## 21. Testing strategy

1. **Unit (pure core)**: filter→SQL, change→SQL (insert/update/delete, per dialect), DDL generation (create/alter table, FK, index), CSV/JSON import mapping, URL parse/format round-trip, safe-mode classifier, statement splitter, query-param substitution, tab-naming comments, chart-comment parser, theme (de)serialization, keymap conflicts.
2. **Snapshot tests** (`insta`) for generated SQL and formatted queries.
3. **Integration** with `testcontainers` (Postgres, MySQL) and real SQLite files: connect, introspect, page, edit+commit, cancel, import/export, backup/restore (if tools present), process list, user management.
4. **SSH tunnel** test against an `openssh-server` container.
5. **UI smoke tests** with `egui_kittest` (or headless `egui::Context` runs): welcome list, open table, edit cell → pending → preview → commit, filter apply, sort cycle, tab open/close semantics.
6. **MCP** conformance: token permission matrix, connection scoping, approval flow.
7. **Property tests** (`proptest`) for identifier quoting and literal escaping (no injection through names/values).
8. CI matrix: Linux, Windows, macOS; `clippy -D warnings`; `cargo deny`.

---

## 22. Milestones and acceptance criteria

| M | Scope | Done when |
|---|---|---|
| **M0** | Workspace skeleton, eframe app, theme plumbing, settings/persistence, keymap/action system, Open-Anything shell | App launches with welcome window, dark theme, settings saved |
| **M1** | SQLite driver; connection form (file); connection list/groups/tags/colors; workspace layout (sidebars, tabs, status bar, console); table Data tab with paging, sorting, selection; Quick Look (text/JSON) | Open `.sqlite`, browse tables, page 300 rows |
| **M2** | Pending changes pipeline: edit/insert/duplicate/delete rows, undo/redo, Preview/Commit/Discard, row state colors; right sidebar Details | Edit→Preview SQL→Commit works; discard reloads |
| **M3** | SQL editor v1: highlight, run current/all, row limit, streaming results, result tabs, messages, history, saved queries, autocomplete (keywords/tables/columns), format, comments, find | Run multi-statement script, named tabs, history persisted |
| **M4** | Structure/Index tabs (columns, constraints, PK/FK, defaults, indexes), create/rename/clone/truncate/drop table, views, copy-script-as, filters (all operators, multi-filter AND/OR, quick filters, column filter), Search in Database | Create table from GUI; filtered grid; DDL snapshots pass |
| **M5** | PostgreSQL + MySQL/MariaDB drivers, TLS, URL import/export/Copy-as-URL, safe-mode levels, keychain, password modes, bootstrap commands, keep-alive/timeout, database list (new/rename/drop), cancel via side connection | Parity of M1–M4 on PG/MySQL; safe-mode prompts work |
| **M6** | SSH tunnel; query parameters; workspaces/database stack; session restore; connection import/export files | Connect through SSH container |
| **M7** | Import wizards (CSV/JSON/SQL) and Export (CSV/JSON/SQL/XLSX, gzip, filename patterns, column selection, background jobs + notifications); backup/restore via external tools | Round-trip export→import equal data |
| **M8** | Tier-2 drivers (DuckDB, MSSQL, CockroachDB/Redshift/Greenplum via PG, ClickHouse); process list; user management; encoding menu; foreign-key navigation | Feature matrix of §5.2 enforced via capabilities |
| **M9** | ER diagram, Metrics Board, charts from results/comments; Redis/Mongo (optional) | Edit FK in diagram → commit |
| **M10** | AI assistant (providers, context, approve-SQL); MCP server (tokens, tools, approvals, activity log); stdio bridge | External MCP client lists & queries a connection under token rules |
| **M11** | Polish: themes import/Git, keymap editor, passcode/security, performance pass, packaging (MSI/DMG/AppImage/deb), docs | NFRs verified |

**Definition of done for every milestone:** compiles on 3 OSes, unit + integration tests green, no `unwrap()` on I/O paths, `clippy` clean, CHANGELOG entry, short demo GIF/notes.

---

## 23. Appendix A — SQL generation reference **[DESIGN]**

| Feature | PostgreSQL | MySQL/MariaDB | SQLite |
|---|---|---|---|
| Quote identifier | `"name"` | `` `name` `` | `"name"` |
| Paging | `LIMIT n OFFSET m` | `LIMIT m, n` or `LIMIT n OFFSET m` | `LIMIT n OFFSET m` |
| Row count estimate | `SELECT reltuples::bigint FROM pg_class WHERE oid = '"s"."t"'::regclass` | `SELECT TABLE_ROWS FROM information_schema.TABLES …` | exact `COUNT(*)` |
| Truncate | `TRUNCATE TABLE t [RESTART IDENTITY] [CASCADE]` | `TRUNCATE TABLE t` | `DELETE FROM t` |
| Drop table | `DROP TABLE t [CASCADE]` | `DROP TABLE t` | `DROP TABLE t` |
| Rename table | `ALTER TABLE a RENAME TO b` | `RENAME TABLE a TO b` | `ALTER TABLE a RENAME TO b` |
| Alter column type | `ALTER TABLE t ALTER COLUMN c TYPE x` | `ALTER TABLE t MODIFY c x` | **table rebuild** (create new, copy, drop, rename) |
| Add FK | `ALTER TABLE … ADD CONSTRAINT … FOREIGN KEY …` | same | **unsupported** after create |
| Upsert on import | `INSERT … ON CONFLICT (pk) DO UPDATE` | `INSERT … ON DUPLICATE KEY UPDATE` / `REPLACE` | `INSERT OR REPLACE` |
| Cancel | `pg_cancel_backend` | `KILL QUERY` | interrupt handle |
| Process list | `pg_stat_activity` | `information_schema.processlist` | n/a |
| Describe columns | `information_schema.columns` + `pg_catalog` | `information_schema.columns` | `PRAGMA table_xinfo` |
| Indexes | `pg_indexes`/`pg_index` | `information_schema.statistics` | `PRAGMA index_list/index_info` |
| Triggers | `information_schema.triggers`/`pg_trigger` | `information_schema.triggers` | `sqlite_master` (read-only UI) |

---

## 24. Appendix B — Pending-change lifecycle (state machine)

```
Clean ──edit──► Dirty(ChangeSet non-empty) ──Preview──► Dirty (popover)
  ▲                │   │
  │                │   └─Discard──► reload tab, clear undo ─► Clean
  └──Commit ok─────┘
        │ Commit error ─► Dirty (error shown; nothing lost)
Safe-mode gate sits between "Commit/Run" and the driver:
  level Silent → send; Alert → confirm(SQL list); Safe → confirm + password/OS-auth.
```

---

## 25. Appendix C — Event/Command catalogue (UI ↔ Session actor) **[DESIGN]**

```rust
enum Command {
  Connect(ConnectConfig), Disconnect, Reconnect, Reload,
  UseDatabase(String),
  LoadSchema{db, schema}, LoadStructure(TableRef),
  FetchPage{req:RequestId, table:TableRef, filters, sort, limit, offset},
  CountExact(TableRef),
  RunSql{req:RequestId, statements:Vec<String>, opts:QueryOpts},
  Commit{req, statements:Vec<String>}, Cancel(RequestId),
  ProcessList, Kill(Vec<String>),
  DbOp(DbOp /*create/rename/drop*/), UserOp(Vec<String>),
  Import(ImportJob), Export(ExportJob), Backup(BackupJob), Restore(RestoreJob),
  SearchInDb(SearchJob), Pause(JobId), Resume(JobId),
}
enum Event {
  Connected{server_version}, Disconnected{reason}, SchemaLoaded(..), StructureLoaded(..),
  RowBatch{req, cols, rows}, QueryDone{req, summary}, QueryError{req, err},
  CommitDone{req}, CommitError{req, err}, Progress{job, pct, text}, JobDone{job, outcome},
  ConsoleLine(ts, sql), Notice(msg), ProcessListLoaded(ResultSet),
}
```

---

## 26. Appendix D — Reference facts checklist (quick verification)

- Free-tier limits, licensing, MDM, beta updates: **excluded**.
- Page size 300; keep-alive 30 s; timeout 300 s; console 1000 entries.
- Import previews: 100 rows (CSV/JSON), 100,000 characters (SQL).
- Safe mode: 5 levels (Silent, Alert 1/2, Safe 1/2).
- 5 default tags: local, testing, development, staging, production.
- Filter operators set; AND default, OR via menu.
- Export formats: CSV, JSON, SQL, Excel (+gzip for SQL); backup default PG `--format=custom`, restore default `--single-transaction`.
- MCP default port 51003; tools: `list_connections, list_open_connections, open_connection, disconnect, execute_read_query, execute_query`; 3 permission levels; token shown once.
- Result-tab naming comment: `-- name:` / `label:` / `tab:`; > 100 statements → single tab.
- Query param formats: `:id`, `%id%`, `?`, `${id}`, `$id`, `{{id}}`.
- Beautify indent width 2/4/6/8; indent types Standard/Tabular Left/Tabular Right.
- Row-limit menu: none, 100 … 500,000.
- Metrics objects: Data table, Scoreboard, Bar, Line, Pie, Exporter, Importer, Input, Dropdown, Button, Connection; refresh 1 s–30 min / on event / none.

---

*End of specification.*
