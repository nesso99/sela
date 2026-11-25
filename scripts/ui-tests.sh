#!/usr/bin/env bash
# Headless GPUI UI tests for high-risk flows (#93).
#
# Prefer GPUI's test platform (TestAppContext / #[gpui_kit::test]) so no display
# is required. This script still starts Xvfb when available so a future test that
# needs a real window backend can run on Linux CI without a separate job.
#
# System packages (Debian/Ubuntu), in addition to the usual GPUI build deps from
# .github/workflows/ci.yml:
#   xvfb
#   mesa-vulkan-drivers   # optional; lavapipe if a test ever paints
#
# Usage (from repo root):
#   ./scripts/ui-tests.sh
#
# Do not wire this into ci.yml here — the maintainer adds the job separately.
#
# Tests live in crates/sela-app/src/ui/ui_tests/ (bin crate unit tests).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for arg in "$@"; do
  case "$arg" in
    -h|--help)
      sed -n '2,20p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

echo "==> sela UI tests (headless GPUI / TestAppContext)"
echo "    cwd: $ROOT"

if command -v xvfb-run >/dev/null 2>&1; then
  echo "    display: xvfb-run -a (available)"
  RUNNER=(xvfb-run -a)
else
  echo "    display: none (TestAppContext only; install xvfb for CI parity)"
  RUNNER=()
fi

# Soft GPU hint for any paint path; harmless when unused.
if [[ -z "${VK_ICD_FILENAMES:-}" && -f /usr/share/vulkan/icd.d/lvp_icd.x86_64.json ]]; then
  export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
  echo "    VK_ICD_FILENAMES=$VK_ICD_FILENAMES"
fi

FILTER='ui_tests::'

echo "==> cargo test -p sela-app -- ${FILTER}"
"${RUNNER[@]}" cargo test -p sela-app -- "${FILTER}"

echo "==> UI tests finished OK"
