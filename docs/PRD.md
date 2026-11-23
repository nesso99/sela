> Source of truth: this file.

# PRD — Sela

Open-source, native desktop database client (PostgreSQL, MySQL, SQLite) built with Rust + GPUI. TablePlus-style UX: connections, schema browse, grid, SQL editor — with keyboard-first workflows and safe defaults for production.

**Product name:** Sela (code name Birch is retired).  
**License:** GPL-3.0-or-later (`publish = false` in Cargo; see root `LICENSE`).  
**Engines:** PostgreSQL, MySQL, SQLite.  
**Stack:** Rust 1.85+, gpui-kit 0.10.0 (gpui-pre 0.3.12 + gpui-component), sqlx 0.8, tokio, keyring 3.

See also: [SPECS.md](./SPECS.md), [CONTRIBUTING.md](./CONTRIBUTING.md), [benchmarks/grid.md](./benchmarks/grid.md).

---

## Summary

Sela already ships a working slice: save connections, browse schemas/tables, page or stream results, cancel queries, and a read-only Structure view. The TablePlus differentiators (inline edit with pending changes, safe mode, SSH, rich SQL editor, tabs, filter/sort, export) are the path through M1–M3.

**Current foundation (M0 done, PRs #1–#8):** Cargo workspace (`sela-core`, `sela-drivers`, `sela-config`, `sela-app` binary `sela`), typed `Value`, classified `DbError`, CI, and a grid decision to **keep** gpui-component `DataTable` ([benchmarks/grid.md](./benchmarks/grid.md)).

**M1 in progress:** result model + cancel/streaming done; Structure view done; tabs next (see [Milestones](#milestones-m0m4)).

---

## Problem and vision

Developers work with databases every day and need a tool that opens instantly, shows and edits data safely, and makes SQL fast. Current options force trade-offs: TablePlus is fast and polished but closed-source and paid per device; DBeaver and DataGrip are powerful but heavy (JVM); Beekeeper Studio is friendly but Electron.

**Vision:** An open-source database client with GPU rendering, a native feel on macOS, Linux, and Windows, keyboard-first workflows, and production-safe defaults: every write is previewed as SQL before commit.

---

## Users / personas

| Persona | Context | Needs most |
| --- | --- | --- |
| Minh, backend dev | Postgres/MySQL daily, macOS or Linux, many env (dev/staging/prod) | Fast open, tabs, filter, edit a few rows, copy results |
| Lan, full-stack / indie | SQLite and local Postgres via Docker, mid-range laptop | Light app, free, CSV import/export |
| Tuấn, on-call / ops | Production DB through SSH bastion | Safe mode, read-only, prod color label, cancel long queries |
| Linux developer | Few good native clients on Linux | First-class Linux (Wayland, keyring) |

---

## Goals and non-goals

### Goals

- Replace TablePlus for day-to-day work with PG, MySQL, and SQLite
- Start and return results faster than JVM/Electron clients
- Safe editing: pending changes, SQL preview, safe mode
- Keyboard first: every primary action has a shortcut and command palette
- Ship on macOS, Linux, and Windows from one codebase
- Stay open source (MIT) and contribution-friendly

### Non-goals (v1)

- 20+ engines like TablePlus
- NoSQL (Redis, MongoDB) in v1
- ER diagram, DB sync, scheduled backup/restore
- AI assistant / MCP server in v1
- Cloud sync of connections, team features

---

## Competitors

| Tool | Platform / price | Strengths | Weak vs Sela |
| --- | --- | --- | --- |
| TablePlus | Native, closed-source; Basic ~$99 one-time/device | 23 databases, code review + safe mode, native SSH, fast | Closed source, paid per device |
| DBeaver | Java/Eclipse; Community free, paid from ~$113/yr | Broadest engine coverage (JDBC), ER diagrams | Heavy, slow start, busy UI |
| DataGrip | JetBrains JVM; free for non-commercial, ~$109/yr personal | Best SQL intelligence / refactor | Heavy, more IDE than fast data browse |
| Beekeeper Studio | Electron; Community free, paid from ~$9/mo | Easy, cross-platform | RAM cost, large grids less smooth |
| Other GPUI clients | Tusk, DBDelve, Cellar, Tables, Koyo (search hits; not audited in depth) | Same GPUI speed advantage | Same niche — differentiate on UX quality and safety |

**Differentiation:** TablePlus-like UX (pending changes, safe mode, shortcuts) that is open source, free, Linux-first-class, with a GPU grid that stays smooth at hundreds of thousands of rows.

---

## Feature scope

| Feature | Stage | Status |
| --- | --- | --- |
| Connection CRUD (PG, MySQL, SQLite), password in keychain | MVP | Done |
| Sidebar schema / table / view | MVP | Done |
| Virtualized grid + paging, typed headers | MVP | Done |
| Run SQL, show errors / row counts / timing | MVP | Done |
| Correct NUMERIC, UUID, JSON, timestamptz display | MVP | Done (typed `Value`) |
| Cancel query (server-side) | MVP | Done (M1-2b) |
| Streaming results + estimated table counts | MVP | Done (M1-2b) |
| Bounded result window / memory | MVP | Done (M1-2a) |
| Structure view (columns, indexes, FK, DDL) read-only | v1 | Done (M1-6, PR #11) |
| Tabs (table and query), light/dark theme | v1 | Tabs: planned M1-1 (PR #12); themes: M1-7 |
| Filter bar, header sort, on-demand exact count | v1 | Planned (M1-3) |
| Cell edit, insert/delete/duplicate, pending changes, preview SQL, commit/discard | v1 | Planned (M2) |
| Safe mode per connection, warn UPDATE/DELETE without WHERE, color label | v1 | Planned (M2) |
| SQL editor: highlight, autocomplete, statement-at-cursor, multi-statement, format | v1 | Planned (M1-4 / M3) |
| Query history with search, saved queries | v1 | Planned (M1-5) |
| Selection + copy TSV/CSV/JSON/INSERT; themes | v1 | Planned (M1-7) |
| TLS modes + CA cert; config schema_version 2 | v1 | Planned (M1-8) |
| Postgres text-cast fallback | v1 | Planned (M1-9, own PR) |
| SSH tunnel (password, key, agent, `~/.ssh/config`), SSL certs, connection groups | v1 | Planned (M3) |
| Export CSV/JSON/SQL INSERT, import CSV | v1 | Planned (M3) |
| Command palette, quick open table (Cmd+P) | v1 | Planned (M3) |
| Signed packages for 3 OS | v1 | Planned (M4) |
| Structure editing (DDL preview), MariaDB, DuckDB, SQL Server, ClickHouse, Redis, ER, AI | Post-v1 | Later |

---

## User stories (US-xx)

| ID | Story | Acceptance criteria | Status |
| --- | --- | --- | --- |
| US-01 | As a developer, I save a connection and reopen it with one click | Create/edit/delete/duplicate; Test connection; password never in config file; survives app restart | Mostly done |
| US-02 | I connect through an SSH bastion | Password, private key (+ passphrase), ssh-agent, `~/.ssh/config`; known_hosts checked; SSH errors distinct from DB errors | Not started |
| US-03 | I open many tables and queries in tabs | Cmd+T new query, Cmd+W close, Cmd+1..9 switch; tab remembers its connection; dirty close prompts when pending edits exist | Not on main (M1-1 / PR #12) |
| US-04 | I filter and sort without writing SQL | Filter bar: column + op (`=`, `!=`, `LIKE`, `IN`, `IS NULL`, `>`, `<`) + value, AND combine; header click to sort; generated SQL visible | Not started |
| US-05 | I edit data on the grid | Double-click or Enter to edit; edited cells highlighted; insert/delete/duplicate rows; Cmd+S commits in one transaction; Esc / Cmd+Shift+Z discard; no PK → read-only with reason | Not started |
| US-06 | I preview SQL before commit | Preview lists exact statements with bound values; commit failure rolls back and keeps pending changes | Not started |
| US-07 | As on-call, I do not accidentally break production | Safe mode Off / Confirm writes / Read-only; UPDATE/DELETE without WHERE always warns; connection color + name on toolbar | Not started |
| US-08 | I write SQL quickly | Syntax highlight; autocomplete keywords, schema, table, columns (after `alias.`); Cmd+Enter runs statement at cursor or selection; multi-statement file | Partial (single statement Textarea) |
| US-09 | I cancel a long-running query | Stop button and Cmd+.; server receives cancel within ~1 s; UI never freezes | Done (M1-2b) |
| US-10 | I find a past query | History stores SQL, connection, time, rows, duration; full-text search; reopen in a new tab | Not started |
| US-11 | I inspect table structure | Structure tab: columns (type, nullable, default, comment), indexes, FKs, full DDL; Data/Structure shortcut | Done (M1-6) |
| US-12 | I export data | Export result or whole table as CSV, JSON, SQL INSERT; background with progress and cancel; not limited to current page | Not started |
| US-13 | I see correctly typed values | NULL ≠ empty string; NUMERIC keeps digits; UUID, JSON, timestamptz correct; BLOB short hex + full view | Mostly done |
| US-14 | I copy data | Multi-cell selection; copy as TSV, CSV, JSON, or INSERT | Not started |

---

## Main flows

### Open a table and edit safely (target UX)

1. **Cmd+P → type table name** — quick open from the connection schema cache; Enter opens a Data tab.
2. **Filter** — Cmd+F opens the filter bar; column, operator, value; Enter applies; grid reloads page 1.
3. **Edit** — Enter on a cell; edited cells, new rows, deleted rows use distinct colors; status bar counts pending changes.
4. **Preview** — Cmd+Shift+P (or Preview) shows the SQL that will run.
5. **Commit** — Cmd+S. If safe mode is on, a confirm dialog lists statements; run in one transaction; on success clear pending and reload.

### Run a query and cancel (current + target)

1. **Cmd+T** opens a query tab bound to the selected connection (tabs: M1-1).
2. **Cmd+Enter** runs the statement at the cursor; status shows Running and a timer.
3. **Cmd+.** / Stop cancels: `CancellationToken` + server cancel; grid keeps rows already received and records cancellation.
4. **Results** stream into `ResultWindow` in batches; status shows row count kind, elapsed time; errors include position when the engine provides it.

---

## Non-functional requirements

| Area | Target | How to measure |
| --- | --- | --- |
| Cold start to usable window | ≤ 500 ms on Apple Silicon Mac; ≤ 1 s on mid-range Linux/Windows | Automated open → first frame with sidebar |
| First data page paint | ≤ 50 ms from data ready to grid painted (~1 000 rows) | In-app timer, debug log |
| Grid scroll | 60 fps (120 on ProMotion) with 100 000 loaded rows × 50 cols | GPUI frame timing in profile builds; see [benchmarks/grid.md](./benchmarks/grid.md) |
| Memory | ≤ 150 MB idle with 3 connections; in-RAM results bounded (default window 16×100 pages; SQL cap 1 000 + load-more) | RSS on 3 OS |
| UI never blocks | No DB work on UI thread; any task > 100 ms has indicator and is cancellable | Code review + tests |
| Credential security | DB/SSH passwords and key passphrases only in OS keychain; never logged; delete connection deletes secret | Tests + log grep |
| Linux without keyring | Keep current behavior (RAM only + status bar); optional ask-every-time | `MemorySecretStore` tests |
| TLS | disable / prefer / require / verify-ca / verify-full, CA and client cert | Integration tests with TLS containers |
| Platforms | macOS 13+ (Metal), Linux x86_64/arm64 Wayland + X11 (Vulkan), Windows 10+ | CI builds; manual smoke each release |
| Stability | ≥ 99.5% sessions without crash; panic in DB task does not kill the app | Opt-in crash reports; `drive()` maps panic → `DbError::Internal` |
| Size | Installer ≤ 40 MB per OS | CI artifact size |

**Measured (lab, lavapipe/Xvfb):** warm time-to-first-frame ≈ 40–43 ms after M1-2b; 100k×50 RSS delta ≈ +30–76 MiB after M1-2a (was +625 MiB / +11.6 GiB). Cold start and fps must be re-checked on a real GPU.

---

## Success metrics

- **Dogfood:** Core team uses Sela instead of TablePlus at least 4 days/week after v1.
- **Performance:** All NFR targets on 3 OS with numbers in CI / release notes.
- **Quality:** No data loss from bad commits; zero credential-leak security bugs.
- **Community:** 1 000 GitHub stars and 30 external contributors within 6 months of open v1 (aspirational).
- **Retention (opt-in telemetry only):** 7-day return rate ≥ 40%.

---

## Milestones M0–M4

Estimates are relative sequencing for independent PRs — not calendar weeks. Each item is an independent PR with tests. See [CONTRIBUTING.md](./CONTRIBUTING.md).

### M0 · Foundation — **done** (PRs #1–#8)

| ID | Work | Done when | Status |
| --- | --- | --- | --- |
| M0-1 | Split Cargo workspace: core, drivers, config, app | `cargo test -p sela-core` without GPUI | Done (#2) |
| M0-2 | GitHub Actions: fmt, clippy, test macOS/Linux/(Windows); PG+MySQL service jobs | Red PR on failure; PG/MySQL tests not skipped when URLs set | Done (#4, #6) |
| M0-3 | Typed `Value` + decode NUMERIC, UUID, JSON, timestamptz, time | Round-trip tests on 3 engines | Done (#5) |
| M0-4 | No panic in `drive()`; classified `DbError` | Panicked task surfaces error; app keeps running | Done (#3) |
| M0-5 | Split `workspace.rs` into sidebar / tab / status modules | No UI file over ~400 lines | Done (#7) |
| M0-6 | Grid bench 100 000 × 50 with DataTable | fps + memory numbers; keep vs rewrite decision | Done (#8) — **keep DataTable**; see [benchmarks/grid.md](./benchmarks/grid.md) |

Also: rename Birch → Sela (#1).

### M1 · Daily read-only use — **approved order**

| Order | ID | Work | Done when | Status |
| --- | --- | --- | --- |
| 1 | M1-2a | Result model and memory (`ResultWindow`) | Bounded pages, lazy Json, truncated display cache; decode errors never NULL | Done (#9) |
| 2 | M1-2b | Cancel and streaming | US-09; batches into `ResultWindow`; estimate counts; SQL stream cap + load-more | Done (#10) |
| 3 | M1-1 | Tabs | US-03 | Next (PR #12 open) |
| 4 | M1-6 | Structure view | US-11 on 3 engines | Done (#11) |
| 5 | M1-3 | Filter and sort | US-04 | Backlog |
| 6 | M1-4 | Editor (highlight, statement-at-cursor, multi-statement) | US-08 minus autocomplete | Backlog |
| 7 | M1-5 | History | US-10 | Backlog |
| 8 | M1-7 | Selection, copy, themes | US-14 + light/dark | Backlog |
| 9 | M1-8 | TLS + config v2 | TLS modes; `schema_version` → 2 with **no migration code** (no users yet) | Backlog |
| 10 | M1-9 | Postgres text-cast fallback | Own dedicated PR | Backlog |

**M1-2a details:** `ResultWindow` keeps at most 16 pages of 100 rows (raise the cap for a fully materialized result); Json stored raw and parsed lazily; cell display truncated to ~200 characters and cached; a decode error is never NULL. 100k×50 RSS delta went from +625 MiB / +11.6 GiB to about +30–76 MiB.

**M1-2b details:**

- Cancel: `CancellationToken` with Stop and Cmd+.. Postgres `pg_cancel_backend` and MySQL `KILL QUERY` from a second connection; connection closed after cancel. SQLite only drops the stream (documented limitation).
- Results stream in batches into `ResultWindow`.
- Table counts are estimates, with an exact-count button.
- Arbitrary SQL is not wrapped; streams up to a 1 000-row cap with load-more. Load-more re-runs only read-only SQL (`SHOW`, `EXPLAIN` without `ANALYZE`, `SELECT`/`VALUES`/`WITH` without DML); otherwise shows “Result capped at N rows”.
- Known limitation: `SELECT my_func()` with side effects is still treated as safe.
- Warm time to first frame ≈ 40–43 ms; cold start to re-check on a real GPU.

**M1 backlog (follow-ups):**

- Idle timeout for SQL streams paused at the cap
- Bound or LRU on the structure cache
- Re-measure on a real GPU

### M2 · Safe editing

| ID | Work | Done when |
| --- | --- | --- |
| M2-1 | `ChangeSet` + grid overlay (edit, insert, delete, duplicate) | US-05 on tables with PK |
| M2-2 | `ChangePlan` → dialect SQL + golden tests | Snapshots for 3 engines |
| M2-3 | Preview SQL, one-transaction commit, discard, rollback keeps changes | US-06 |
| M2-4 | Safe mode 3 levels, no-WHERE warnings (`sqlparser`) | US-07 |
| M2-5 | Connection color/label, groups, duplicate, import URL | US-01 complete |

### M3 · Connectivity and data

| ID | Work | Done when |
| --- | --- | --- |
| M3-1 | SSH tunnel (russh): password, key + passphrase, agent, `~/.ssh/config`, known_hosts | US-02; openssh container test |
| M3-2 | Schema + alias autocomplete | US-08 complete |
| M3-3 | Export CSV/JSON/SQL with background progress; CSV import | US-12 |
| M3-4 | Command palette + quick open table | Every primary action reachable from palette |

### M4 · Ship v1.0

| ID | Work | Done when |
| --- | --- | --- |
| M4-1 | Signed packages for 3 OS in CI | Installable from GitHub Releases |
| M4-2 | Hit NFRs: start, fps, memory; publish benchmark report | Numbers in release notes |
| M4-3 | Naming polish (bundle id, docs); keychain already `app.sela.database` | Branding consistent; no Birch leftovers |
| M4-4 | User docs, shortcuts page, CONTRIBUTING | New contributor builds in ~15 minutes |

After v1: structure editing (DDL with preview), MariaDB (near MySQL driver), DuckDB, SQL Server, ClickHouse, Redis, ER diagram, AI/MCP.

---

## Risks and open questions

| Risk | Impact | Mitigation |
| --- | --- | --- |
| GPUI not 1.0; API churn; pin gpui-kit 0.10.0 → gpui-pre 0.3.12 | Costly upgrades; one upstream | Pin hard; isolate GPUI behind `ui/`; quarterly upgrade window |
| GPUI code editor / text input still immature | Autocomplete, Vietnamese IME may fail | Spike early in M1; test Telex on 3 OS |
| Limited GPUI accessibility | Hard to meet a11y bars | Document limits; track upstream |
| Windows / macOS less exercised than Linux | Release surprises | CI on 3 OS from M0; smoke each release |
| sqlx lacks a standard cancel/interrupt API | SQLite cancel weak | Second-connection cancel for PG/MySQL; document SQLite stream-drop |
| Many similar GPUI clients | Hard to stand out | Focus on safety + keyboard UX + Linux |
| Chasing engine count | Dilutes quality | Stay at 3 engines through v1 |

**Open questions**

1. Prefer which OS for the first polished release: macOS (TablePlus peer) or Linux (few native peers)?
2. Fourth engine after v1: MariaDB, DuckDB, or SQL Server?
3. Opt-in telemetry for usage metrics?
4. AI assistant in v1.x? Local model vs API?

---

## Naming

**Decision: product name is Sela.** Code name Birch is retired. Crates: `sela-core`, `sela-drivers`, `sela-config`, `sela-app` (binary `sela`). Keychain service: `app.sela.database`. Config: `~/.config/sela/connections.json` (macOS `~/Library/Application Support/Sela/`, Windows `%APPDATA%\Sela\`), override `SELA_CONFIG_DIR`.

Round-2 naming notes (historical): short, easy for Vietnamese and English speakers, suggest speed/clarity/data/query; avoid wood themes and TablePlus-like names. Sela (from SELECT) won over Rowza and Quilo on availability (crates.io / domains) and memorability. Rejected or deferred: Kuery (existing GPUI DB client), Clarq, Fasq, Qeri, Tia, Selo, and wood-theme round-1 names (Teak, Ashgrid, …).
