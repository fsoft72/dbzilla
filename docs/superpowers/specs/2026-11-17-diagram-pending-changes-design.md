# ER diagram: pending changes (SPEC 11.3)

## Intent
Edit the schema from the diagram (add and delete tables, table settings, foreign keys) through a queue of pending changes, applied together with `Mod+S`, with Undo and Redo, as SPEC 11.3 asks.

## Goals / non-goals
- Goals: Add Table, Delete table, table settings (name, colour, structure editor, index editor), real foreign keys created and deleted through the queue, Undo/Redo, Apply and Discard, one batch through the safe-mode gate showing all SQL.
- Non-goals: routing lines around tables (dropped), PNG export, editing foreign keys from the column editor inside the diagram (they are made by dragging).
- Anti-goal: a half applied schema the user cannot see or undo.

## Approaches
- A (chosen): one `StructDraft` per touched or new table, plus lists of dropped tables and foreign key changes. Undo and Redo keep snapshots of the whole pending state. Reuses `StructDraft::to_ops`, `show_columns` and `show_indexes`.
- B: a log of `StructuralOp`. Rejected: the table editor works on drafts, so every field edit would become an operation.

## Design
- `ui/diagram_pending.rs`: `DiagramPending` (Clone) holds `existing` drafts by original name, `created` drafts by id, `dropped`, `fk_adds`, `fk_drops`, undo and redo stacks. Pure logic, unit tested.
- The diagram draws the effective structures (base structures with the pending changes applied). New tables have a green border, dropped tables red, edited tables amber.
- Foreign keys: a new FK on an existing table is an `fk_adds` entry (SQLite needs the base structure to rebuild); on a new table it is inline in its draft. Deleting a real FK is an `fk_drops` entry. Links "only in the diagram" stay immediate.
- Colours are a view property: saved at once in `diagrams.json`, not pending.
- Apply order: drop FKs, create tables (referenced first), alter existing tables, add FKs, drop tables. Cycles between new tables are refused with a message.
- SQLite: adding an FK to a table that also has structure changes is refused (it rebuilds from the stored structure): apply the structure first.
- `Mod+S`, `Mod+Z`, `Mod+Shift+Z` act on the diagram when it is the focused tab. Rearrange is disabled while changes are pending.
- On a failed apply the pending changes stay; on success they are cleared and the diagram reloads.

## Left to the builder
Window layout of the table settings, wording of messages.
