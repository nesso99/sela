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
| **v0.6.0** | Long-value cell editor | Multi-line text and JSON cells open a popover editor ([#72](https://github.com/nesso99/sela/issues/72)). |
| **v0.7.0** | Large CSV import | Import streams rows through one bounded, cancellable, transactional sink ([#162](https://github.com/nesso99/sela/pull/162)). |
| **v0.8.0** | Row identity without a primary key | A unique NOT NULL key is the row identity for edit and optimistic WHERE; partial and expression indexes are never used ([#164](https://github.com/nesso99/sela/pull/164)). |

## Planned

Owner stop line: **v1.0.0** is in flight (item 70). Everything after it below is a **plan**, not a commitment: the next version starts only when the owner asks. Order is by value and risk; each version is one theme.

### v1.0.0 · Ship (M4)

**Goal:** Installable, measured, documented release. Detail in [roadmap.md](./roadmap.md#m4--ship-v10).

| ID | Work |
| --- | --- |
| M4-1 | Packages for three OS built in CI and attached to GitHub Releases with `SHA256SUMS`. **Unsigned** (owner decision, 2026-11-27); installation docs say so |
| M4-2 | Start, fps, and memory numbers published with the release |
| M4-3 | Naming and branding polish; no Birch leftovers |
| M4-4 | User docs, shortcuts, and a contributor path that builds in about 15 minutes |

### v1.0.1 · UI polish

**Goal:** Fix the three open UI backlog bugs.

| Work | Done when |
| --- | --- |
| [#157](https://github.com/nesso99/sela/issues/157) Postgres connection error below the fold | The error shows in the visible part of the connection dialog. |
| [#158](https://github.com/nesso99/sela/issues/158) Data / Structure toolbar jumps | One stable width and position when switching. |
| [#161](https://github.com/nesso99/sela/issues/161) Active tab not prominent | Active tab is clearly distinct in light and dark themes. |

### v1.1.0 · SQL editor quality and import follow-ups

**Goal:** Close the editor and import gaps left from M1 and M3.

| Work | Done when |
| --- | --- |
| SQL format (`sqlformat`), bound to `secondary-shift-f` (reserved in the shortcuts page) | Format selection or buffer; undo restores; tests per dialect. |
| tree-sitter SQL highlight (deferred in M3-2) | Muted keyword and comment colors; no regression in typing latency (`grid_bench` / start numbers). |
| Create-table import (v0.7 backlog) | Import into a new table from CSV with typed columns; rollback on failure; safe-mode gates apply. |

### v1.2.0 · Distribution

**Goal:** Make installing and updating Sela easy.

| Work | Done when |
| --- | --- |
| Automatic update check from GitHub Releases, user confirms ([SPECS](./SPECS.md)) | Opt-in prompt; checksum verified; no silent install. |
| Linux `.deb`, then Flatpak; macOS universal (Intel + Apple Silicon) | Installable from Releases; Vulkan deps documented. |
| Revisit code signing for macOS and Windows | Only if the owner provides certificates; otherwise stays unsigned. |

### v1.3.0 · Structure editing (columns)

**Goal:** Change a table's columns from the UI, with a preview first.

| Work | Done when |
| --- | --- |
| Add, rename, drop, change type and nullability of a column (DDL with preview) | Preview SQL dialog like data commit; safe mode gates it; Postgres, MySQL, SQLite limits documented (SQLite alter limits); tests per engine. |

### v1.4.0 · Structure editing (indexes, keys, tables)

**Goal:** Finish structure editing.

| Work | Done when |
| --- | --- |
| Create or drop index, unique constraint, foreign key; create and drop table | Same preview and safe-mode rules; tests per engine. |

### v1.5.0 · Fourth engine

**Goal:** Add one more database. Recommendation: **MariaDB** first, because it is closest to the MySQL driver. DuckDB and SQL Server stay candidates.

| Work | Done when |
| --- | --- |
| MariaDB driver, connection dialog entry, sample and QA plan | Same feature set as MySQL; integration tests in CI. |

### Later (no version yet)

ER diagram, SQL Server, DuckDB, ClickHouse, Redis, AI / MCP assistant. Each becomes its own **v1.x** theme when the owner pulls it in.

## Decisions for the owner

| Question | Why it matters | Default if no answer |
| --- | --- | --- |
| Which fourth engine? | Shapes v1.5.0. | MariaDB |
| Opt-in telemetry? ([PRD](./PRD.md) open question 3) | Dogfood goal needs usage numbers. | No telemetry |
| AI assistant: local model or API? (PRD open question 4) | Large scope; privacy. | Not planned |
| Provide macOS and Windows signing certificates? | Unsigned packages show OS warnings. | Stay unsigned |

## How to use this file

1. **PM** assigns issues to a version section before handing work to engineering.
2. **Engineering** implements that version, then the release-cut PR bumps the version and CHANGELOG.
3. One Orca item per version. QA and Code Review run on that version’s PRs.

Last updated: 2026-11-27 (plan after v1.0.0 added).
