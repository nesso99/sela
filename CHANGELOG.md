# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-24

### Fixed

- Copy as INSERT: non-finite floats (`NaN` / ±Infinity) are quoted for Postgres (`'NaN'`, `'Infinity'`, `'-Infinity'`) and refused on MySQL/SQLite with a status message naming the column. JSON copy emits a JSON string for those values (valid JSON; not a bare `NaN` token). Roadmap/SPECS M1-3 and M1-7 status markers updated to Done (#17/#22, #24).

### Added

- GitHub Actions release workflow (`.github/workflows/release.yml`): tag `v*` publishes a GitHub Release; `workflow_dispatch` supports dry-run / RC builds that upload workflow artifacts without a final release. Artifacts: Linux `tar.gz` + AppImage, macOS Apple Silicon (arm64) `.dmg` + `.zip` (unsigned; **Intel Macs are not supported yet**), Windows `.zip`, plus `SHA256SUMS` and notes from this changelog section. Each binary is smoke-tested with `sela --version`.
- Prebuilt install docs per platform, including the macOS arm64-only limitation, unsigned-build caveats (macOS Gatekeeper, Windows SmartScreen), and Linux Vulkan / GPU runtime dependencies (`docs/user/installation.md`).
- M1-5 query history (US-10): `history.sqlite` with FTS5 (mode `0600`), global 10 000-row retention, per-connection `record_history` (default on), bottom history panel with scope/outcome/search, Open / Insert / Copy / Rerun / Delete / Clear history. Rerun uses `is_safe_to_rerun_for_load_more` and confirms otherwise; Open/Insert never auto-run. Password-setting DDL (`CREATE`/`ALTER` `USER`|`ROLE` … `PASSWORD`, `IDENTIFIED BY`, `SET PASSWORD`) is skipped (not redacted) with status note `not saved to history (contains a password)`. User docs: `docs/user/query-history.md`.
- M1-7 selection, copy, and themes (US-14 + light/dark): grid cell/row/column and Shift-range selection; typed clipboard path (`CopyCell`) so text `NULL` is not confused with SQL NULL; copy as TSV (Cmd/Ctrl+C), CSV, JSON, or INSERT from the grid context menu (INSERT quotes identifiers per dialect, escapes MySQL backslashes, refuses decode/unsupported cells and unloaded rows with a status message; SQL-result INSERT uses placeholder `table` plus `-- replace table name`); Light / Dark / System via the status-bar menu, persisted in `settings.json` (no secrets); Sela `ThemeConfig` overrides and `SelaPalette` gap colors from `docs/design/ux/06-theme-tokens.md`; SQL result strip is a tab stop with Left/Right Prev/Next. User docs: [copying.md](docs/user/copying.md), [themes.md](docs/user/themes.md).
- M1-4 SQL editor (GPUI): `Editor` widget with resizable/collapsible column, statement-at-caret and selection run, run-all with sequential execution and up to eight result snapshots, gutter marks, Postgres error underline when position is parseable, and header find bar (`docs/design/ux/03-sql-editor.md`).
- Dialect-aware `split_statements(sql, dialect)`: Postgres `$tag$…$tag$` dollar quotes (so plpgsql bodies and `$1` placeholders split correctly), MySQL `\'` escapes inside `'…'`, Postgres backslash escapes only in `E'…'` / `e'…'`, SQLite without backslash escapes.
- Per-connection TLS for PostgreSQL and MySQL: modes `disable` / `prefer` / `require` / `verify-ca` / `verify-full`, optional CA and client certificate paths, `DbError::Tls` for handshake/cert failures (including rustls peer-cert errors that sqlx surfaces as Io), and `connections.json` `schema_version` 2 (PR #15). Legacy v1 `ssl: true` maps in memory to `require` (no migration rewriter). Client keys must be unencrypted PEM for now. `verify-ca` skips hostname verification with a trusted CA (sqlx ≥ 0.9 / [sqlx#3861](https://github.com/launchbadge/sqlx/pull/3861)). CI job `db-tls-postgres` exercises require, verify-ca (hostname mismatch OK), verify-full hostname mismatch → `DbError::Tls`, and hostssl-only disable.
- UX specs for the main window, a review of filter/sort, and the SQL editor (`docs/design/ux/`).
- Cargo workspace with `sela-core`, `sela-drivers`, `sela-config`, and `sela-app` (binary `sela`) (#2).
- Drivers for PostgreSQL, MySQL, and SQLite via sqlx; typed `Value` decode for NUMERIC, UUID, JSON, timestamptz, and related types (#3, #5).
- Saved connections with passwords in the OS keychain (`keyring`); never written to `connections.json`.
- Virtualized results grid (gpui-component `DataTable`) with paging and typed column headers (#8).
- Bounded `ResultWindow` (16 pages × 100 rows), lazy Json storage, and truncated cell display (#9).
- Query cancel (Stop / Cmd/Ctrl+.) with server-side cancel on Postgres and MySQL; batched streaming and load-more for arbitrary SQL up to a 1 000-row cap (#10).
- Read-only Structure view: columns, indexes, foreign keys, and DDL for all three engines (#11).
- Tabs for tables and queries: open/close/switch (Cmd/Ctrl+T/W/1..9), per-tab cancel-on-close, restore from `tabs.json` without auto-running SQL (#12).
- Filter bar and column-header sort for table Data tabs (M1-3 / US-04; UX: `docs/design/ux/02-filter-sort-review.md`): AND-only conditions, bound parameters + `quote_ident`, text `LIKE` / `NOT LIKE` with `ESCAPE '!'` (`%`/`_` wildcards), column-kind operators, status-bar Count over the filtered WHERE (cancellable; ticks `Counting… · elapsed`; failures/cancels do not set `view.error`), multi-column Shift-click sort (max 3, marks ▲1/▼2), read-only predicate strip under the grid (not written into `TabView.sql`). Filtered tables show `unknown rows · N on this page` and clear the page total.
- Postgres text fallback for arrays, enums, inet/cidr, macaddr, money, bit, point, int4range, and tsvector; other undecodable binary types show `<TYPE>` (#13).
- GPL-3.0-or-later license, `deny.toml`, and CI `cargo-deny` checks (#14).
- GitHub Actions CI: fmt, clippy, tests (Linux/macOS; Windows on main), and Postgres/MySQL integration (#4, #6).
- Reorganized user / design / ADR docs and offline link checks (#16).
- UX specs for query history, the connection dialog (including TLS), and light/dark theme tokens (`docs/design/ux/04`–`06`, indexed in `docs/design/ux/README.md`).

### Changed

- Upgrade sqlx from 0.8.6 to 0.9.1. Drop the vendored `sqlx-core` backport of [sqlx#3861](https://github.com/launchbadge/sqlx/pull/3861) (`vendor/`, `[patch.crates-io]`, and the `vendor-drift` CI job). Feature flags move from `runtime-tokio-rustls` to `runtime-tokio` + `tls-rustls`; `macros` is enabled for `Executor::describe`. Dynamic SQL goes through `AssertSqlSafe` / a small `query` helper. Align `bit-vec` 0.8 and `ipnetwork` 0.21 with sqlx. (#19)
- New PostgreSQL and MySQL connections default to `tls.mode = prefer` (dialog / `SavedConnection::new`). Saved configs with no `tls` value still deserialize as `disable`; `TlsMode::default` stays `disable`. Prefer shows the hint: *prefer may fall back to unencrypted; use require or verify-full for remote servers*.
- M1-3 polish: bool filter values use a true/false menu (null ops still hide the control); filter-bar keyboard extras (`secondary-shift-enter` insert row, `secondary-backspace` remove/clear sole row when the focused column/value field is empty or focus is not in a text field — otherwise delete-to-line-start for macOS ⌘⌫, Escape leaves the bar per the UX spec); UX review notes for sortable header clicks, legacy contains/starts/ends restore, and ASC→DESC→none sort cycle.
- Product rename from Birch to Sela (#1).
- UI split into focused workspace modules under ~400 lines (#7).
