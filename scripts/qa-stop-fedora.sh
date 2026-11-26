#!/usr/bin/env bash
# Stop the Sela started by scripts/qa-launch-fedora.sh. Run it after every QA session.
#
# Usage: scripts/qa-stop-fedora.sh
#
# Reads the PID file written by the launch script. If it is missing, falls back to
# the `PID=<pid>` lines recorded in the launch log. Never uses pkill/killall: only
# PIDs recorded by the launcher are signalled, and only if they still run a `sela`
# binary. Sends TERM, waits up to STOP_WAIT_SECS, then KILL. Idempotent: exits 0
# when nothing is running. Always removes the PID file.
#
# Env:
#   SELA_PID_FILE   default: ${XDG_RUNTIME_DIR:-/tmp/sela-qa-$UID}/sela-qa.pid
#   SELA_LOG        default: ${XDG_RUNTIME_DIR:-/tmp/sela-qa-$UID}/sela-qa.log
#   STOP_WAIT_SECS  seconds to wait after TERM before KILL (default: 5)
set -uo pipefail

RUN_DIR="${XDG_RUNTIME_DIR:-/tmp/sela-qa-$(id -u)}"
PID_FILE="${SELA_PID_FILE:-$RUN_DIR/sela-qa.pid}"
SELA_LOG="${SELA_LOG:-$RUN_DIR/sela-qa.log}"
WAIT="${STOP_WAIT_SECS:-5}"

pids=()
if [[ -f "$PID_FILE" ]]; then
  read -r p < "$PID_FILE" || true
  [[ "$p" =~ ^[0-9]+$ ]] && pids+=("$p")
elif [[ -f "$SELA_LOG" ]]; then
  while IFS= read -r p; do pids+=("$p"); done \
    < <(sed -n 's/^qa-launch: PID=\([0-9][0-9]*\)$/\1/p' "$SELA_LOG")
fi

# Only signal live processes whose command is a sela binary (guards against PID reuse).
live=()
for p in "${pids[@]}"; do
  kill -0 "$p" 2>/dev/null || continue
  [[ "$(ps -o comm= -p "$p" 2>/dev/null)" == sela* ]] && live+=("$p")
done

if [[ ${#live[@]} -eq 0 ]]; then
  echo "qa-stop: no Sela running"
  rm -f "$PID_FILE"
  exit 0
fi

kill -TERM "${live[@]}" 2>/dev/null || true
for ((i = 0; i < WAIT * 10; i++)); do
  alive=0
  for p in "${live[@]}"; do kill -0 "$p" 2>/dev/null && alive=1; done
  [[ $alive -eq 0 ]] && break
  sleep 0.1
done
for p in "${live[@]}"; do
  if kill -0 "$p" 2>/dev/null; then
    echo "qa-stop: PID $p ignored TERM; sending KILL" >&2
    kill -KILL "$p" 2>/dev/null || true
  fi
done
rm -f "$PID_FILE"
echo "qa-stop: stopped ${live[*]}"
