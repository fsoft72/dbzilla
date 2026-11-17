# Trigger editor and ER diagram FK delete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an add/edit/delete trigger editor to the Structure tab and a "Delete foreign key" entry for real FK lines in the ER diagram.

**Architecture:** DDL generation for triggers is pure code in `dbzilla-core` (two new `StructuralOp` variants). The PostgreSQL driver returns structured trigger fields and lists trigger functions through a new `Conn` method, carried by a new session command and event. The UI part sends every change through the existing `submit_ddl` (safe-mode gate, History). The diagram edge carries its constraint name.

**Tech Stack:** Rust workspace (egui, tokio), tests with `cargo test`. Integration tests for PostgreSQL and MySQL need the servers the existing `crates/dbzilla-drivers/tests/support/mod.rs` already expects.

**Spec:** `docs/superpowers/specs/2026-11-17-trigger-editor-fk-delete-design.md` (behaviour: `SPEC.md` 8.8 and 11.3)

## Global Constraints

- Never use the em dash character; use `-`. Code and comments in English.
- Every function gets a doc comment; constants UPPERCASE; early returns for exit conditions.
- Text files end with an empty line. Package manager rules do not apply (Rust only).
- Never commit with `--no-verify`; fix hook failures. Do not push. No AI attribution lines in commit messages.
- Work on a regular branch (no worktrees): `feature/trigger-editor` for Tasks 1-4, then `feature/diagram-fk-delete` from `master` for Tasks 5-6.
- SQLite triggers stay read-only (SPEC 8.8, appendix table). Triggers need `caps.triggers`.
- Update `CHANGES.md` and tick the matching `FEATURES.md` line in the last task of each branch.

## Review Focus

- MySQL body with `;` inside `BEGIN ... END`: must reach the driver as one statement, unsplit (Task 1 test).
- Names with quotes or spaces, and a PostgreSQL table and function in different non-`public` schemas: correct quoting and qualification (Task 1 tests).
- A PostgreSQL trigger the editor cannot represent (several events, `UPDATE OF`, `WHEN`, constraint trigger, arguments, `INSTEAD OF`): shown read-only with a reason, never silently rewritten (Tasks 2 and 4).
- Edit is drop then create; on MySQL (non-transactional DDL) a failed create leaves the trigger dropped: the UI must reload the structure after any failure so the list is truthful (Task 4).
- Foreign key edge without a constraint name, or on SQLite: the menu entry is disabled with the reason, not hidden and not a crash (Task 6).

---

## Branch 1: `feature/trigger-editor`

Start with `git checkout -b feature/trigger-editor` (from `master`) and commit the two docs files first: `git add docs/superpowers && git commit -m "docs: design and plan for trigger editor and FK delete"`.

### Task 1: Trigger DDL in core

**Files:**
- Modify: `crates/dbzilla-core/src/ddl.rs` (new types, two `StructuralOp` variants, generation, `DdlError::Invalid`)
- Test: `crates/dbzilla-core/tests/ddl.rs`

**Interfaces:**
- Produces:
  - `pub struct TriggerDef { pub name: String, pub timing: String, pub event: String, pub level: TriggerLevel, pub function: Option<TableRef>, pub body: String }`
  - `pub enum TriggerLevel { Row, Statement }` (derive `Debug, Clone, Copy, PartialEq, Eq, Default`, default `Row`)
  - `StructuralOp::CreateTrigger { table: TableRef, trigger: TriggerDef }`, `StructuralOp::DropTrigger { table: TableRef, name: String }`
  - `DdlError::Invalid(String)` (message prefix `invalid: `)

