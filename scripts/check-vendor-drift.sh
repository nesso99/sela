#!/usr/bin/env bash
# Fail if vendor/sqlx-core differs from upstream sqlx-core 0.8.6 + vendor/sqlx-core-0.8.6-sela.patch.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL https://crates.io/api/v1/crates/sqlx-core/0.8.6/download | tar xz -C "$tmp"
cd "$tmp/sqlx-core-0.8.6"
rm -f .cargo_vcs_info.json
patch -p1 --quiet --no-backup-if-mismatch < "$root/vendor/sqlx-core-0.8.6-sela.patch"
diff -r "$tmp/sqlx-core-0.8.6" "$root/vendor/sqlx-core"
echo "vendor/sqlx-core matches upstream 0.8.6 + patch"
