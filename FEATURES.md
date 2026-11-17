# Features to implement

Source of truth for behaviour is `SPEC.md`. Section numbers refer to it.
Out of scope on purpose (see the SPEC change log): plugins, light and Auto themes, multi-window, team features, multi-caret, split panes.

## Database tools

- [x] Metrics Board (11.2): boards, data table, scoreboard, bar, line and pie charts, input, dropdown, button, `$name` variables, refresh rates, import and export
- [ ] Metrics Board, still missing: Exporter, Importer and Connection objects, wiring by dragging between connection points (today a checkbox list in the object settings), input submit modes, board list in the left sidebar, Add to Metrics Board from a result chart, `Tools > Show Metrics Board` menu entry
- [x] Process list UI with kill (11.4): PostgreSQL and MySQL/MariaDB
- [ ] Process list, still missing: SQL Server, left list of databases for MySQL, Quick Look and export of the page, `Mod+.` shortcut (taken by Cancel Running Queries), `Tools` menu entry
- [x] User management with a SQL preview before apply (11.5): PostgreSQL and MySQL/MariaDB
- [ ] User management, still missing: column level privileges for PostgreSQL, table level grants for MySQL, `GRANT OPTION`, renaming a user, `Tools` menu entry
- [x] Backup and restore for PostgreSQL, MySQL and MariaDB (12.3), plus a tar.bz2 backup for SQLite
- [ ] Backup and restore, still missing: tool version picker, connection picker inside the sheet, job entry in the notifications bell, `File` menu entries, Open Anything entries
- [ ] View Using Encoding to re-decode text columns (11.6)

## Tables and structure

- [x] Trigger editor: add, edit and delete triggers, `Mod+I` for a new one (8.8): MySQL/MariaDB and PostgreSQL, SQLite read only
- [x] Delete a real foreign key from the ER diagram (drop through the safe-mode gate)

## ER diagram

- [x] Add and delete tables from the diagram, through the pending changes (11.3): Add table, Delete table, Undo, Redo, Apply (`Mod+S`), Discard
- [x] Table settings window: colour, column editor, index editor (name is in the column editor), Delete table. Foreign keys are made by dragging, not from the column editor
- [x] `Mod+F` shortcut for the diagram search, and Up and Down to move between matches (Enter and Shift+Enter too)
- [x] Remember the "Inferred links" switch between openings (per diagram)
- [-] Route lines around tables instead of behind them (dropped on purpose)
- [x] Show diagram in the sidebar item menu. There is no Tools menu yet, so that entry waits for a menu bar
- [x] Warn in the link dialog when the two columns have different types
- [x] Export the diagram as an SVG image (PNG not done)

## Connections

- [ ] SSH tunnel
- [ ] Import and export of connection files (6.5)
- [ ] Reopen last session: save the open workspaces and tabs on quit and restore them at launch (6.7). The setting exists but does nothing yet
- [ ] Passcode to protect the application (14.6)

## Editor and settings

- [ ] Keymap editor (14.5)
- [ ] Verify the Vim key binding visually and fix what is off

## AI

- [ ] AI assistant (15): chat UI, OpenAI-compatible, Anthropic and Ollama providers with streaming, database access for the model, privacy options

## Other databases

- [ ] Tier 2 drivers (SPEC 1.1)
- [ ] Tier 3 document-style databases (13)

## Distribution and quality

- [ ] macOS builds in the release workflow (removed after the CI kept failing)
- [ ] Verify the application on macOS and Windows (only Linux is tested today)
- [ ] Packaging and installers
