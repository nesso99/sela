#!/usr/bin/env bash
# Start an openssh-server bastion in front of Postgres (and optional MySQL) for
# M3-1 SSH integration tests. Prints `export …` lines for eval.
#
# Topology (Docker network):
#   - postgres:16 (and optionally mysql:8.4) with NO published host ports
#   - linuxserver/openssh-server (or atmoz/sftp-style) publishing 2222:2222
#     so the runner reaches only the bastion; the DB is reachable from the
#     bastion by Docker DNS name (`postgres` / `mysql`).
#
# Keys and known_hosts are generated under a temp dir — never committed.
# Usage:
#   eval "$(./scripts/run-ssh-bastion.sh)"
#   cargo test -p sela-drivers --test ssh_tunnel -- --nocapture
#   docker rm -f sela-ssh-bastion sela-ssh-pg sela-ssh-mysql
#   docker network rm sela-ssh-net

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKDIR="${RUNNER_TEMP:-/tmp}/sela-ssh-$$"
mkdir -p "$WORKDIR/keys" "$WORKDIR/home/.ssh"
chmod 700 "$WORKDIR/home/.ssh"

SSH_PORT="${SELA_SSH_PUBLISH_PORT:-2222}"
PASSWORD="sela-ssh-test"
PASSPHRASE="sela-key-pass"
USER_NAME="sela"
NETWORK="sela-ssh-net"

echo "Generating throwaway SSH keys in $WORKDIR" >&2
ssh-keygen -t ed25519 -N "" -f "$WORKDIR/keys/id_ed25519" -q
ssh-keygen -t ed25519 -N "$PASSPHRASE" -f "$WORKDIR/keys/id_ed25519_enc" -q
cp "$WORKDIR/keys/id_ed25519.pub" "$WORKDIR/keys/authorized_keys"
chmod 600 "$WORKDIR/keys/id_ed25519" "$WORKDIR/keys/id_ed25519_enc" \
  "$WORKDIR/keys/authorized_keys"

# Docker network + DB without host publish.
# Keep docker chatter off stdout — callers use `eval "$(./scripts/run-ssh-bastion.sh)"`.
docker network inspect "$NETWORK" >/dev/null 2>&1 \
  || docker network create "$NETWORK" >/dev/null
docker rm -f sela-ssh-pg sela-ssh-mysql sela-ssh-bastion >/dev/null 2>&1 || true

# Image digests pinned (resolved from :16 / :8.4 / :latest via registry API).
PG_IMAGE="postgres@sha256:07abc3f43c9395b0bdbbab36bc25ddb0e8d83cd4fcafb5fb7d6abe14ff156dd8"
MYSQL_IMAGE="mysql@sha256:8c233bad33d520a7aae1c1e619c3a9775f4a1ba2d463d692fada04ec7f40be00"
# lscr.io/linuxserver/openssh-server:latest
OPENSSH_IMAGE="lscr.io/linuxserver/openssh-server@sha256:c8431b8982e7eb3935ebf9c46607d19c6417e39c12ce884cc66945df2e9334e8"

docker run -d --name sela-ssh-pg --network "$NETWORK" \
  -e POSTGRES_USER=sela \
  -e POSTGRES_PASSWORD=sela \
  -e POSTGRES_DB=sample \
  "$PG_IMAGE" >/dev/null

docker run -d --name sela-ssh-mysql --network "$NETWORK" \
  -e MYSQL_ROOT_PASSWORD=sela \
  -e MYSQL_USER=sela \
  -e MYSQL_PASSWORD=sela \
  -e MYSQL_DATABASE=sample \
  "$MYSQL_IMAGE" >/dev/null

# linuxserver/openssh-server: password + pubkey, custom port 2222 inside.
docker run -d --name sela-ssh-bastion --network "$NETWORK" \
  -e PUID="$(id -u)" \
  -e PGID="$(id -g)" \
  -e TZ=UTC \
  -e USER_NAME="$USER_NAME" \
  -e USER_PASSWORD="$PASSWORD" \
  -e PASSWORD_ACCESS=true \
  -e PUBLIC_KEY_FILE=/keys/authorized_keys \
  -p "${SSH_PORT}:2222" \
  -v "$WORKDIR/keys:/keys:ro" \
  "$OPENSSH_IMAGE" >/dev/null

