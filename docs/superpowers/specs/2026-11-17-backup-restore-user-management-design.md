# Backup/restore and user management - design

Approved in conversation on 2026-11-17. Source of truth for behaviour: `SPEC.md` 11.5 and 12.3.

## Intent
Close two items of FEATURES.md: backup and restore (PostgreSQL, MySQL, MariaDB, plus a tar.bz2 backup for SQLite) and user management with a SQL preview before apply.

## Goals
- Backup/restore by spawning `pg_dump`, `pg_restore`, `mysqldump`, `mysql` found on PATH or in a configured directory. Passwords through env vars only.
- SQLite backup is a `tar.bz2` of the db file and its `-wal`, `-shm`, `-journal` sidecars. Restore extracts to a chosen path and never overwrites without explicit confirmation.
- User management is a diff from the original model to the edited one, shown as SQL before Apply, executed through the safe mode and recorded in History.

## Non-goals
Bundled dump binaries, SQL Server users, Tier 2 drivers, a custom dump writer.

## Approaches
External tools (chosen, per SPEC): compatible with `pg_restore` archives. Native dump writer: rejected, not compatible. Bundled binaries: excluded by SPEC.

## Plan of work
Two branches, one commit each, no push: `feature/backup-restore` first, then `feature/user-management`.
Core crate holds pure logic (argument builders, privilege diff) with unit tests; the app crate holds the sheets.
