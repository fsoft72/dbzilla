# Trigger editor and ER diagram FK delete - design

For approval on 2026-11-17. Source of truth for behaviour: `SPEC.md` 8.8 and 11.3.

## Intent
Close the two items of FEATURES.md under "Tables and structure": the trigger editor (add, edit, delete) and deleting a real foreign key from the ER diagram.

## Goals
- Trigger editor replaces the read-only "Triggers" modal of the Structure tab: list on one side, editor on the other. `+ Trigger` and `Mod+I` start a new one. Only when `caps.triggers`.
- MySQL/MariaDB: fields name, timing (BEFORE/AFTER), event (INSERT/UPDATE/DELETE), body in an SQL editor.
- PostgreSQL: fields name, timing, event, FOR EACH ROW/STATEMENT, and a function chosen from the existing `RETURNS trigger` functions. No function creation. A new driver query lists those functions.
- SQLite: stays read-only, as SPEC 8.8 and the capability table say.
- Edit is drop plus create in one batch (neither engine has a usable `ALTER TRIGGER`). Delete is `DROP TRIGGER`.
- Every change goes through `submit_ddl`, so the safe-mode gate and History apply, like the foreign key dialog.
- Diagram: right-click on a real FK line shows "Delete foreign key". It uses the existing `StructuralOp::DropForeignKey` through `submit_ddl`. On SQLite the entry is disabled with the reason.

## Non-goals
Creating or editing PostgreSQL trigger functions, trigger enable/disable, SQL Server triggers, deleting inferred links (already handled), adding or deleting tables from the diagram (separate item).

## Constraints
- `TriggerInfo.statement` differs per driver: full `CREATE TRIGGER` on PostgreSQL and SQLite, body only on MySQL. The PostgreSQL editor must parse timing, event, level and function out of the definition, or the driver must return them as fields.
- A real FK edge must carry its constraint name, which `DiagramEdge` does not hold today.
- Pure logic (DDL generation, definition parsing) goes in `dbzilla-core` with unit tests, UI in `dbzilla-app`.

## Approaches
PostgreSQL trigger editor: A, structured fields plus a function picker (chosen). B, free text `CREATE TRIGGER`: simpler but unguided. C, A plus a plpgsql function editor: too large for this item.
FK delete: one sensible way, the existing drop operation, because the gate and DDL already exist.

## Plan of work
Two branches, one commit each, no push: `feature/trigger-editor` first, then `feature/diagram-fk-delete`. FEATURES.md and CHANGES.md updated on each.

## Left to the builder
Exact layout of the editor, how the PostgreSQL definition is parsed or returned, the confirmation wording for deletes.