echo "Waiting for bastion, Postgres, and MySQL…" >&2
bastion_up=false
pg_up=false
for i in $(seq 1 90); do
  if docker exec sela-ssh-pg pg_isready -U sela -d sample >/dev/null 2>&1; then
    pg_up=true
  fi
  if ! $bastion_up; then
    if ssh -i "$WORKDIR/keys/id_ed25519" -o StrictHostKeyChecking=no \
         -o UserKnownHostsFile=/dev/null -o ConnectTimeout=2 -p "$SSH_PORT" \
         "${USER_NAME}@127.0.0.1" "true" >/dev/null 2>&1; then
      bastion_up=true
    fi
  fi
  if $pg_up && $bastion_up; then
    break
  fi
  sleep 1
done
# MySQL can take longer to accept connections.
for i in $(seq 1 90); do
  if docker exec sela-ssh-mysql mysqladmin ping -h 127.0.0.1 -usela -psela --silent >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

# Capture the bastion host key into known_hosts (and a deliberately wrong one).
ssh-keyscan -p "$SSH_PORT" 127.0.0.1 2>/dev/null > "$WORKDIR/home/.ssh/known_hosts" || true
# Wrong key: reuse a different key type entry for the same host.
ssh-keygen -t ed25519 -N "" -f "$WORKDIR/keys/fake_host" -q
{
  echo -n "127.0.0.1 "
  # known_hosts line: host keytype base64
  awk '{print $1" "$2}' "$WORKDIR/keys/fake_host.pub"
} > "$WORKDIR/home/.ssh/known_hosts.bad"
# Also cover [127.0.0.1]:port form used when port != 22.
if [[ "$SSH_PORT" != "22" ]]; then
  {
    echo -n "[127.0.0.1]:${SSH_PORT} "
    awk '{print $1" "$2}' "$WORKDIR/keys/fake_host.pub"
  } > "$WORKDIR/home/.ssh/known_hosts.bad"
  # keyscan may write without brackets; rewrite known_hosts with bracket form too.
  if [[ -s "$WORKDIR/home/.ssh/known_hosts" ]]; then
    # Keep keyscan output; also ensure bracket form exists for russh.
    KEYLINE=$(grep -v '^#' "$WORKDIR/home/.ssh/known_hosts" | head -1 | awk '{print $2" "$3}')
    echo "[127.0.0.1]:${SSH_PORT} $KEYLINE" >> "$WORKDIR/home/.ssh/known_hosts"
  fi
fi

# Install the unencrypted public key into the container user if PUBLIC_KEY_FILE
# was not enough (image-dependent). Best-effort.
docker exec sela-ssh-bastion bash -c \
  "mkdir -p /config/.ssh && cat /keys/authorized_keys >> /config/.ssh/authorized_keys && chmod 600 /config/.ssh/authorized_keys" \
  >/dev/null 2>&1 || true

cat <<EOF
export SELA_SSH_HOST=127.0.0.1
export SELA_SSH_PORT=${SSH_PORT}
export SELA_SSH_USER=${USER_NAME}
export SELA_SSH_PASSWORD=${PASSWORD}
export SELA_SSH_KEY=${WORKDIR}/keys/id_ed25519
export SELA_SSH_KEY_PASSPHRASE=${PASSPHRASE}
export SELA_SSH_KEY_ENCRYPTED=${WORKDIR}/keys/id_ed25519_enc
export SELA_SSH_KNOWN_HOSTS=${WORKDIR}/home/.ssh/known_hosts
export SELA_SSH_KNOWN_HOSTS_BAD=${WORKDIR}/home/.ssh/known_hosts.bad
export SELA_SSH_DB_HOST=postgres
export SELA_SSH_DB_PORT=5432
export SELA_SSH_DB_USER=sela
export SELA_SSH_DB_PASSWORD=sela
export SELA_SSH_DB_NAME=sample
export SELA_SSH_MYSQL_HOST=mysql
export SELA_SSH_MYSQL_PORT=3306
export HOME=${WORKDIR}/home
EOF

echo "Bastion ready on 127.0.0.1:${SSH_PORT}. Workdir: $WORKDIR" >&2
echo "Note: set HOME to the throwaway home so tests inject paths via SshPaths::from_home, or pass ssh_paths in ConnectAuth (the Rust tests do)." >&2
