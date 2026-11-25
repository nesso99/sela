# Building and testing

## Cargo

```bash
cargo build --workspace
cargo run -p sela-app
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses bans sources
cargo doc --no-deps -p sela-core -p sela-drivers -p sela-config
```

Library crates can be tested without GPUI:

```bash
cargo test -p sela-core
cargo test -p sela-drivers
cargo test -p sela-config
```

OS prerequisites (C toolchain, Vulkan/Metal, SQLite headers): [installation.md](../user/installation.md).

## Headless UI tests (#93)

`sela-app` includes GPUI UI integration tests under `src/ui/ui_tests/`. They use
`#[gpui_kit::test]` / `TestAppContext` (dev-dependency feature `gpui-kit/test-support`)
and do not need a physical display.

```bash
./scripts/ui-tests.sh
```

Linux CI should install `xvfb` (script uses `xvfb-run -a` when available) plus the
same GPUI packages as the `test` job. Optional: `mesa-vulkan-drivers` / lavapipe
if a test paints. Do not put secrets or connection URLs in assertions that print
on failure.

## Docker Compose databases

```bash
docker compose up -d
```

Starts Postgres 16 and MySQL 8.4 with the sample library seed. Init scripts run only on a fresh data directory; use `docker compose down -v` after changing seed SQL.

## Integration env vars

SQLite tests need no server. Postgres and MySQL tests run when URLs are set; otherwise they print a skip line and pass. When the URL is set but the database is unreachable, the test fails.

```bash
export SELA_POSTGRES_URL='postgres://sela:sela@127.0.0.1:5432/sample?sslmode=disable'
export SELA_MYSQL_URL='mysql://sela:sela@127.0.0.1:3306/sample?ssl-mode=disabled'
cargo test -p sela-drivers
```

Optional schema overrides: `SELA_POSTGRES_SCHEMA` (default `public`) and `SELA_MYSQL_SCHEMA` (default `sample`).

CI job `db-integration` sets the same URLs against service containers.

### Postgres TLS (CI acceptance for M1-8)

Tests skip unless both env vars are set. They use the real `sela_drivers::connect` path:

| Env | Example |
| --- | --- |
| `SELA_PG_TLS_URL` | `postgres://sela:sela@127.0.0.1:5433/sample` |
| `SELA_PG_TLS_CA` | `/tmp/…/ca.crt` |

Locally (Docker required):

```bash
# Generates throwaway CA + server cert (SAN=localhost only) and starts postgres:16 with ssl=on.
eval "$(./scripts/run-pg-tls.sh 5433)"
cargo test -p sela-drivers postgres_tls -- --nocapture
docker rm -f sela-pg-tls
```

Or generate certs only into a directory you mount yourself:

```bash
CERT_DIR=$(mktemp -d)
./scripts/gen-pg-tls-certs.sh "$CERT_DIR"
# then start Postgres with ssl_cert_file / ssl_key_file / hba_file pointing at $CERT_DIR
```

Never commit key or cert material. CI job `db-tls-postgres` generates certs in the job and runs the same suite (`require`, `verify-ca` with hostname mismatch OK, `verify-full` mismatch → `DbError::Tls`, hostssl-only `disable`). MySQL live TLS is a follow-up.

## cargo-deny

[`deny.toml`](../../deny.toml) at the repo root. CI runs `cargo deny check licenses bans sources`. See [ADR 0005](../adr/0005-gpl-and-cargo-deny.md).

## Grid benchmark

```bash
# Linux: Xvfb + lavapipe for a headless GPU
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
xvfb-run -a cargo run -p sela-app --release --example grid_bench
xvfb-run -a cargo run -p sela-app --release --example grid_bench -- --wide
xvfb-run -a cargo run -p sela-app --release --example grid_bench -- --wide-unique

cargo bench -p sela-core --bench value_display
```

Numbers and the keep-DataTable decision: [benchmarks.md](./benchmarks.md), [ADR 0001](../adr/0001-keep-datatable.md).
