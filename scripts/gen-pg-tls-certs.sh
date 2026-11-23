#!/usr/bin/env bash
# Generate a throwaway CA + Postgres server cert (CN/SAN = localhost only).
# Usage: ./scripts/gen-pg-tls-certs.sh <output-dir>
# Never commit the output. Keys are mode 0600.
set -euo pipefail

OUT="${1:?usage: $0 <output-dir>}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -keyout "$OUT/ca.key" \
  -out "$OUT/ca.crt" \
  -subj "/CN=Sela Test CA" \
  >/dev/null 2>&1

openssl req -newkey rsa:2048 -nodes \
  -keyout "$OUT/server.key" \
  -out "$OUT/server.csr" \
  -subj "/CN=localhost" \
  >/dev/null 2>&1

cat >"$OUT/server.ext" <<'EOF'
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost
EOF

openssl x509 -req -in "$OUT/server.csr" -CA "$OUT/ca.crt" -CAkey "$OUT/ca.key" \
  -CAcreateserial -out "$OUT/server.crt" -days 1 -extfile "$OUT/server.ext" \
  >/dev/null 2>&1

rm -f "$OUT/server.csr" "$OUT/server.ext" "$OUT/ca.srl"
chmod 600 "$OUT/ca.key" "$OUT/server.key"
chmod 644 "$OUT/ca.crt" "$OUT/server.crt"

# hostssl-only HBA: plain TCP is rejected; TLS + password is allowed.
cat >"$OUT/pg_hba.conf" <<'EOF'
# TYPE  DATABASE  USER  ADDRESS      METHOD
local   all       all                trust
hostssl all       all   0.0.0.0/0    scram-sha-256
hostssl all       all   ::/0         scram-sha-256
hostnossl all     all   0.0.0.0/0    reject
hostnossl all     all   ::/0         reject
EOF

echo "$OUT"
