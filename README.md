# Sela

[![CI](https://github.com/nesso99/sela/actions/workflows/ci.yml/badge.svg)](https://github.com/nesso99/sela/actions/workflows/ci.yml)

Sela is a native desktop database client for PostgreSQL, MySQL, and SQLite. Early TablePlus-style slice: save a connection, browse schemas and tables, open tabs, page or stream results, cancel queries, and inspect structure — built with Rust and [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) via [GPUI Kit](https://github.com/longbridge/gpui-kit) **0.10.0**.

![Screenshot placeholder](docs/empty.png)

*Replace with a current product screenshot.*

## Quick start

Prebuilt binaries (Linux AppImage/`tar.gz`, macOS arm64 DMG/zip, Windows zip) are attached to [GitHub Releases](https://github.com/nesso99/sela/releases). See [installation](docs/user/installation.md) for Gatekeeper / SmartScreen caveats and Linux Vulkan runtime deps.

To build from source: Rust **1.85+**, a C toolchain, and `pkg-config`.

```bash
cargo build --workspace
cargo run -p sela-app
```

Sample SQLite library: sidebar **Sample library**, or seed your own with `scripts/seed-sqlite.sql`. Local Postgres/MySQL: `docker compose up -d` (user/password/db `sela`/`sela`/`sample`).

## Feature status

| Area | Status |
| --- | --- |
| Connections (PG / MySQL / SQLite), passwords in OS keychain | Done |
| TLS modes + CA/client cert paths (PG / MySQL); config schema v2 | Done (M1-8) |
| Schema / table / view sidebar | Done |
| Virtualized grid, typed cells, bounded result window | Done |
| Run SQL, cancel, streaming + load-more | Done |
| Read-only Structure view | Done |
| Tabs (open / close / switch / restore) | Done |
| Filter bar + column sort (table tabs) | Done |
| Query history (FTS, per-connection scope) | Done (M1-5) |
| Grid selection + copy (TSV/CSV/JSON/INSERT); light/dark/system themes | Done (M1-7) |
| Inline edit, safe mode, SSH, export | Planned — [roadmap](docs/design/roadmap.md) |

## Documentation

| Doc | Path |
| --- | --- |
| Install | [docs/user/installation.md](docs/user/installation.md) |
| Connecting | [docs/user/connecting.md](docs/user/connecting.md) |
| Keyboard shortcuts | [docs/user/keyboard-shortcuts.md](docs/user/keyboard-shortcuts.md) |
| Filtering & sorting | [docs/user/filtering.md](docs/user/filtering.md) |
| Query history | [docs/user/query-history.md](docs/user/query-history.md) |
| Copying grid data | [docs/user/copying.md](docs/user/copying.md) |
| Themes | [docs/user/themes.md](docs/user/themes.md) |
| FAQ | [docs/user/faq.md](docs/user/faq.md) |
| Architecture | [docs/dev/architecture.md](docs/dev/architecture.md) |
| Building & testing | [docs/dev/building-and-testing.md](docs/dev/building-and-testing.md) |
| Grid benchmarks | [docs/dev/benchmarks.md](docs/dev/benchmarks.md) |
| PRD | [docs/design/PRD.md](docs/design/PRD.md) |
| Specs | [docs/design/SPECS.md](docs/design/SPECS.md) |
| Roadmap | [docs/design/roadmap.md](docs/design/roadmap.md) |
| UX specs | [docs/design/ux/README.md](docs/design/ux/README.md) |
| ADRs | [docs/adr/](docs/adr/) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Changelog | [CHANGELOG.md](CHANGELOG.md) |
| Security | [SECURITY.md](SECURITY.md) |

## License

Sela is licensed under **GPL-3.0-or-later**. See [`LICENSE`](LICENSE).
