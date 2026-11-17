# Features to implement

Source of truth for behaviour is `SPEC.md`. Section numbers refer to it.
Out of scope on purpose (see the SPEC change log): plugins, light and Auto themes, multi-window, team features, multi-caret, split panes.

## Database tools

- [x] Metrics Board (11.2): boards, data table, scoreboard, bar, line and pie charts, input, dropdown, button, `$name` variables, refresh rates, import and export
- [ ] Metrics Board, still missing: Exporter, Importer and Connection objects, wiring by dragging between connection points (today a checkbox list in the object settings), input submit modes, board list in the left sidebar, Add to Metrics Board from a result chart, `Tools > Show Metrics Board` menu entry
- [x] Process list UI with kill (11.4): PostgreSQL and MySQL/MariaDB
- [ ] Process list, still missing: SQL Server, left list of databases for MySQL, Quick Look and export of the page, `Mod+.` shortcut (taken by Cancel Running Queries), `Tools` menu entry
- [ ] User management with a SQL preview before apply (11.5)
- [ ] Backup and restore for PostgreSQL, MySQL and MariaDB (12.3)
- [ ] View Using Encoding to re-decode text columns (11.6)

## Tables and structure

- [ ] Trigger editor: add, edit and delete triggers, `Mod+I` for a new one (8.8). Today the Structure tab only lists them
- [ ] Delete a real foreign key from the ER diagram (drop through the safe-mode gate)

## ER diagram

- [ ] Add and delete tables from the diagram, through the pending changes (11.3)
- [ ] Table settings popover: name, colour, structure editor, index editor
- [ ] `Mod+F` shortcut for the diagram search, and Up and Down to move between matches
- [ ] Remember the "Inferred links" switch between openings
- [ ] Route lines around tables instead of behind them
- [ ] Menu entry under Tools and an entry in the sidebar for Show Diagram
- [ ] Warn in the link dialog when the two columns have different types
- [ ] Export the diagram as an image (PNG or SVG)

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
