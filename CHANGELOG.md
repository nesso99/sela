# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once a stable release is tagged. Until then, work lands on `main` under **Unreleased**.

## [Unreleased]

### Added

- Per-connection TLS for PostgreSQL and MySQL: modes `disable` / `prefer` / `require` / `verify-ca` / `verify-full`, optional CA and client certificate paths, `DbError::Tls` for handshake/cert failures (including rustls peer-cert errors that sqlx surfaces as Io), and `connections.json` `schema_version` 2 (PR #15). Legacy v1 `ssl: true` maps in memory to `require` (no migration rewriter). Client keys must be unencrypted PEM for now. `verify-ca` skips hostname verification with a trusted CA. This relies on a vendored `sqlx-core` 0.8.6 with a one-line backport of [launchbadge/sqlx#3861](https://github.com/launchbadge/sqlx/pull/3861) (see `vendor/README.md`; CI job `vendor-drift` checks it against upstream); removal is tracked in [#19](https://github.com/nesso99/sela/issues/19). CI job `db-tls-postgres` exercises require, verify-ca (hostname mismatch OK), verify-full hostname mismatch → `DbError::Tls`, and hostssl-only disable.
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

- Product rename from Birch to Sela (#1).
- UI split into focused workspace modules under ~400 lines (#7).
