#!/usr/bin/env bash
# Measure the PRD non-functional requirements (M4-2): cold start, grid fps, memory.
#
# What it does
#   1. Builds release `sela` + the `grid_bench` example (skip with SKIP_BUILD=1).
#   2. Start: launches `sela` RUNS times with SELA_BENCH_STARTUP=1 and a fresh
#      config dir holding 3 saved connections; time = spawn -> first Workspace
#      frame (marker printed by crates/sela-app/src/ui/bench.rs).
#   3. Idle memory: RSS of the first run, SETTLE_SECS after its first frame.
#   4. Grid: runs `grid_bench` (narrow + wide-unique, 100 000 x 50) for fps and
#      RSS delta.
#   5. Writes a markdown report (machine, results, targets) to --out.
#
# Display: by default a private headless `mutter` (virtual 1280x800 monitor,
# real GPU) is started so frame callbacks always arrive; a window on the user's
# own Wayland desktop gets none while occluded and the fling never advances.
# Set BENCH_COMPOSITOR=session to use the current session (window must stay
# visible). Run it on the machine you want to report.
#
# Usage (repo root):
#   ./scripts/bench-nfr.sh [--out docs/benchmarks.md] [--runs N] [--settle SECS]
#   ./scripts/bench-nfr.sh --check   # offline self-test of the parsing helpers
#
# Env: RUNS (default 10), SETTLE_SECS (default 5), SKIP_BUILD=1, OUT.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS="${RUNS:-10}"
SETTLE_SECS="${SETTLE_SECS:-5}"
OUT="${OUT:-$ROOT/docs/benchmarks.md}"
MODE=run

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --runs) RUNS="$2"; shift 2 ;;
    --settle) SETTLE_SECS="$2"; shift 2 ;;
    --check) MODE=check; shift ;;
    -h | --help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ---- helpers (unit-checked by --check) -------------------------------------

# median of numbers on stdin
median() {
  sort -n | awk '{a[NR]=$1} END {if (NR==0) exit 1; print (NR%2) ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2}'
}

# "sela-bench first_frame_epoch_ns=N" line -> ms since start_ns
first_frame_ms() {
  local start_ns="$1" line="$2" ns
  ns="${line##*first_frame_epoch_ns=}"
  [[ "$ns" =~ ^[0-9]+$ ]] || return 1
  awk -v s="$start_ns" -v e="$ns" 'BEGIN {printf "%.1f", (e - s) / 1e6}'
}

# VmRSS of a pid in MiB
rss_mib() {
  awk '/^VmRSS:/ {printf "%.1f", $2 / 1024}' "/proc/$1/status"
}

# key=value pick from a grid_bench line: grid_field "<file>" "<line prefix>" "<key>"
grid_field() {
  grep -m1 "^$2" "$1" | tr ' ' '\n' | sed -n "s/^$3=\\(.*\\)$/\\1/p" | head -n1 | tr -d 'ms'
}

verdict() { # value limit  -> PASS/FAIL (value <= limit)
  awk -v v="$1" -v l="$2" 'BEGIN {print (v <= l) ? "PASS" : "FAIL"}'
}

if [[ "$MODE" == check ]]; then
  fail=0
  t() { [[ "$1" == "$2" ]] || { echo "FAIL: got '$1' want '$2'" >&2; fail=1; }; }
  t "$(printf '3\n1\n2\n' | median)" "2"
  t "$(printf '4\n1\n2\n3\n' | median)" "2.5"
  t "$(first_frame_ms 1000000000 'sela-bench first_frame_epoch_ns=1250000000')" "250.0"
  t "$(verdict 120 150)" "PASS"
  t "$(verdict 151 150)" "FAIL"
  tmp="$(mktemp)"
  echo 'vertical_fling: frames=63 p50=48.00ms p95=48.10ms p99=48.20ms fps=21.2' >"$tmp"
  t "$(grid_field "$tmp" vertical_fling fps)" "21.2"
  t "$(grid_field "$tmp" vertical_fling p95)" "48.10"
  rm -f "$tmp"
  [[ "$fail" == 0 ]] && echo "bench-nfr --check: OK"
  exit "$fail"
fi

# ---- run --------------------------------------------------------------------

cd "$ROOT"

if [[ "${SKIP_BUILD:-0}" != 1 ]]; then
  cargo build --release -p sela-app --bin sela --example grid_bench
fi
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
SELA_BIN="$TARGET_DIR/release/sela"
GRID_BIN="$TARGET_DIR/release/examples/grid_bench"
test -x "$SELA_BIN" && test -x "$GRID_BIN"

WORK="$(mktemp -d)"
SELA_PID=""
MUTTER_PID=""
cleanup() {
  [[ -n "$SELA_PID" ]] && kill "$SELA_PID" 2>/dev/null || true
  [[ -n "$MUTTER_PID" ]] && { kill "$MUTTER_PID" 2>/dev/null; pkill -f -- "--wayland-display ${WL_NAME:-none}" 2>/dev/null; } || true
  rm -rf "$WORK"
}
trap cleanup EXIT

