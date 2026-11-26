#!/usr/bin/env bash
# Launch Sela on a Wayland desktop (Fedora GNOME) so `orca computer` can drive it.
#
# Usage: scripts/qa-launch-fedora.sh [--build] [--no-a11y]
#   --build     run `cargo build --release -p sela-app` first (default: only if the binary is missing)
#   --no-a11y   do not touch org.a11y.Status (skip the AT-SPI enable step)
#
# Env:
#   SELA_BIN      binary to run (default: <repo>/target/release/sela)
#   SELA_LOG      log file (default: ${XDG_RUNTIME_DIR:-/tmp}/sela-qa.log)
#   WAIT_SECS     max seconds to wait for the window (default: 30)
#   ORCA_CLI      orca executable used to confirm the window (default: $ORCA_CLI_COMMAND, else orca)
#                 NOTE: bare `orca` on Fedora outside an Orca terminal is the GNOME screen reader.
#   VK_ICD_FILENAMES  honoured if set; otherwise left alone (real GPU driver).
#
# Prints: PID=<pid>, WINDOW_TITLE=<title>, APP_SELECTOR=pid:<pid>. Exit non-zero on failure.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELA_BIN="${SELA_BIN:-$ROOT/target/release/sela}"
SELA_LOG="${SELA_LOG:-${XDG_RUNTIME_DIR:-/tmp}/sela-qa.log}"
WAIT_SECS="${WAIT_SECS:-30}"
ORCA_CLI="${ORCA_CLI:-${ORCA_CLI_COMMAND:-orca}}"
BUILD=0
A11Y=1
for a in "$@"; do
  case "$a" in
    --build) BUILD=1 ;;
    --no-a11y) A11Y=0 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown arg: $a" >&2; exit 2 ;;
  esac
done

if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${DISPLAY:-}" ]]; then
  echo "qa-launch: no WAYLAND_DISPLAY or DISPLAY; run from the desktop session" >&2
  exit 1
fi

if [[ $BUILD -eq 1 || ! -x "$SELA_BIN" ]]; then
  echo "qa-launch: building release binary" >&2
  (cd "$ROOT" && cargo build --release -p sela-app)
  [[ -x "$SELA_BIN" ]] || { echo "qa-launch: $SELA_BIN not found after build" >&2; exit 1; }
fi

# GNOME ships org.a11y.Status IsEnabled=false unless a screen reader or
# toolkit-accessibility is on. accesskit_unix (GPUI) only registers on the
# AT-SPI bus when it is true, so `orca computer list-apps` would never show
# Sela. The property is runtime-only (not persisted) and reset on session restart.
if [[ $A11Y -eq 1 ]]; then
  if [[ "$(busctl --user get-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled 2>/dev/null)" != "b true" ]]; then
    busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true \
      || echo "qa-launch: warning: could not enable AT-SPI (Sela will not be visible to orca computer)" >&2
  fi
fi

: > "$SELA_LOG"
setsid "$SELA_BIN" >>"$SELA_LOG" 2>&1 < /dev/null &
PID=$!

# Wait for the window. Prefer the AT-SPI view via orca; fall back to process liveness.
TITLE=""
for ((i = 0; i < WAIT_SECS * 2; i++)); do
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "qa-launch: sela exited early; log:" >&2
    tail -20 "$SELA_LOG" >&2
    exit 1
  fi
  if [[ $A11Y -eq 1 ]] && command -v "$ORCA_CLI" >/dev/null 2>&1; then
    out="$("$ORCA_CLI" computer list-windows --app "pid:$PID" --json 2>/dev/null || true)"
    TITLE="$(printf '%s' "$out" | sed -n 's/.*"title": *"\([^"]*\)".*/\1/p' | head -1)"
    [[ -n "$TITLE" ]] && break
  fi
  sleep 0.5
done

if [[ -z "$TITLE" ]]; then
  if [[ $A11Y -eq 1 ]]; then
    echo "qa-launch: process $PID is alive but not visible via AT-SPI after ${WAIT_SECS}s (see docs/qa/fedora-gui-launch.md)" >&2
    exit 1
  fi
  TITLE="Sela"
fi

echo "PID=$PID"
echo "WINDOW_TITLE=$TITLE"
echo "APP_SELECTOR=pid:$PID"
echo "LOG=$SELA_LOG"
