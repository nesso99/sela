# Version roadmap

How **releases** are planned. Milestone detail stays in [roadmap.md](./roadmap.md); this file is the **semver train**: each version ships one **coherent theme**, not a single ticket at a time.

## Rules

| Rule | Meaning |
| --- | --- |
| **One theme per version** | Scope is a named group of issues with shared acceptance and one RC QA pass. |
| **Cut once** | Version bump and the CHANGELOG section land in a **release PR** after that version’s feature work is on `main`. |
| **At most 3 open PRs** | Finish the current version before opening the next ([AGENTS.md](../../AGENTS.md)). |
| **Backlog → version** | New ideas go to GitHub issues. PM assigns them to a version section here before implementation starts. |
| **Tag** | After green `main`, Fedora RC QA, and QA + PM sign-off ([CONTRIBUTING.md](../../CONTRIBUTING.md)). |
| **One version ahead** | Do not start the version after the one currently in flight unless the owner asks. |

## Shipped

| Version | Theme | What landed |
| --- | --- | --- |
| **v0.3.0** | M3 connectivity and data | Autocomplete, SSH, export/import, command palette, optimistic WHERE. See [CHANGELOG](../../CHANGELOG.md). |
| **v0.3.1** | Post-M3 patch and QA ops | Column auto-fit (#118), column width (#137), connection-dialog and query-tab fixes (#140, #141), Fedora QA scripts and local-first policy (#143–#146). Tag on [#148](https://github.com/nesso99/sela/pull/148). |
| **v0.3.2** | Window and menu chrome | File menu removed from the CSD title bar and the SSD menu row. Quit stays on Ctrl+Q and close (#147, [#150](https://github.com/nesso99/sela/pull/150)). |
| **v0.4.0** | Grid editing batch 1 | While the cell editor is open, Duplicate and Insert do nothing. An empty insert with a generated primary key commits. The text `NULL` is not painted as SQL NULL ([#152](https://github.com/nesso99/sela/pull/152), [#72](https://github.com/nesso99/sela/issues/72)). |
| **v0.5.0** | Grid editing batch 2 | Pending-edit paint keys use `pk_match`. Restored views stay views. `BIGINT UNSIGNED` above `i64::MAX` round-trips ([#154](https://github.com/nesso99/sela/pull/154), [#72](https://github.com/nesso99/sela/issues/72)). |

## Planned

Nothing here is in progress. The next version starts only when the owner asks.

### v0.6.0 · Long-value cell editor

**Goal:** Edit multi-line text and JSON in the grid instead of sending the user to SQL.

| Work | Done when |
| --- | --- |
| [#72](https://github.com/nesso99/sela/issues/72) popover editor | Multi-line text and JSON open a popover editor. Those cells are no longer stuck on `Use SQL to change it.` Headless UI tests cover open, accept, and cancel. |

**Not in v0.6.0:** CSV import scale, packaging.

### v0.7.0 · Large CSV import

**Goal:** Import a large CSV without holding the whole change plan in memory.

| Work | Done when |
| --- | --- |
| IM-07 streaming sink ([v0.3 test plan](../qa/v0.3-test-plan.md)) | Import streams rows into `apply_changes` (or a successor sink). Progress, cancel, and rollback match the existing import rules. Memory stays bounded for a large file. |
| Create-table import | Stays backlog unless the owner pulls it into this version. |

### v0.8.0 · Row identity without a primary key

**Goal:** Edit a table that has no primary key but has a unique NOT NULL key.

| Work | Done when |
| --- | --- |
| Unique NOT NULL row key (roadmap backlog, #65 follow-up) | Commit and optimistic WHERE use that key. A table with neither a primary key nor such a key stays read-only. Tests cover both cases. |

### v1.0.0 · Ship (M4)

**Goal:** Installable, measured, documented release. Detail in [roadmap.md](./roadmap.md#m4--ship-v10).

| ID | Work |
| --- | --- |
| M4-1 | Signed packages for three OS in CI, installable from GitHub Releases |
| M4-2 | Start, fps, and memory numbers published with the release |
| M4-3 | Naming and branding polish; no Birch leftovers |
| M4-4 | User docs, shortcuts, and a contributor path that builds in about 15 minutes |

### After v1.0

Structure editing (DDL with preview), MariaDB, DuckDB, SQL Server, ClickHouse, Redis, an ER diagram, and AI/MCP. Each becomes its own **v1.x** theme when requested.

## How to use this file

1. **PM** assigns issues to a version section before handing work to engineering.
2. **Engineering** implements that version, then the release-cut PR bumps the version and CHANGELOG.
3. One Orca item per version. QA and Code Review run on that version’s PRs.

Last updated: 2026-11-27.
