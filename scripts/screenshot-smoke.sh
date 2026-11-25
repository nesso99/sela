#!/usr/bin/env bash
# Capture smoke screenshots of a release `sela` binary under Xvfb + Mesa.
#
# Env (required):
#   SELA_BIN  - path to the sela executable
#   OUT_DIR   - directory for PNG output
#
# Optional:
#   DISPLAY_NUM   - X display number (default: 99)
#   SCREEN_SIZE   - Xvfb screen geometry (default: 1280x800x24)
#   WAIT_SECS     - seconds to wait for the main window (default: 60)
#   SETTLE_SECS   - seconds to settle after window appears (default: 2)
#
# Writes:
#   OUT_DIR/01-main-window.png
#   OUT_DIR/02-file-menu.png
#
# Exit non-zero on failure (caller may set continue-on-error).
set -euo pipefail

SELA_BIN="${SELA_BIN:?SELA_BIN required}"
OUT_DIR="${OUT_DIR:?OUT_DIR required}"
DISPLAY_NUM="${DISPLAY_NUM:-99}"
SCREEN_SIZE="${SCREEN_SIZE:-1280x800x24}"
WAIT_SECS="${WAIT_SECS:-60}"
SETTLE_SECS="${SETTLE_SECS:-2}"

test -x "$SELA_BIN"
mkdir -p "$OUT_DIR"

export DISPLAY=":${DISPLAY_NUM}"

# Soft Vulkan ICD for GPUI/wgpu under Xvfb (lavapipe).
if [[ -z "${VK_ICD_FILENAMES:-}" ]]; then
  for icd in \
    /usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
    /usr/share/vulkan/icd.d/lvp_icd.json; do
    if [[ -f "$icd" ]]; then
      export VK_ICD_FILENAMES="$icd"
      break
    fi
  done
fi
echo "screenshot-smoke: DISPLAY=$DISPLAY VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-<unset>}"

cleanup() {
  local code=$?
  if [[ -n "${SELA_PID:-}" ]] && kill -0 "$SELA_PID" 2>/dev/null; then
    kill "$SELA_PID" 2>/dev/null || true
    wait "$SELA_PID" 2>/dev/null || true
  fi
  if [[ -n "${WM_PID:-}" ]] && kill -0 "$WM_PID" 2>/dev/null; then
    kill "$WM_PID" 2>/dev/null || true
    wait "$WM_PID" 2>/dev/null || true
  fi
  if [[ -n "${XVFB_PID:-}" ]] && kill -0 "$XVFB_PID" 2>/dev/null; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  exit "$code"
}
trap cleanup EXIT

Xvfb "$DISPLAY" -screen 0 "$SCREEN_SIZE" -ac +extension GLX +render -noreset &
XVFB_PID=$!
sleep 1
if ! kill -0 "$XVFB_PID" 2>/dev/null; then
  echo "error: Xvfb failed to start on $DISPLAY" >&2
  exit 1
fi

# Lightweight WM helps some toolkits map/focus windows under Xvfb.
if command -v openbox >/dev/null 2>&1; then
  openbox &
  WM_PID=$!
  sleep 1
fi

"$SELA_BIN" >"$OUT_DIR/sela-stdout.log" 2>"$OUT_DIR/sela-stderr.log" &
SELA_PID=$!

WID=""
for ((i = 1; i <= WAIT_SECS; i++)); do
  if ! kill -0 "$SELA_PID" 2>/dev/null; then
    echo "error: sela exited before a window appeared (after ${i}s)" >&2
    echo "--- sela stderr ---" >&2
    cat "$OUT_DIR/sela-stderr.log" >&2 || true
    exit 1
  fi
  # Prefer exact title from WindowOptions; fall back to class/name contains.
  WID="$(xdotool search --onlyvisible --name '^Sela$' 2>/dev/null | head -n1 || true)"
  if [[ -z "$WID" ]]; then
    WID="$(xdotool search --onlyvisible --name 'Sela' 2>/dev/null | head -n1 || true)"
  fi
  if [[ -n "$WID" ]]; then
    echo "screenshot-smoke: window id=$WID after ${i}s"
    break
  fi
  sleep 1
done

if [[ -z "$WID" ]]; then
  echo "error: timed out waiting for Sela window (${WAIT_SECS}s)" >&2
  echo "--- sela stderr ---" >&2
  cat "$OUT_DIR/sela-stderr.log" >&2 || true
  echo "--- xdotool search ---" >&2
  xdotool search --name '' 2>/dev/null || true
  exit 1
fi

sleep "$SETTLE_SECS"
xdotool windowactivate --sync "$WID" || true
sleep 0.5

# Prefer the app window; fall back to root so we still get *something* for QA.
capture() {
  local dest="$1"
  if ! import -window "$WID" "$dest" 2>/dev/null; then
    echo "warning: import -window $WID failed; capturing root" >&2
    import -window root "$dest"
  fi
  echo "screenshot-smoke: wrote $dest ($(wc -c <"$dest") bytes)"
}

capture "$OUT_DIR/01-main-window.png"

# File is a content-sized ghost Button in the SSD menu bar (top-left of client).
# Click near the label; geometry is relative to the window's top-left.
xdotool windowactivate --sync "$WID" || true
xdotool mousemove --window "$WID" 36 48 click 1
sleep 1

capture "$OUT_DIR/02-file-menu.png"

ls -lah "$OUT_DIR"/*.png
echo "screenshot-smoke: done"
