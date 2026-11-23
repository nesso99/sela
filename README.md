# Sela

[![CI](https://github.com/nesso99/sela/actions/workflows/ci.yml/badge.svg)](https://github.com/nesso99/sela/actions/workflows/ci.yml)

Sela is a native desktop database client for PostgreSQL, MySQL, and SQLite. It is an early slice of a TablePlus-style app: save a connection, browse schemas and tables, page through rows, and run SQL.

The window is [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), pinned through [GPUI Kit](https://github.com/longbridge/gpui-kit) **0.10.0**. That release depends on `gpui-pre` **0.3.12** (the published GPUI snapshot GPUI Kit was built against). Tables, inputs, and the sidebar tree come from `gpui-component` in the same release.

The first window, with no connections yet, and the sample library opened to `main.bookmarks`:

![Empty Sela window](docs/empty.png)

![Sample library, bookmarks table](docs/sample-library.png)

## What you can do

- Create, edit, and delete saved connections for PostgreSQL, MySQL, and SQLite.
- Connections are stored in a local JSON file. Passwords are not in that file. Sela writes them to the OS keychain with the [`keyring`](https://crates.io/crates/keyring) crate when a secret service is available.
- Browse schemas (or databases) and tables for the active connection, including views.
- Open a table in a virtualized grid, 100 rows per page, with the column type in the header.
- Write one SQL statement and run it with Ctrl+Enter or Cmd+Enter. Results use the same grid. Errors, row counts, and timing show in the status bar.
- Database I/O runs on a dedicated Tokio runtime. The GPUI thread only awaits the result.

## Prerequisites

Rust **1.85** or newer (`rustup update stable`). A C toolchain and `pkg-config` are required because GPUI and SQLite link native libraries.

### macOS

GPUI renders with Metal.

- Install Xcode, launch it once, and install the macOS platform components.
- SQLite headers: `brew install sqlite pkg-config` if `cargo build` cannot find `sqlite3`.

Passwords go to the login Keychain.

### Linux

GPUI renders with Vulkan on Wayland or X11. You need a display server; the process does not draw without one.

Debian or Ubuntu:

```bash
sudo apt install build-essential pkg-config libsqlite3-dev libssl-dev \
  libvulkan-dev libwayland-dev libxkbcommon-dev libxcb1-dev libx11-dev \
  libxrandr-dev libxi-dev libfontconfig1-dev libfreetype6-dev libdbus-1-dev \
  libasound2-dev fonts-dejavu-core
```

A Vulkan driver must be visible to the loader (`vulkaninfo` is a useful check). On a machine without a GPU, Mesa's lavapipe software rasterizer can stand in:

```bash
sudo apt install mesa-vulkan-drivers
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
```

Passwords use the Secret Service API (gnome-keyring or KWallet) over D-Bus. If no secret service is running, Sela still saves the connection and keeps the password in memory until you quit. It does not write the password into the JSON file. The status bar says when that fallback is in use.

### Windows

GPUI Kit targets Windows (Direct3D through wgpu) and `keyring` uses Credential Manager. This tree was built and tested on Linux, so treat Windows as untested until someone runs it there.

## Build and run

This repo is a Cargo workspace. The GPUI binary package is `sela-app`; the binary name is still `sela`.

```bash
cargo build --workspace
cargo run -p sela-app
```

The connections file is:

| OS | Path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/sela/connections.json`, or `~/.config/sela/connections.json` |
| macOS | `~/Library/Application Support/Sela/connections.json` |
| Windows | `%APPDATA%\Sela\connections.json` |

Set `SELA_CONFIG_DIR` to point the file somewhere else. The file mode is `0600` on Unix.

### Sample SQLite library

Click **Sample library** in the sidebar. Sela creates `sample.sqlite` next to the connections file and opens it. The database has `authors`, `books`, `bookmarks`, and a `books_in_print` view.

You can also build that file yourself:

```bash
mkdir -p examples
sqlite3 examples/library.sqlite < scripts/seed-sqlite.sql
```

Then add a SQLite connection whose file path is `examples/library.sqlite`.

### PostgreSQL and MySQL

`docker-compose.yml` starts Postgres 16 and MySQL 8.4 with the same library data. Postgres also creates an `archive` schema. MySQL also creates an `archive` database and grants it to `sela`.

```bash
docker compose up -d
```

| Engine | Host | Port | User | Password | Database |
| --- | --- | --- | --- | --- | --- |
| PostgreSQL | `127.0.0.1` | `5432` | `sela` | `sela` | `sample` |
| MySQL | `127.0.0.1` | `3306` | `sela` | `sela` | `sample` |

Leave **Require SSL** off for these containers. They do not serve TLS.

The init scripts run only on a brand-new data directory. If you change a seed file, remove the compose volumes (`docker compose down -v`) before starting again.

## Tests

```bash
cargo test --workspace
```

Library crates can be tested without pulling in GPUI:

```bash
cargo test -p sela-core
cargo test -p sela-drivers
cargo test -p sela-config
```

SQLite tests use a temporary file and do not need a server. They cover the catalog, NULL versus text, blobs, paging at 100 rows, a trailing line comment, empty result columns, and statement errors.

PostgreSQL and MySQL tests run only when their URL is set. Otherwise they print a skip line and pass. When the URL is set but the database is unreachable, the test fails.

```bash
export SELA_POSTGRES_URL='postgres://sela:sela@127.0.0.1:5432/sample?sslmode=disable'
export SELA_MYSQL_URL='mysql://sela:sela@127.0.0.1:3306/sample?ssl-mode=disabled'
cargo test -p sela-drivers
```

Optional schema overrides: `SELA_POSTGRES_SCHEMA` (default `public`) and `SELA_MYSQL_SCHEMA` (default `sample`).

## Layout

```text
crates/sela-core/     domain types, SQL helpers, DatabaseSession (no GPUI)
crates/sela-drivers/ Postgres, MySQL, and SQLite drivers (sqlx + tokio)
crates/sela-config/  connections.json and OS keychain access
crates/sela-app/     GPUI window binary (`sela`)
scripts/             seed SQL for SQLite, Postgres, and MySQL
```

`DatabaseSession` is the extension point for another engine. The UI only sees schemas, tables, and `StatementOutcome`.

Generated SQL quotes identifiers (`"name"` or `` `name` ``) and rejects an empty identifier or a null byte. The editor accepts one statement. `SELECT`, `WITH`, and `VALUES` are wrapped so the server applies `LIMIT` and `OFFSET`. `SHOW`, `EXPLAIN`, and `PRAGMA` are run as-is and capped in the client. Anything else is executed, and the status bar shows the affected-row count.

## Roadmap toward TablePlus

The pieces below are intentionally not in this version. The module split is there so they can land without rewriting the drivers.

- Multiple result tabs, and a tab per open table.
- Inline cell editing and inserting or deleting rows.
- Filters, sorts, and a where-bar that composes SQL instead of replacing it.
- Structure view: columns, indexes, foreign keys, and DDL.
- SQL syntax highlighting, history, and multiple statements.
- Export to CSV or JSON, and import.
- SSH tunnels, SSL certificates, and connection groups.
- More engines (MariaDB is close to the MySQL driver; SQL Server and Redis are not).

## Keychain limitation

Sela never writes a password into `connections.json`. On Linux that depends on a running Secret Service. A headless session, or a session with no unlocked keyring, cannot persist the password. Sela keeps it in process memory, tells you in the status bar, and asks again the next time you launch the app.
