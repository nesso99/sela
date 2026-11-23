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
| 5 | M1-3 | Filter and sort | US-04. UX review: [ux/02-filter-sort-review.md](./ux/02-filter-sort-review.md) | Backlog |
| 6 | M1-4 | Editor (highlight, statement-at-cursor, multi-statement) | US-08 minus autocomplete. UX: [ux/03-sql-editor.md](./ux/03-sql-editor.md) | Backlog |
| 7 | M1-5 | History | US-10 | Backlog |
| 8 | M1-7 | Selection, copy, themes | US-14 + light/dark | Backlog |
| 9 | M1-8 | TLS + config v2 | TLS modes; `schema_version` → 2 with **no migration code** (no users yet) | Backlog |
| 10 | M1-9 | Postgres text-cast fallback | Own dedicated PR | Backlog |

### M1-2a details

`ResultWindow` keeps at most 16 pages of 100 rows (raise the cap for a fully materialized result); Json stored raw and parsed lazily; cell display truncated to ~200 characters and cached; a decode error is never NULL. 100k×50 RSS delta went from +625 MiB / +11.6 GiB to about +30–76 MiB. See [ADR 0002](../adr/0002-bounded-result-window.md).

### M1-2b details

- Cancel: `CancellationToken` with Stop and Cmd+./Ctrl+.. Postgres `pg_cancel_backend` and MySQL `KILL QUERY` from a second connection; connection closed after cancel. SQLite only drops the stream (documented limitation). See [ADR 0003](../adr/0003-cancel-second-connection.md).
- Results stream in batches into `ResultWindow`.
- Table counts are estimates, with an exact-count button.
- Arbitrary SQL is not wrapped; streams up to a 1 000-row cap with load-more. Load-more re-runs only read-only SQL (`SHOW`, `EXPLAIN` without `ANALYZE`, `SELECT`/`VALUES`/`WITH` without DML); otherwise shows “Result capped at N rows”. See [ADR 0004](../adr/0004-safe-load-more.md).
- Known limitation: `SELECT my_func()` with side effects is still treated as safe.
- Warm time to first frame ≈ 40–43 ms; cold start to re-check on a real GPU.

### M1 backlog (follow-ups)

- Idle timeout for SQL streams paused at the cap
- Bound or LRU on the structure cache
- Re-measure on a real GPU
- Dedicated DB connection per query tab (shared pool today)

---

## M2 · Safe editing

| ID | Work | Done when |
| --- | --- | --- |
| M2-1 | `ChangeSet` + grid overlay (edit, insert, delete, duplicate) | US-05 on tables with PK |
| M2-2 | `ChangePlan` → dialect SQL + golden tests | Snapshots for 3 engines |
| M2-3 | Preview SQL, one-transaction commit, discard, rollback keeps changes | US-06 |
| M2-4 | Safe mode 3 levels, no-WHERE warnings (`sqlparser`) | US-07 |
| M2-5 | Connection color/label, groups, duplicate, import URL | US-01 complete |

---

## M3 · Connectivity and data

| ID | Work | Done when |
| --- | --- | --- |
| M3-1 | SSH tunnel (russh): password, key + passphrase, agent, `~/.ssh/config`, known_hosts | US-02; openssh container test |
| M3-2 | Schema + alias autocomplete | US-08 complete |
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
