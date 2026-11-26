# Version roadmap

How **releases** are planned. Milestone detail stays in [roadmap.md](./roadmap.md); this file is the **semver train**: each version ships one **coherent theme**, not a single ticket at a time.

## Rules

| Rule | Meaning |
| --- | --- |
| **One theme per version** | Scope is a named group of issues/PRs with shared acceptance and one RC QA pass. |
| **Cut once** | Version bump + CHANGELOG section happen in a **release PR** after all feature PRs for that version are on `main` (or one integration branch), same pattern as v0.3.1. |
| **At most 3 open PRs** | Prefer finishing the current version’s feature PRs before starting the next version’s work ([AGENTS.md](../../AGENTS.md)). |
| **Backlog → version** | New ideas go to GitHub issues; PM assigns them to a **version section** here before SE starts. |
| **Tag** | After green `main`, RC QA on Fedora, QA + PM sign-off ([CONTRIBUTING.md](../../CONTRIBUTING.md)). |

## Shipped

| Version | Theme | On `main` (high level) |
| --- | --- | --- |
| **v0.3.0** | M3 connectivity & data | Autocomplete, SSH, export/import, palette, optimistic WHERE, etc. See [CHANGELOG](../../CHANGELOG.md#030---2026-11-26). |
| **v0.3.1** | Post-M3 patch + QA ops | #118 column auto-fit, #137 column width, #140–#141 UI fixes, Fedora QA scripts + local-first policy (#143–#146). **In flight:** release PR [#148](https://github.com/nesso99/sela/pull/148). |

## Planned

### v0.3.2 · Window & menu chrome

**Goal:** One consistent top-of-window experience on CSD (Wayland) and SSD (X11/Windows); no one-off menu strips.

| Issue / work | Done when |
| --- | --- |
| [#147](https://github.com/nesso99/sela/issues/147) — Remove File from CSD title bar; Quit via Ctrl+Q and close only | UX spec + headless UI tests updated; SSD row policy documented |
| SSD `menu_bar_row` | Same Quit story without a second File menu where chrome is unified (align with #147 decision in PR) |
| QA | `M-REG-01` smoke on Fedora after all v0.3.2 PRs merge; one RC for **0.3.2** |

**Not in v0.3.2:** grid editing, import scale, M4 packaging.

### v0.4.0 · Grid editing follow-ups (batch 1)

**Goal:** Safer, more predictable grid keyboard and commit behavior ([#72](https://github.com/nesso99/sela/issues/72) part 1).

| Slice from #72 | Done when |
| --- | --- |
| Shortcuts while cell editor open | Duplicate/Insert do not fire through the field; matches Discard rules |
| Inserts & generated keys | Commit enablement matches DEFAULT / omitted generated columns |
| Types & display (small) | Empty accept / `NULL` string paint fixes called out in #72 |

One RC; update [v0.3-test-plan.md](../qa/v0.3-test-plan.md) or add v0.4-test-plan when scope is locked.

### v0.5.0 · Grid editing follow-ups (batch 2) + scale

**Goal:** Performance and correctness at larger edit sets ([#72](https://github.com/nesso99/sela/issues/72) part 2).

| Slice | Done when |
| --- | --- |
| `build_edit_paint` cost | Indexed pending updates or equivalent; measurable win on large pages |
| Views / structure reload | View tabs and `EditSession::from_structure` behavior corrected |
| BIGINT UNSIGNED / round-trip | Editor and binders handle edge cases in #72 |
| IM-07 (partial) | Streaming CSV import sink — **only if** bundled here; otherwise own version |

### v1.0.0 · Ship (M4)

**Goal:** Installable, documented, measured release per [roadmap.md](./roadmap.md#m4--ship-v10).

| ID | Work |
| --- | --- |
| M4-1 | Signed packages for 3 OS in CI |
| M4-2 | NFRs + benchmark report in release notes |
| M4-3 | Naming / branding polish |
| M4-4 | User docs, shortcuts, contributor path |

### After v1.0

Structure editing (DDL + preview), extra drivers (MariaDB, DuckDB, SQL Server, ClickHouse, Redis), ER diagram, AI/MCP — sequenced into **v1.x** themes when requested; not single-issue drops.

## How to use this file

1. **PM / owner** — Move issues under a version section before handoff to SE.
2. **SE** — Implement all rows for that version (usually 1–2 feature PRs), then release cut PR.
3. **CoS** — One item per **version**, not per issue; parallel QA + Code Review only within the same version RC.

Last updated: 2026-11-26 (PM).