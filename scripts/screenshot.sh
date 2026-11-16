#!/usr/bin/env bash
# Usage: [ACTIONS="click:X,Y;key:ctrl+k;type:text;sleep:1"] scripts/screenshot.sh OUT.png [WAIT_SECONDS] [-- app args...]
# Launches the debug binary, optionally replays actions (coordinates relative to the window),
# captures its window on X11, then closes it.
set -u
OUT="$1"; WAIT="${2:-4}"; shift; shift 2>/dev/null || true
[ "${1:-}" = "--" ] && shift
BIN="$(dirname "$0")/../target/debug/dbzilla"
"$BIN" "$@" >/tmp/dbzilla-shot.log 2>&1 &
PID=$!
sleep "$WAIT"
WID=$(xdotool search --pid "$PID" 2>/dev/null | tail -1)
if [ -z "$WID" ]; then echo "no window found" >&2; kill "$PID" 2>/dev/null; exit 1; fi
IFS=';' read -ra STEPS <<< "${ACTIONS:-}"
for step in "${STEPS[@]:-}"; do
  [ -z "$step" ] && continue
  kind="${step%%:*}"; arg="${step#*:}"
  case "$kind" in
    click) xdotool mousemove --window "$WID" "${arg%,*}" "${arg#*,}"; sleep 0.15; xdotool click 1; sleep 0.5 ;;
    dclick) xdotool mousemove --window "$WID" "${arg%,*}" "${arg#*,}"; sleep 0.15; xdotool click --repeat 2 --delay 80 1; sleep 0.8 ;;
    rclick) xdotool mousemove --window "$WID" "${arg%,*}" "${arg#*,}"; sleep 0.15; xdotool click 3; sleep 0.5 ;;
    key) xdotool key --window "$WID" "$arg"; sleep 0.4 ;;
    type) xdotool type --window "$WID" --delay 40 "$arg"; sleep 0.4 ;;
    sleep) sleep "$arg" ;;
  esac
done
import -window "$WID" "$OUT"
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
[ -s "$OUT" ] || { echo "screenshot failed" >&2; exit 1; }