- [ ] **Step 1: Write failing tests** in `tests/ddl.rs`, using the file's `ddl(DriverId, StructuralOp)` helper. Names and assertions:
  - `create_trigger_postgres_calls_the_function`: table `shop.orders`, trigger `trg_orders` BEFORE INSERT, `Row`, function `shop.trg` gives exactly `CREATE TRIGGER "trg_orders" BEFORE INSERT ON "shop"."orders" FOR EACH ROW EXECUTE FUNCTION "shop"."trg"()`.
  - `create_trigger_postgres_function_in_other_schema_and_statement_level`: table in `a`, function in `b`, `Statement` gives `... FOR EACH STATEMENT EXECUTE FUNCTION "b"."f"()`.
  - `create_trigger_mysql_keeps_a_multi_statement_body_whole`: body `BEGIN SET NEW.a = 1; SET NEW.b = 2; END` gives a one element vec equal to ``CREATE TRIGGER `t1` AFTER UPDATE ON `orders` FOR EACH ROW BEGIN SET NEW.a = 1; SET NEW.b = 2; END``.
  - `trigger_names_are_quoted`: name `we"ird name` is quoted by the dialect on both PostgreSQL and MySQL (MySQL backtick doubling).
  - `drop_trigger_per_dialect`: PostgreSQL `DROP TRIGGER "trg_orders" ON "shop"."orders"`; MySQL ``DROP TRIGGER `trg_orders` `` qualified with the table's database when `TableRef.database` is set (use `d.quote_table` on a `TableRef` built from the database and trigger name).
  - `trigger_ops_are_refused_on_sqlite`: both ops return `Err(DdlError::Unsupported(_))`.
  - `create_trigger_validates`: empty name, MySQL empty body, PostgreSQL missing function, timing not in `BEFORE|AFTER`, event not in `INSERT|UPDATE|DELETE` each return `Err(DdlError::Invalid(_))`.
