#!/usr/bin/env bash
# Start Postgres 16 with SSL using throwaway certs, then print env for tests.
# Usage: ./scripts/run-pg-tls.sh [port]
# Requires Docker. Tear down with: docker rm -f sela-pg-tls
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${1:-5433}"
CERT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sela-pg-tls.XXXXXX")"
"$ROOT/scripts/gen-pg-tls-certs.sh" "$CERT_DIR" >/dev/null

# Official image runs as uid 999 (postgres). mktemp dirs are mode 0700, so
# open the directory for traversal; chmod the key after chown needs sudo.
chmod 755 "$CERT_DIR"
if command -v sudo >/dev/null 2>&1; then
  sudo chown 999:999 "$CERT_DIR/server.key" "$CERT_DIR/server.crt" "$CERT_DIR/ca.crt" "$CERT_DIR/pg_hba.conf"
  sudo chmod 600 "$CERT_DIR/server.key"
else
  chown 999:999 "$CERT_DIR/server.key" "$CERT_DIR/server.crt" "$CERT_DIR/ca.crt" "$CERT_DIR/pg_hba.conf" 2>/dev/null || true
  chmod 600 "$CERT_DIR/server.key" 2>/dev/null || true
fi

docker rm -f sela-pg-tls >/dev/null 2>&1 || true
docker run -d --name sela-pg-tls \
  -e POSTGRES_USER=sela \
  -e POSTGRES_PASSWORD=sela \
  -e POSTGRES_DB=sample \
  -p "${PORT}:5432" \
  -v "$CERT_DIR:/certs:ro" \
  postgres:16 \
  -c ssl=on \
  -c ssl_cert_file=/certs/server.crt \
  -c ssl_key_file=/certs/server.key \
  -c hba_file=/certs/pg_hba.conf \
  >/dev/null

for _ in $(seq 1 40); do
  if docker exec sela-pg-tls pg_isready -U sela -d sample >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

export SELA_PG_TLS_URL="postgres://sela:sela@127.0.0.1:${PORT}/sample"
export SELA_PG_TLS_CA="$CERT_DIR/ca.crt"
cat <<EOF
# Postgres TLS is up (container sela-pg-tls, certs in $CERT_DIR).
export SELA_PG_TLS_URL='$SELA_PG_TLS_URL'
export SELA_PG_TLS_CA='$SELA_PG_TLS_CA'
# Run: cargo test -p sela-drivers postgres_tls -- --nocapture
# Stop: docker rm -f sela-pg-tls && rm -rf '$CERT_DIR'
EOF
