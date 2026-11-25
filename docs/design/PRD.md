> Source of truth: this file.

# PRD — Sela

Open-source, native desktop database client (PostgreSQL, MySQL, SQLite) built with Rust + GPUI. TablePlus-style UX: connections, schema browse, grid, SQL editor — with keyboard-first workflows and safe defaults for production.

**Product name:** Sela (code name Birch is retired).  
**License:** GPL-3.0-or-later (`publish = false` in Cargo; see root `LICENSE`).  
**Engines:** PostgreSQL, MySQL, SQLite.  
**Stack:** Rust 1.85+, gpui-kit 0.10.0 (gpui-pre 0.3.12 + gpui-component), sqlx 0.9.1, tokio, keyring 3.

See also: [SPECS.md](./SPECS.md), [roadmap.md](./roadmap.md), [UX specs](./ux/README.md), [CONTRIBUTING.md](../../CONTRIBUTING.md), [benchmarks.md](../dev/benchmarks.md).

---

## Summary

Sela already ships a working slice: save connections, browse schemas/tables, tabs, page or stream results, cancel queries, and a read-only Structure view. The TablePlus differentiators (inline edit with pending changes, safe mode, SSH, rich SQL editor, filter/sort, export) are the path through the rest of M1–M3.

**Current foundation (M0 done, PRs #1–#8):** Cargo workspace (`sela-core`, `sela-drivers`, `sela-config`, `sela-app` binary `sela`), typed `Value`, classified `DbError`, CI, and a grid decision to **keep** gpui-component `DataTable` ([benchmarks.md](../dev/benchmarks.md)).

**M1 so far:** result model (#9), cancel/streaming (#10), Structure view (#11), and tabs (#12) are done. Remaining M1 items and M2–M4 live in [roadmap.md](./roadmap.md).

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
- Stay open source (GPL-3.0-or-later) and contribution-friendly

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
| Tabs (table and query), light/dark theme | v1 | Tabs: Done (M1-1, PR #12); themes: M1-7 |
| Filter bar, header sort, on-demand exact count | v1 | Done (M1-3) |
| Cell edit, insert/delete/duplicate, pending changes, preview SQL, commit/discard | v1 | Planned (M2) |
| Safe mode per connection, warn UPDATE/DELETE without WHERE, color label | v1 | Planned (M2) |
| SQL editor: highlight, autocomplete, statement-at-cursor, multi-statement. Format (`sqlformat`) is backlog, not M1-4 | v1 | Highlight and multi-statement: M1-4. Autocomplete: M3-2. Format: backlog |
| Query history with search, saved queries | v1 | Planned (M1-5) |
| Selection + copy TSV/CSV/JSON/INSERT; themes | v1 | Planned (M1-7) |
| TLS modes + CA cert; config schema_version 2 | v1 | Done (M1-8) |
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
| US-02 | I connect through an SSH bastion | Password, private key (+ passphrase), ssh-agent, `~/.ssh/config`; known_hosts checked; SSH errors distinct from DB errors | Core/drivers done (UI deferred) |
| US-03 | I open many tables and queries in tabs | Cmd+T new query, Cmd+W close, Cmd+1..9 switch; tab remembers its connection; dirty close prompts when pending edits exist | Done (M1-1); dirty prompt is M2 |
| US-04 | I filter and sort without writing SQL | Filter bar: column + op (`=`, `!=`, `LIKE`, `IN`, `IS NULL`, `>`, `<`) + value, AND combine; header click to sort; generated SQL visible | Done (M1-3) |
| US-05 | I edit data on the grid | Double-click or Enter to edit; edited cells highlighted; insert/delete/duplicate rows; Cmd+S commits in one transaction; Esc cancels the open cell editor only; Cmd+Shift+Z and Discard drop pending edits after confirm; no primary key → read-only with a reason | Not started |
| US-06 | I preview SQL before commit | Preview lists exact statements with bound values; commit failure rolls back and keeps pending changes | Not started |
| US-07 | As on-call, I do not accidentally break production | Safe mode Off / Confirm writes / Read-only; UPDATE/DELETE without WHERE always warns; connection color + name on toolbar | Not started |
| US-08 | I write SQL quickly | Syntax highlight; autocomplete keywords, schema, table, columns (after `alias.`); Cmd+Enter runs statement at cursor or selection; multi-statement file | Partial (editor + multi-run; no autocomplete / no SQL grammar yet) |
| US-09 | I cancel a long-running query | Stop button and Cmd+.; server receives cancel within ~1 s; UI never freezes | Done (M1-2b) |
| US-10 | I find a past query | History stores SQL, connection, time, rows, duration; full-text search; reopen in a new tab | Not started |
| US-11 | I inspect table structure | Structure tab: columns (type, nullable, default, comment), indexes, FKs, full DDL; Data/Structure shortcut | Done (M1-6) |
| US-12 | I export data | Export result or whole table as CSV, JSON, SQL INSERT; background with progress and cancel; not limited to current page | Not started |
| US-13 | I see correctly typed values | NULL ≠ empty string; NUMERIC keeps digits; UUID, JSON, timestamptz correct; BLOB short hex + full view | Mostly done |
| US-14 | I copy data | Multi-cell selection; copy as TSV, CSV, JSON, or INSERT | Done (M1-7; full-table export remains US-12) |

---

## Main flows

### Open a table and edit safely (target UX)

1. **Cmd+P → type table name** — quick open from the connection schema cache; Enter opens a Data tab.
2. **Filter** — Cmd+F opens the filter bar; column, operator, value; Enter applies; grid reloads page 1.
3. **Edit** — Enter on a cell; edited cells, new rows, deleted rows use distinct colors; status bar counts pending changes.
4. **Preview** — Cmd+Shift+P (or Preview) shows the SQL that will run.
5. **Commit** — Cmd+S. If safe mode is on, a confirm dialog lists statements; run in one transaction; on success clear pending and reload.

### Run a query and cancel (current + target)

1. **Cmd+T** opens a query tab bound to the selected connection.
2. **Cmd+Enter** runs the statement at the cursor; status shows Running and a timer.
3. **Cmd+.** / Stop cancels: `CancellationToken` + server cancel; grid keeps rows already received and records cancellation.
4. **Results** stream into `ResultWindow` in batches; status shows row count kind, elapsed time; errors include position when the engine provides it.

---

## Non-functional requirements

| Area | Target | How to measure |
| --- | --- | --- |
| Cold start to usable window | ≤ 500 ms on Apple Silicon Mac; ≤ 1 s on mid-range Linux/Windows | Automated open → first frame with sidebar |
| First data page paint | ≤ 50 ms from data ready to grid painted (~1 000 rows) | In-app timer, debug log |
| Grid scroll | 60 fps (120 on ProMotion) with 100 000 loaded rows × 50 cols | GPUI frame timing in profile builds; see [benchmarks.md](../dev/benchmarks.md) |
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

Milestone tables, M1 details, and post-v1 ideas live in **[roadmap.md](./roadmap.md)**. M0 is done; M1-1 (tabs), M1-2a/b, M1-6, and M1-8 (TLS + config v2, PR #15) are done or in review on `main`.

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
