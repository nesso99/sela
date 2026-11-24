#!/usr/bin/env bash
# Extract the Keep a Changelog section for a given version into a file.
# Usage: extract-changelog.sh <version> [changelog-path] [output-path]
# version may be "0.1.0" or "v0.1.0".
set -euo pipefail

raw="${1:?version required}"
version="${raw#v}"
changelog="${2:-CHANGELOG.md}"
output="${3:-release-notes.md}"

if [[ ! -f "$changelog" ]]; then
  echo "error: changelog not found: $changelog" >&2
  exit 1
fi

awk -v ver="$version" '
  BEGIN { printing = 0 }
  /^## \[Unreleased\]/ { printing = 0; next }
  $0 ~ ("^## \\[" ver "\\]") { printing = 1; next }
  /^## \[/ { if (printing) exit }
  printing { print }
' "$changelog" | sed -e 's/[[:space:]]*$//' | awk '
  NF { seen = 1 }
  seen { print }
' >"$output"

if [[ ! -s "$output" ]]; then
  echo "error: no changelog section found for version $version in $changelog" >&2
  exit 1
fi

echo "Wrote $(wc -l <"$output" | tr -d ' ') lines to $output"
