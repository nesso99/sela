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