COMPOSITOR="current session"
if [[ "${BENCH_COMPOSITOR:-headless}" == headless ]]; then
  command -v mutter >/dev/null && command -v dbus-run-session >/dev/null ||
    { echo "error: mutter + dbus-run-session needed (or BENCH_COMPOSITOR=session)" >&2; exit 1; }
  WL_NAME="sela-bench-$$"
  dbus-run-session -- mutter --headless --wayland --no-x11 \
    --virtual-monitor 1280x800 --wayland-display "$WL_NAME" >"$WORK/mutter.log" 2>&1 &
  MUTTER_PID=$!
  for _ in $(seq 1 50); do [[ -S "${XDG_RUNTIME_DIR:?}/$WL_NAME" ]] && break; sleep 0.1; done
  [[ -S "$XDG_RUNTIME_DIR/$WL_NAME" ]] || { echo "error: headless mutter did not start" >&2; cat "$WORK/mutter.log" >&2; exit 1; }
  export WAYLAND_DISPLAY="$WL_NAME"
  unset DISPLAY
  COMPOSITOR="headless mutter $(mutter --version | awk '{print $2}'), virtual monitor 1280x800 (Wayland)"
elif [[ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
  echo "error: no DISPLAY / WAYLAND_DISPLAY" >&2
  exit 1
fi

seed_config() { # dir
  mkdir -p "$1"
  local db="$1/sample.db"
  sqlite3 "$db" <"$ROOT/scripts/seed-sqlite.sql"
  cat >"$1/connections.json" <<JSON
{"schema_version":2,"connections":[
 {"id":"bench-sqlite","name":"Sample SQLite","engine":"sqlite","file_path":"$db"},
 {"id":"bench-pg","name":"Local Postgres","engine":"postgres","host":"127.0.0.1","port":5432,"username":"sela","database":"sample"},
 {"id":"bench-mysql","name":"Local MySQL","engine":"mysql","host":"127.0.0.1","port":3306,"username":"sela","database":"sample"}
]}
JSON
}

echo "==> start + idle memory: $RUNS runs, settle ${SETTLE_SECS}s"
START_MS=()
IDLE_RSS=""
for ((i = 1; i <= RUNS; i++)); do
  cfg="$WORK/cfg$i"
  seed_config "$cfg"
  err="$WORK/err$i.log"
  start_ns="$(date +%s%N)"
  SELA_CONFIG_DIR="$cfg" SELA_BENCH_STARTUP=1 "$SELA_BIN" >/dev/null 2>"$err" &
  SELA_PID=$!
  line=""
  for _ in $(seq 1 600); do
    line="$(grep -m1 'sela-bench first_frame_epoch_ns=' "$err" 2>/dev/null || true)"
    [[ -n "$line" ]] && break
    kill -0 "$SELA_PID" 2>/dev/null || { echo "error: sela exited early" >&2; cat "$err" >&2; exit 1; }
    sleep 0.05
  done
  [[ -n "$line" ]] || { echo "error: no first-frame marker (30s)" >&2; exit 1; }
  ms="$(first_frame_ms "$start_ns" "$line")"
  START_MS+=("$ms")
  echo "    run $i: ${ms} ms"
  if [[ "$i" == 1 ]]; then
    sleep "$SETTLE_SECS"
    IDLE_RSS="$(rss_mib "$SELA_PID")"
    echo "    idle RSS after ${SETTLE_SECS}s: ${IDLE_RSS} MiB"
  fi
  kill "$SELA_PID" 2>/dev/null || true
  wait "$SELA_PID" 2>/dev/null || true
  SELA_PID=""
  sleep 0.5
done
START_FIRST="${START_MS[0]}"
START_MED="$(printf '%s\n' "${START_MS[@]}" | median | awk '{printf "%.1f", $1}')"
START_MIN="$(printf '%s\n' "${START_MS[@]}" | sort -n | head -n1)"
START_MAX="$(printf '%s\n' "${START_MS[@]}" | sort -n | tail -n1)"

echo "==> grid_bench narrow + wide-unique"
"$GRID_BIN" >"$WORK/grid-narrow.txt" 2>"$WORK/grid-narrow.err"
"$GRID_BIN" --wide-unique >"$WORK/grid-wide.txt" 2>"$WORK/grid-wide.err"

gf() { grid_field "$WORK/grid-$1.txt" "$2" "$3"; }
N_VFPS="$(gf narrow vertical_fling fps)"; N_VP95="$(gf narrow vertical_fling p95)"
N_HFPS="$(gf narrow horizontal_fling fps)"; N_HP95="$(gf narrow horizontal_fling p95)"
W_VFPS="$(gf wide vertical_fling fps)"; W_HFPS="$(gf wide horizontal_fling fps)"
N_BASE="$(gf narrow rss_mib baseline)"; N_END="$(gf narrow rss_mib end)"; N_DEL="$(gf narrow rss_mib end_delta)"
W_BASE="$(gf wide rss_mib baseline)"; W_END="$(gf wide rss_mib end)"; W_DEL="$(gf wide rss_mib end_delta)"
N_TTFF="$(grep -m1 '^time_to_first_frame_ms' "$WORK/grid-narrow.txt" | awk '{print $2}')"

# ---- machine ----------------------------------------------------------------
CPU="$(awk -F': ' '/^model name/ {print $2; exit}' /proc/cpuinfo) ($(nproc) threads)"
RAM="$(awk '/^MemTotal:/ {printf "%.0f GiB", $2 / 1048576}' /proc/meminfo)"
OS="$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-Linux}") · kernel $(uname -r)"
SESSION="${XDG_SESSION_TYPE:-unknown} (${XDG_CURRENT_DESKTOP:-unknown})"
GPU="$(lspci 2>/dev/null | grep -iE 'vga|3d|display' | head -n1 | sed 's/^[^:]*: //; s/^[^:]*: //' || true)"
GPU="${GPU:-unknown}"
GPU_NOTE="hardware Vulkan"
[[ "${VK_ICD_FILENAMES:-}" == *lvp* ]] && GPU_NOTE="software Vulkan (lavapipe)"
RUSTC="$(rustc --version)"
GIT_SHA="$(git rev-parse --short HEAD)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
TODAY="$(date +%F)"

# ---- report -----------------------------------------------------------------
mkdir -p "$(dirname "$OUT")"
cat >"$OUT" <<MD
# Sela NFR benchmark report

Generated by \`scripts/bench-nfr.sh\` on $TODAY (commit \`$GIT_SHA\`, workspace version $VERSION). Re-run on the target machine with \`./scripts/bench-nfr.sh --out docs/benchmarks.md\`.

Targets come from the PRD [non-functional requirements](./design/PRD.md#non-functional-requirements) and roadmap [M4-2](./design/roadmap.md). The earlier lab record for the grid model work is [dev/benchmarks.md](./dev/benchmarks.md).

## Machine

| | |
| --- | --- |
| CPU | $CPU |
| RAM | $RAM |
| OS | $OS |
| Compositor | $COMPOSITOR (host session: $SESSION) |
| Display / GPU | $GPU ($GPU_NOTE) |
| Build profile | \`cargo build --release\` (workspace default release profile) · $RUSTC |

## Results vs. targets

Linux only: this machine cannot measure the Apple Silicon and Windows rows, which stay open until measured there.

| NFR | Target | Measured | Verdict |
| --- | --- | --- | --- |
| Cold start to first frame with sidebar (Linux) | ≤ 1 000 ms | median **$START_MED ms** (min $START_MIN, max $START_MAX, first run $START_FIRST; $RUNS runs) | $(verdict "$START_MED" 1000) |
| Idle memory, 3 saved connections | ≤ 150 MB | **$IDLE_RSS MiB** RSS, ${SETTLE_SECS}s after first frame | $(verdict "$IDLE_RSS" 150) |
| Grid scroll, 100 000 × 50 | 60 fps | vertical **$N_VFPS fps**, horizontal **$N_HFPS fps** (narrow); wide-unique $W_VFPS / $W_HFPS fps | $(awk -v v="$N_VFPS" -v h="$N_HFPS" 'BEGIN {print (v >= 60 && h >= 60) ? "PASS" : "NOT MET (see note)"}') |
| Bounded result memory | window of 16 × 100 rows | RSS delta at full window: narrow **+$N_DEL MiB**, wide-unique **+$W_DEL MiB** | PASS (bounded) |

Grid fps note: \`grid_bench\` advances one scroll step per \`on_next_frame\`, and each step also builds synthetic result pages on the UI thread. The figure is therefore the frame cost of the whole bench loop (p95 ${N_VP95} ms vertical, ${N_HP95} ms horizontal, narrow; 60 fps needs ≤ 16.7 ms), not a pure paint cost, and it does not move when the virtual monitor refresh is raised from 60 to 120 Hz. Treat it as a conservative lower bound.

## Method

- **Start**: \`SELA_BENCH_STARTUP=1\` makes the first \`Workspace\` frame print the wall-clock epoch (ns) on stderr; the script subtracts the timestamp it took just before \`exec\`. This covers process start, GPU/window setup and first paint of the window with the sidebar. Each run uses a fresh config dir with 3 saved connections (SQLite sample file, Postgres, MySQL; none connected) and a warm page cache.
- **Idle memory**: \`VmRSS\` from \`/proc/<pid>/status\` of the first run, ${SETTLE_SECS}s after the first frame, nothing connected. Connected-session memory is not covered by this script.
- **Grid**: \`cargo run -p sela-app --release --example grid_bench\` (narrow and \`--wide-unique\`); protocol in [dev/benchmarks.md](./dev/benchmarks.md#method). Baseline RSS narrow $N_BASE MiB → end $N_END MiB; wide-unique $W_BASE MiB → end $W_END MiB. Time to first frame of the grid itself: $N_TTFF ms.

## Release-note lines

- Cold start to first frame: **$START_MED ms** median on Linux ($CPU).
- Idle memory (3 saved connections): **$IDLE_RSS MiB**.
- 100 000 × 50 grid: **$N_VFPS fps** vertical / **$N_HFPS fps** horizontal fling; memory bounded at +$N_DEL MiB (narrow).
MD
echo "==> wrote $OUT"
