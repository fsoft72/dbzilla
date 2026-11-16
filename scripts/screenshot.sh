#!/usr/bin/env bash
# Usage: scripts/screenshot.sh OUT.png [WAIT_SECONDS] [-- app args...]
# Launches the debug binary, captures its window on X11, then closes it.
set -u
OUT="$1"; WAIT="${2:-4}"; shift; shift 2>/dev/null || true
[ "${1:-}" = "--" ] && shift
BIN="$(dirname "$0")/../target/debug/dbzilla"
"$BIN" "$@" >/tmp/dbzilla-shot.log 2>&1 &
PID=$!
sleep "$WAIT"
WID=$(xdotool search --pid "$PID" 2>/dev/null | tail -1)
if [ -n "$WID" ]; then import -window "$WID" "$OUT"; else echo "no window found" >&2; fi
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
[ -s "$OUT" ] || { echo "screenshot failed" >&2; exit 1; }