- [ ] **Step 2: Run** `cargo test -p dbzilla-core --test ddl trigger` - Expected: FAIL (compile error, types missing).
- [ ] **Step 3: Implement** the types and the two match arms in `generate_ddl` (the file's existing function). Validation first as early returns. MySQL has no `FOR EACH STATEMENT`, so `level` is ignored there and always `FOR EACH ROW`. PostgreSQL function call always ends in `()` (no arguments, see the editable rule in Task 2).
- [ ] **Step 4: Run** `cargo test -p dbzilla-core` - Expected: all PASS.
- [ ] **Step 5: Commit** `git add crates/dbzilla-core && git commit -m "feat: trigger create and drop DDL"`

### Task 2: Driver trigger metadata and function list

**Files:**
- Modify: `crates/dbzilla-core/src/model.rs` (`TriggerInfo`), `crates/dbzilla-drivers/src/traits.rs`, `postgres.rs`, `mysql.rs`, `sqlite.rs`
- Test: `crates/dbzilla-drivers/tests/postgres.rs`, `mysql.rs`, `sqlite.rs`

**Interfaces:**
- Produces:
  - `TriggerInfo` gains `#[serde(default)] pub level: String` (`"ROW"` or `"STATEMENT"`, empty when unknown), `#[serde(default)] pub function: Option<TableRef>` (PostgreSQL only), `#[serde(default)] pub editable: bool`; derive `Default`.
  - `Conn::trigger_functions(&mut self, schema: Option<&str>) -> DriverResult<Vec<TableRef>>` with a default body returning `Ok(vec![])`.
- Consumes: `TriggerLevel` is not used here; level stays a string in the model.

- [ ] **Step 1: Write failing tests.**
  - In `tests/postgres.rs` extend the structure test fixture (already creates `shop.trg()` and `trg_orders`): assert `st.triggers[0]` has `level == "ROW"`, `function == Some(TableRef{schema: Some("shop"), name: "trg", ..})`, `editable == true`. Add a second trigger `trg_multi AFTER INSERT OR UPDATE ON shop.orders FOR EACH ROW EXECUTE FUNCTION shop.trg()` and assert `editable == false`. Add `trigger_functions_lists_only_trigger_returning_functions`: with `shop.f()` (returns int) and `shop.trg()`, `c.trigger_functions(Some("shop"))` equals only `shop.trg`.
  - In `tests/mysql.rs` assert the existing trigger has `editable == true` and `level == "ROW"`.
  - In `tests/sqlite.rs` assert `editable == false`.
- [ ] **Step 2: Run** `cargo test -p dbzilla-drivers --test sqlite` and the PostgreSQL and MySQL tests - Expected: FAIL (compile).
- [ ] **Step 3: Implement.**
  - PostgreSQL: extend the existing trigger query in `postgres.rs` to also select `(t.tgtype & 1) <> 0`, the function's schema and name (join `pg_proc p ON p.oid = t.tgfoid` and `pg_namespace`), and the editable condition: exactly one of the bits `4|8|16` set in `tgtype` (no TRUNCATE bit 32), `tgtype & 64 = 0` (not INSTEAD OF), `t.tgqual IS NULL`, `t.tgattr::text = ''`, `t.tgnargs = 0`, `t.tgconstraint = 0`. `trigger_functions` queries `pg_proc` joined to `pg_namespace` where `prorettype = 'trigger'::regtype`, restricted to `schema` (or every non-system schema when `None`), ordered by schema then name.
  - MySQL: level `"ROW"`, `editable = true`. SQLite: level empty, `editable = false`. Replace the three struct literals with `..Default::default()` where needed.
- [ ] **Step 4: Run** the three driver test files and `cargo test -p dbzilla-core` - Expected: PASS.
- [ ] **Step 5: Commit** `git add crates && git commit -m "feat: structured trigger metadata and trigger function list"`

### Task 3: Session command for trigger functions

**Files:**
- Modify: `crates/dbzilla-session/src/commands.rs`, `crates/dbzilla-session/src/actor.rs`
- Test: `crates/dbzilla-session/tests/session.rs`

**Interfaces:**
- Consumes: `Conn::trigger_functions`.
- Produces: `Command::LoadTriggerFunctions { schema: Option<String> }`, `Event::TriggerFunctions { schema: Option<String>, functions: Vec<TableRef> }`.

- [ ] **Step 1: Write failing test** `load_trigger_functions_on_sqlite_returns_an_empty_list` following the existing structure/diagram tests in `tests/session.rs`: send the command, expect `Event::TriggerFunctions` with an empty vec.
- [ ] **Step 2: Run** `cargo test -p dbzilla-session load_trigger_functions` - Expected: FAIL (compile).
- [ ] **Step 3: Implement** the variants and the actor arm next to `Command::LoadStructure`; on error emit `Event::Error { error: format!("cannot list the trigger functions: {e}") }`.
- [ ] **Step 4: Run** `cargo test -p dbzilla-session` - Expected: PASS.
- [ ] **Step 5: Commit** `git add crates/dbzilla-session && git commit -m "feat: session command to list trigger functions"`

### Task 4: Trigger editor UI

**Files:**
- Create: `crates/dbzilla-app/src/ui/triggers.rs` (draft model, pure op builder, editor widget); register in `ui/mod.rs`
- Modify: `crates/dbzilla-app/src/ui/workspace.rs` (replace the `triggers_for` modal around line 3019, handle `Event::TriggerFunctions`, `Action::AddRow` near line 1036), `crates/dbzilla-app/src/ui/structure.rs:187` (button), `CHANGES.md`, `FEATURES.md`
- Test: unit tests in `triggers.rs`; one workspace test in `workspace.rs` tests module

**Interfaces:**
- Consumes: `StructuralOp::{CreateTrigger, DropTrigger}`, `TriggerInfo`, `Command::LoadTriggerFunctions`, `submit_ddl(ops: Vec<StructuralOp>, after: DdlAfter)`.
- Produces: `pub struct TriggerDraft { pub original: Option<TriggerInfo>, pub name: String, pub timing: String, pub event: String, pub level: TriggerLevel, pub function: Option<TableRef>, pub body: String }`; `TriggerDraft::blank(driver: DriverId) -> TriggerDraft`; `TriggerDraft::from_info(&TriggerInfo, DriverId) -> TriggerDraft`; `TriggerDraft::to_ops(&self, table: &TableRef) -> Result<Vec<StructuralOp>, String>` (new: `[CreateTrigger]`; edit: `[DropTrigger(original.name), CreateTrigger]`); `pub fn delete_ops(table: &TableRef, name: &str) -> Vec<StructuralOp>`.

- [ ] **Step 1: Write failing unit tests** in `triggers.rs`:
  - `new_draft_makes_one_create_op`; `edited_draft_drops_the_original_name_then_creates` (rename from `a` to `b` gives `[DropTrigger a, CreateTrigger b]`); `delete_makes_one_drop_op`; `to_ops_reports_the_core_validation_message` (empty name returns `Err` containing `invalid`); `from_info_of_a_not_editable_trigger_is_flagged` (`editable == false` info gives a draft whose `to_ops` returns `Err("This trigger uses options the editor cannot keep (several events, UPDATE OF, WHEN or arguments). Edit it in a SQL tab.")`).
- [ ] **Step 2: Run** `cargo test -p dbzilla-app triggers` - Expected: FAIL (module missing).
- [ ] **Step 3: Implement `triggers.rs`** types and builder. For `to_ops` call `generate_ddl` on the result with the driver's dialect is not needed here; core validates when `submit_ddl` runs, but call it once inside `to_ops` so the error surfaces in the editor instead of a toast.
- [ ] **Step 4: Run** `cargo test -p dbzilla-app triggers` - Expected: PASS.
- [ ] **Step 5: Implement the widget and wiring.**
  - Replace the modal with a larger one: left list of triggers plus `+ Trigger`, right editor. MySQL editor: name, timing and event combos, a multiline monospace body. PostgreSQL editor: name, timing, event, ROW/STATEMENT, function combo filled from `Event::TriggerFunctions` (request it when the modal opens, with the table's schema), no body. A not-editable trigger shows its `statement` read-only with the reason. SQLite: list and statements read-only, no buttons. Buttons: Save, Delete (with a confirm line), Close.
  - Save and Delete call `submit_ddl` with `DdlAfter { closed: None, message }`; after any result, success or failure, reload the structure (`Command::LoadStructure`) so the list is truthful.
  - Show the `Triggers (n)` button whenever `caps.triggers` (also with zero triggers, so the first one can be added) and keep `StructAction::ShowTriggers`.
  - In the `Action::AddRow` handler, when the trigger modal is open start a blank draft instead of adding a row (`Mod+I`).
- [ ] **Step 6: Write and run workspace test** `triggers_modal_is_read_only_on_sqlite` in `workspace.rs` tests (use the `connected`/`wait` helpers seen at `diagram_only_links_are_kept_and_deleted`): create a table with a trigger, open its structure, open the modal, assert the draft/editor state reports read-only and `submit_ddl` is never reachable (no pending gate, `ws.gate.is_none()`). Run `cargo test -p dbzilla-app` - Expected: PASS.
- [ ] **Step 7: Manual check** with `cargo run` against a local MySQL and PostgreSQL: create, edit, delete a trigger on each; confirm safe mode asks for confirmation at levels above Silent.
- [ ] **Step 8: Docs and commit.** Tick the "Trigger editor" line in `FEATURES.md`, add a `CHANGES.md` entry, then `cargo clippy --workspace -- -D warnings` and `git add -A crates CHANGES.md FEATURES.md && git commit -m "feat: trigger editor in the structure tab"`. Then `git checkout master && git merge --no-ff feature/trigger-editor`.

---

## Branch 2: `feature/diagram-fk-delete`

Start with `git checkout -b feature/diagram-fk-delete` from the merged `master`.

### Task 5: Constraint name on diagram edges

**Files:**
- Modify: `crates/dbzilla-core/src/diagram.rs` (`DiagramEdge`, `Diagram::build`, `add_manual`, `infer_edges`, existing tests that build edge literals around lines 407, 408, 561, 637)

**Interfaces:**
- Produces: `DiagramEdge.constraint: Option<String>` (`Some(name)` only for `EdgeKind::Real` whose `ForeignKeyRef.name` is set).

- [ ] **Step 1: Write failing test** `real_edge_carries_the_constraint_name` in the `diagram.rs` tests module: a column with `ForeignKeyRef { name: Some("fk_c"), .. }` builds a `Real` edge with `constraint == Some("fk_c".into())`; a `ForeignKeyRef` with `name: None` gives `None`; manual and inferred edges give `None`.
- [ ] **Step 2: Run** `cargo test -p dbzilla-core diagram` - Expected: FAIL (compile).
- [ ] **Step 3: Implement** the field and set it in the three constructors; update the existing test literals with `constraint: None`.
- [ ] **Step 4: Run** `cargo test --workspace` - Expected: PASS.
- [ ] **Step 5: Commit** `git add crates && git commit -m "feat: foreign key edges carry the constraint name"`

### Task 6: Delete foreign key from the diagram

**Files:**
- Modify: `crates/dbzilla-app/src/ui/diagram.rs` (context menu near line 414, `DiagramAction`, `DiagramTab`), `crates/dbzilla-app/src/ui/workspace.rs` (`diagram_action` near line 1366, confirm dialog), `CHANGES.md`, `FEATURES.md`
- Test: workspace tests module

**Interfaces:**
- Consumes: `DiagramEdge.constraint`, `StructuralOp::DropForeignKey { table, name }`, `Capabilities.add_fk_after_create`.
- Produces: `DiagramAction::DeleteForeignKey { table: String, constraint: String }`; `DiagramTab::set_fk_drop_blocked(&mut self, reason: Option<String>)`.

- [ ] **Step 1: Write failing tests** in `workspace.rs` tests (SQLite, same helpers as `diagram_only_links_are_kept_and_deleted`):
  - `deleting_a_foreign_key_on_sqlite_is_refused_with_a_notice`: a table with a real FK, send `DiagramAction::DeleteForeignKey`, assert a notice mentioning "cannot drop a foreign key" is pushed, `ws.gate.is_none()` and the FK still exists (`pragma_foreign_key_list` count 1).
  - `foreign_key_delete_dialog_opens_when_the_driver_allows_it`: set `ws.caps.add_fk_after_create = true` on the same workspace, send the action, assert `ws.fk_delete.is_some()` with the table and constraint name (the dialog is a new `fk_delete: Option<FkDelete>` field holding both strings).
- [ ] **Step 2: Run** `cargo test -p dbzilla-app deleting_a_foreign_key foreign_key_delete_dialog` - Expected: FAIL (compile).
- [ ] **Step 3: Implement.**
  - Menu: for `EdgeKind::Real` edges show `Delete foreign key`; disabled with hover text when the tab has a block reason (SQLite: "SQLite cannot drop a foreign key from an existing table") or when `edge.constraint` is `None` ("This foreign key has no name").
  - Workspace sets the block reason on the diagram tab when it loads (`!self.caps.add_fk_after_create`), and `diagram_action` re-checks it for the notice test above.
  - On allowed: open a confirm modal "Delete foreign key `<name>` on `<table>`?"; Confirm builds `StructuralOp::DropForeignKey { table: <TableRef of that table from self.objects>, name }` and calls `submit_ddl` with `DdlAfter { closed: None, message: "Foreign key <name> deleted" }`, then request the diagram again (`request_diagram`) so the line disappears.
- [ ] **Step 4: Run** `cargo test --workspace` - Expected: PASS. Then `cargo clippy --workspace -- -D warnings` - Expected: no warnings.
- [ ] **Step 5: Manual check** with `cargo run`: on PostgreSQL and MySQL right-click a real FK line, delete it, confirm the line is gone and the gate asks at levels above Silent; on SQLite the entry is disabled with its reason.
- [ ] **Step 6: Docs and commit.** Tick "Delete a real foreign key from the ER diagram" in `FEATURES.md`, add a `CHANGES.md` entry, `git add -A crates CHANGES.md FEATURES.md && git commit -m "feat: delete a real foreign key from the ER diagram"`. Then merge into `master` with `--no-ff`.
