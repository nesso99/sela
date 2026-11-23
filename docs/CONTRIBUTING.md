> Source of truth: this file; canvas is a snapshot.

# Contributing to Sela

Sela is open source under **GPL-3.0-or-later** (see [`LICENSE`](../LICENSE)). Product and technical docs: [PRD.md](./PRD.md), [SPECS.md](./SPECS.md).

All project text — docs, source comments, UI strings, commit messages, and PR bodies — must be in **English**.

## Process

1. Every PR is reviewed by the Project Manager agent and **squash-merged** only after approval **and** green CI.
2. At most **3** PRs are open at once.
3. Cloud agents use the **Auto** model.
4. Every PR body includes the self-review checklist below (tick or mark **N/A**).

Prefer small, independent PRs that match the milestone order in [PRD.md](./PRD.md).

## Self-review checklist

Copy into the PR body and mark each item done or N/A:

- [ ] **Re-execution / side effects** — load-more and similar paths do not re-run unsafe SQL; restored tabs do not auto-run.
- [ ] **Concurrency / cancel** — request ids and `CancellationToken`s stay correct; stale completions ignored; cancel cleanup does not poison the pool.
- [ ] **Boundaries** — paging / streaming / page caps still bounded; no unbounded materialization on hot paths.
- [ ] **SQL safety** — identifiers quoted; values bound (no string-concat of user data into SQL).
- [ ] **Errors** — never silently become NULL, empty, or success; preserve `DbError` variant and sqlstate / vendor code where applicable.
- [ ] **Memory / perf** — no large clones on hot paths; caches bounded; if grid/result code changed, note `grid_bench` numbers.
- [ ] **Engines** — all 3 engines covered, or the limitation documented.
- [ ] **Tests** — observable behavior covered; one regression test per bug fixed.
- [ ] **Docs / backlog** — PRD/SPECS/README/backlog updated when behavior or plans change.
- [ ] **fmt / clippy / CI** — `cargo fmt`, `clippy -D warnings`, and CI green (including `cargo deny` when license-relevant).

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Integration tests for Postgres/MySQL need URLs (see README) or the CI `db-integration` job.
