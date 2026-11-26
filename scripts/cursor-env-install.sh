#!/usr/bin/env bash
# Idempotent Cloud Agent Build install: Linux GPUI deps + Rust components + cargo pre-warm.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> Step A: Linux GPUI & test system dependencies"

PACKAGES=(
  build-essential
  pkg-config
  libsqlite3-dev
  libssl-dev
  libvulkan-dev
  libwayland-dev
  libxkbcommon-dev
  libxkbcommon-x11-dev
  libxcb1-dev
  libx11-dev
  libxrandr-dev
  libxi-dev
  libfontconfig1-dev
  libfreetype6-dev
  libdbus-1-dev
  libasound2-dev
  fonts-dejavu-core
  mold
  xvfb
  openssh-client
  sshpass
)

# True if $1 is installed, or is a virtual/transitional name Provided by an
# installed package (e.g. libfreetype6-dev <- libfreetype-dev on Ubuntu Noble).
# Avoid `grep -q` in a pipe under `pipefail` (early close → SIGPIPE → false negative).
pkg_satisfied() {
  local pkg="$1" provided
  if dpkg -s "$pkg" >/dev/null 2>&1; then
    return 0
  fi
  while IFS= read -r provided; do
    if [[ "$provided" == "$pkg" ]]; then
      return 0
    fi
  done < <(dpkg-query -W -f='${Provides}\n' 2>/dev/null | tr ', ' '\n')
  return 1
}

MISSING=()
for pkg in "${PACKAGES[@]}"; do
  if ! pkg_satisfied "$pkg"; then
    MISSING+=("$pkg")
  fi
done

if ((${#MISSING[@]} > 0)); then
  echo "Installing missing packages: ${MISSING[*]}"
  if [[ "$(id -u)" -eq 0 ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${MISSING[@]}"
  else
    sudo DEBIAN_FRONTEND=noninteractive apt-get update
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${MISSING[@]}"
  fi
else
  echo "All system packages already installed; skipping apt-get."
fi

echo "==> Step B: Rust toolchain"

if [[ -d "${HOME}/.cargo/bin" ]]; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
fi

CARGO=(cargo)
if command -v rustup >/dev/null 2>&1; then
  # Workspace rust-version is 1.85+; ensure a current stable toolchain is available.
  # Skip `toolchain install` when stable is already present so re-runs stay offline-fast.
  if rustup run stable rustc --version >/dev/null 2>&1; then
    echo "stable toolchain already installed: $(rustup run stable rustc --version)"
  else
    rustup toolchain install stable --profile minimal
  fi
  rustup component add rustfmt clippy --toolchain stable
  CARGO=(cargo +stable)
else
  echo "rustup not found; skipping rustfmt/clippy components."
fi

echo "==> Step C: Pre-warm Cargo/Rust target/"

# Configure mold in the VM user cargo config (never committed) so subsequent
# cargo invocations outside this script share the same fingerprint. Do not
# export RUSTFLAGS — that would rebuild when later commands omit it.
if command -v mold >/dev/null 2>&1 && [[ "$(uname -m)" == "x86_64" ]]; then
  CARGO_HOME_DIR="${HOME}/.cargo"
  CARGO_CONFIG="${CARGO_HOME_DIR}/config.toml"
  mkdir -p "${CARGO_HOME_DIR}"
  if [[ -f "${CARGO_CONFIG}" ]] && grep -Fqe 'fuse-ld=mold' "${CARGO_CONFIG}"; then
    echo "mold already configured in ${CARGO_CONFIG}; leaving untouched."
  else
    if [[ -f "${CARGO_CONFIG}" && -s "${CARGO_CONFIG}" ]]; then
      # Ensure a trailing newline before appending so we do not glue lines.
      if [[ "$(tail -c1 "${CARGO_CONFIG}" | wc -l)" -eq 0 ]]; then
        printf '\n' >> "${CARGO_CONFIG}"
      fi
      printf '\n' >> "${CARGO_CONFIG}"
    fi
    cat >> "${CARGO_CONFIG}" <<'EOF'
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
EOF
    echo "Configured mold linker in ${CARGO_CONFIG}"
  fi
fi

"${CARGO[@]}" test --workspace --no-run --locked
"${CARGO[@]}" clippy --workspace --all-targets --locked

echo "==> cursor-env-install.sh complete."
