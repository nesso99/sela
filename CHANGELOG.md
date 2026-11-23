# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once a stable release is tagged. Until then, work lands on `main` under **Unreleased**.

## [Unreleased]

### Added

- UX specs for the main window, a review of filter/sort, and the SQL editor (`docs/design/ux/`).
- Cargo workspace with `sela-core`, `sela-drivers`, `sela-config`, and `sela-app` (binary `sela`) (#2).
- Drivers for PostgreSQL, MySQL, and SQLite via sqlx; typed `Value` decode for NUMERIC, UUID, JSON, timestamptz, and related types (#3, #5).
- Saved connections with passwords in the OS keychain (`keyring`); never written to `connections.json`.
- Virtualized results grid (gpui-component `DataTable`) with paging and typed column headers (#8).
- Bounded `ResultWindow` (16 pages × 100 rows), lazy Json storage, and truncated cell display (#9).
- Query cancel (Stop / Cmd/Ctrl+.) with server-side cancel on Postgres and MySQL; batched streaming and load-more for arbitrary SQL up to a 1 000-row cap (#10).
- Read-only Structure view: columns, indexes, foreign keys, and DDL for all three engines (#11).
- Tabs for tables and queries: open/close/switch (Cmd/Ctrl+T/W/1..9), per-tab cancel-on-close, restore from `tabs.json` without auto-running SQL (#12).
- GPL-3.0-or-later license, `deny.toml`, and CI `cargo-deny` checks (#14).
- GitHub Actions CI: fmt, clippy, tests (Linux/macOS; Windows on main), and Postgres/MySQL integration (#4, #6).

### Changed

- Product rename from Birch to Sela (#1).
- UI split into focused workspace modules under ~400 lines (#7).
