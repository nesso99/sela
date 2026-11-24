> Source of truth: this file.

# Roadmap — Sela

Milestones M0–M4. Relative sequencing for independent PRs — not calendar estimates. Each item is an independent PR with tests. See [CONTRIBUTING.md](../../CONTRIBUTING.md).

Product intent lives in [PRD.md](./PRD.md). Technical detail lives in [SPECS.md](./SPECS.md).

---

## M0 · Foundation — **done** (PRs #1–#8)

| ID | Work | Done when | Status |
| --- | --- | --- | --- |
| M0-1 | Split Cargo workspace: core, drivers, config, app | `cargo test -p sela-core` without GPUI | Done (#2) |
| M0-2 | GitHub Actions: fmt, clippy, test macOS/Linux/(Windows); PG+MySQL service jobs | Red PR on failure; PG/MySQL tests not skipped when URLs set | Done (#4, #6) |
| M0-3 | Typed `Value` + decode NUMERIC, UUID, JSON, timestamptz, time | Round-trip tests on 3 engines | Done (#5) |
| M0-4 | No panic in `drive()`; classified `DbError` | Panicked task surfaces error; app keeps running | Done (#3) |
| M0-5 | Split `workspace.rs` into sidebar / tab / status modules | No UI file over ~400 lines | Done (#7) |
| M0-6 | Grid bench 100 000 × 50 with DataTable | fps + memory numbers; keep vs rewrite decision | Done (#8) — **keep DataTable**; see [benchmarks.md](../dev/benchmarks.md) |

Also: rename Birch → Sela (#1). License + cargo-deny (#14).

---

## M1 · Daily read-only use

| Order | ID | Work | Done when | Status |
| --- | --- | --- | --- |
| 1 | M1-2a | Result model and memory (`ResultWindow`) | Bounded pages, lazy Json, truncated display cache; decode errors never NULL | Done (#9) |
| 2 | M1-2b | Cancel and streaming | US-09; batches into `ResultWindow`; estimate counts; SQL stream cap + load-more | Done (#10) |
| 3 | M1-1 | Tabs | US-03 | Done (#12) |
| 4 | M1-6 | Structure view | US-11 on 3 engines | Done (#11) |
| 5 | M1-3 | Filter and sort | US-04. UX review: [ux/02-filter-sort-review.md](./ux/02-filter-sort-review.md) | Done (#17, #22) |
| 6 | M1-4 | Editor (highlight, statement-at-cursor, multi-statement) | US-08 minus autocomplete. UX: [ux/03-sql-editor.md](./ux/03-sql-editor.md) | Done (no tree-sitter SQL grammar; no autocomplete) |
| 7 | M1-5 | History | US-10 | Done (#25) |
| 8 | M1-7 | Selection, copy, themes | US-14 + light/dark; result-strip keyboard | Done (#24) |
| 9 | M1-8 | TLS + config v2 | TLS modes; `schema_version` → 2 with **no migration code**; Postgres TLS CI | Done (#15) |
| 10 | M1-9 | Postgres text-cast fallback | Own dedicated PR | Done (#13) |

### M1-2a details

`ResultWindow` keeps at most 16 pages of 100 rows (raise the cap for a fully materialized result); Json stored raw and parsed lazily; cell display truncated to ~200 characters and cached; a decode error is never NULL. 100k×50 RSS delta went from +625 MiB / +11.6 GiB to about +30–76 MiB. See [ADR 0002](../adr/0002-bounded-result-window.md).

### M1-2b details

- Cancel: `CancellationToken` with Stop and Cmd+./Ctrl+.. Postgres `pg_cancel_backend` and MySQL `KILL QUERY` from a second connection; connection closed after cancel. SQLite only drops the stream (documented limitation). See [ADR 0003](../adr/0003-cancel-second-connection.md).
- Results stream in batches into `ResultWindow`.
- Table counts are estimates, with an exact-count button.
- Arbitrary SQL is not wrapped; streams up to a 1 000-row cap with load-more. Load-more re-runs only read-only SQL (`SHOW`, `EXPLAIN` without `ANALYZE`, `SELECT`/`VALUES`/`WITH` without DML); otherwise shows “Result capped at N rows”. See [ADR 0004](../adr/0004-safe-load-more.md).
- Known limitation: `SELECT my_func()` with side effects is still treated as safe.
- Warm time to first frame ≈ 40–43 ms; cold start to re-check on a real GPU.

### M1-3 details

- Filter bar on table tabs only: column + operator + value, AND-combined; generated SQL with bound parameters; text menu offers `LIKE` / `NOT LIKE` with `ESCAPE '!'` (`%`/`_` wildcards; `!` escapes). Legacy convenience ops (contains / starts / ends) still deserialize from `tabs.json` and use ILIKE on Postgres.
- Header click cycles asc → desc → none via server-side `ORDER BY` on quoted identifiers. Shift-click adds up to 3 keys (▲1 / ▼2 marks). SQL result tabs have no header sort.
- Operators depend on column kind (`ColumnKind`). Apply validates every row; invalid Apply refuses the whole set. Empty text `=` binds `''`.
- Applying filter or sort re-queries from page 0 with a new request id (cancels in-flight work). Under a filter, row count is Unknown until Count runs `COUNT(*)` with the same WHERE (status bar; cancellable with elapsed tick).
- Filters/sorts persist on table tabs in `tabs.json` (serde defaults); restore never auto-runs beyond the normal table preview after reconnect.
- Shortcut: Cmd/Ctrl+F (`secondary-f`) toggles the filter bar. Enter / secondary-enter in a filter field Applies; in the editor secondary-enter Runs.
- User docs: [filtering.md](../user/filtering.md). UX acceptance: [ux/02-filter-sort-review.md](./ux/02-filter-sort-review.md).

### M1 backlog (follow-ups)

- Idle timeout for SQL streams paused at the cap
- Bound or LRU on the structure cache
- Re-measure on a real GPU
- Dedicated DB connection per query tab (shared pool today)
- Optional OR combine / raw-WHERE mode for filters
- Remaining Postgres `<TYPE>` placeholders (tsquery, polygon, composites, …)
- TLS client-key passphrase. rustls needs an unencrypted PEM today. A passphrase, if added later, goes to the OS keychain and never into `connections.json`.
- M2 follow-up: edit a table that has no primary key but has a unique NOT NULL key. v0.2 is primary-key only (`ChangeError::NoPrimaryKey`, #65). UX: [ux/07-grid-editing.md](./ux/07-grid-editing.md).

---

### M1-7 details

- Grid selection via gpui-component `DataTable` (`cell_selectable`): cell / row / column, plus Shift-click range tracked on `GridDelegate`.
- Clipboard formats in `sela-core` (`CopyFormat` + typed `CopyCell`: TSV default on `secondary-c`, CSV, JSON, INSERT). Context menu on the grid. Unloaded rows and decode cells refuse copy/INSERT with a status message; MySQL/SQLite also refuse INSERT for non-finite floats (Postgres emits `'NaN'` / `'Infinity'` / `'-Infinity'`). Unit tests cover NULL-vs-text-`NULL`, MySQL backslash escaping, non-finite numbers, and selection math.
- Light and dark `ThemeConfig`s from [ux/06-theme-tokens.md](./ux/06-theme-tokens.md); preference in `{config_dir}/settings.json` (`light` / `dark` / `system`). System follows `Window::appearance` / `Theme::sync_system_appearance`.
- Result strip: `ResultStrip` key context, tab stop, Left/Right for Prev/Next (roadmap backlog item closed here).
- User docs: [copying.md](../user/copying.md), [themes.md](../user/themes.md), [keyboard-shortcuts.md](../user/keyboard-shortcuts.md).

---

## M2 · Safe editing

| ID | Work | Done when | Status |
| --- | --- | --- | --- |
| M2-1 | `ChangeSet` + grid overlay (edit, insert, delete, duplicate) | US-05 on tables with PK | Done (core + sela-app overlay UI) |
| M2-2 | `ChangePlan` → dialect SQL + golden tests | Snapshots for 3 engines | Done (core + `apply_changes` + golden / integration tests) |
| M2-3 | Preview SQL, one-transaction commit, discard, rollback keeps changes | US-06 | Done (core/drivers + pending bar, Preview sheet, Commit/Discard UI) |
| M2-4 | Safe mode 3 levels, no-WHERE warnings (`sqlparser`) | US-07 | Done (config + classifier + editor/history/grid Commit gating; toolbar) |
| M2-5 | Connection color/label, groups, duplicate, import URL | US-01 complete | |

### M2-1 / M2-2 details (core)

- `sela-core::ChangeSet` records pending cell updates, inserts, and deletes for one table. Tables without a primary key are rejected (`ChangeError::NoPrimaryKey`). Merge rules: edit-on-insert updates the insert; delete-of-insert removes it; re-edit back to the original value drops the cell update.
- `sela-core::ChangePlan::build` emits ordered DELETE → UPDATE → INSERT SQL with dialect quoting and bind placeholders (`$n` / `?`). Values are never interpolated into executable SQL. `preview_sql` / `preview_bound` are for the M2-3 dialog.
- `DatabaseSession::apply_changes` runs a plan in one transaction (commit on success, rollback on error) on Postgres, MySQL, and SQLite. UPDATE/DELETE must affect exactly one row (`DbError::ChangeConflict` otherwise).

### M2-4 details

- `SavedConnection.safe_mode`: `off` / `confirm` / `read-only` (serde default Off; omit on save when Off; no `schema_version` bump).
- `classify_statement_safety` + `decide_editor_gate` / `decide_grid_commit_gate` / `decide_history_rerun` in `sela-core` (`sqlparser`, dialect-matched). Unparseable SQL fails closed. No-WHERE warns on `UPDATE`/`DELETE` without a `WHERE` (including CTE bodies); `WHERE 1=1` does not. `EXPLAIN (ANALYZE …)`, `SELECT … INTO`, and `FOR UPDATE`/`SHARE` are writes.
- App: connection dialog Safe mode select, toolbar `Safe: …` segment, editor / history Rerun / grid Commit gating with quit-style confirms (Cancel primary). Read-only disables Commit and shows the grid strip. Drivers apply server-side Read-only on connect; saving reconnects.

---

## M3 · Connectivity and data

| ID | Work | Done when |
| --- | --- | --- |
| M3-1 | SSH tunnel (russh): password, key + passphrase, agent, `~/.ssh/config`, known_hosts | US-02; openssh container test |
| M3-2 | Schema + alias autocomplete; optional MIT tree-sitter SQL grammar for muted keyword/comment highlight | US-08 complete |
| M3-3 | Export CSV/JSON/SQL with background progress; CSV import | US-12 |
| M3-4 | Command palette + quick open table | Every primary action reachable from palette |

---

## M4 · Ship v1.0

| ID | Work | Done when |
| --- | --- | --- |
| M4-1 | Signed packages for 3 OS in CI | Installable from GitHub Releases |
| M4-2 | Hit NFRs: start, fps, memory; publish benchmark report | Numbers in release notes |
| M4-3 | Naming polish (bundle id, docs); keychain already `app.sela.database` | Branding consistent; no Birch leftovers |
| M4-4 | User docs, shortcuts page, CONTRIBUTING | New contributor builds in ~15 minutes |

After v1: structure editing (DDL with preview), MariaDB (near MySQL driver), DuckDB, SQL Server, ClickHouse, Redis, ER diagram, AI/MCP.
