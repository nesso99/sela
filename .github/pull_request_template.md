## Summary

<!-- What does this PR change, and why? -->

## Self-review checklist

Mark each item done or **N/A**:

- [ ] **Re-execution / side effects** — load-more and similar paths do not re-run unsafe SQL; restored tabs do not auto-run.
- [ ] **Concurrency / cancel** — request ids and cancellation tokens stay correct; stale completions ignored; cancel cleanup does not poison the pool.
- [ ] **Boundaries** — paging / streaming / page caps still bounded; no unbounded materialization on hot paths.
- [ ] **SQL safety** — identifiers quoted; values bound (no string-concat of user data into SQL).
- [ ] **Errors** — never silently become NULL, empty, or success; preserve `DbError` variant and sqlstate / vendor code where applicable.
- [ ] **Changed meaning** — if this PR changes the meaning of a value (row index, NULL/unset, status, id), grep every use of it and list the call sites you checked in the PR description.
- [ ] **Memory / perf** — no large clones on hot paths; caches bounded; if grid/result code changed, note `grid_bench` numbers.
- [ ] **Engines** — all 3 engines covered, or the limitation documented.
- [ ] **Tests** — observable behavior covered; one regression test per bug fixed.
- [ ] **UI test** — if this PR touches `crates/sela-app/src/ui/**`, a headless test in `ui_tests` is added or extended, or the PR body says why that isn't possible.
- [ ] **Docs / backlog** — design docs / README / CHANGELOG updated when behavior or plans change.
- [ ] **Behavior change?** — CHANGELOG + relevant docs updated in this PR.
- [ ] **fmt / clippy / CI** — `cargo fmt`, `clippy -D warnings`, tests, and CI green (including `cargo deny` when dependencies or license config change).
