> Source of truth: this file.

# Contributing to Sela

Sela is open source under **GPL-3.0-or-later** (see [`LICENSE`](./LICENSE)). Product and technical docs: [PRD](./docs/design/PRD.md), [SPECS](./docs/design/SPECS.md), [roadmap](./docs/design/roadmap.md).

## Language

All project text — docs, source comments, UI strings, commit messages, and pull request descriptions — must be in **English**.

## How to contribute

1. Fork the repository and create a topic branch from `main`.
2. Make a focused change with tests for any observable behavior you touch.
3. Open a pull request against `main`. Fill in the PR template checklist (tick each item or mark **N/A**).
4. Maintainers review the PR. It is **squash-merged** only after approval **and** green CI.

Prefer small, independent PRs that match the milestone order in [roadmap.md](./docs/design/roadmap.md).

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses bans sources
```

More detail (Docker DBs, env vars, `grid_bench`): [building-and-testing.md](./docs/dev/building-and-testing.md).

### Database integration tests

SQLite tests need no server. Postgres and MySQL tests run when URLs are set (otherwise they skip):

```bash
docker compose up -d
export SELA_POSTGRES_URL='postgres://sela:sela@127.0.0.1:5432/sample?sslmode=disable'
export SELA_MYSQL_URL='mysql://sela:sela@127.0.0.1:3306/sample?ssl-mode=disabled'
cargo test -p sela-drivers
```

See the root [README](./README.md) for OS prerequisites and connection paths.

## Pull request checklist

Use the same items as [`.github/pull_request_template.md`](./.github/pull_request_template.md):

- [ ] **Re-execution / side effects** — load-more and similar paths do not re-run unsafe SQL; restored tabs do not auto-run.
- [ ] **Concurrency / cancel** — request ids and cancellation tokens stay correct; stale completions ignored; cancel cleanup does not poison the pool.
- [ ] **Boundaries** — paging / streaming / page caps still bounded; no unbounded materialization on hot paths.
- [ ] **SQL safety** — identifiers quoted; values bound (no string-concat of user data into SQL).
- [ ] **Errors** — never silently become NULL, empty, or success; preserve `DbError` variant and sqlstate / vendor code where applicable.
- [ ] **Changed meaning** — if this PR changes the meaning of a value (row index, NULL/unset, status, id), grep every use of it and list the call sites you checked in the PR description.
- [ ] **Memory / perf** — no large clones on hot paths; caches bounded; if grid/result code changed, note `grid_bench` numbers.
- [ ] **Engines** — all 3 engines covered, or the limitation documented.
- [ ] **Tests** — observable behavior covered; one regression test per bug fixed.
- [ ] **Docs / backlog** — design docs / README / CHANGELOG updated when behavior or plans change.
- [ ] **Behavior change?** — CHANGELOG + relevant docs updated in this PR.
- [ ] **fmt / clippy / CI** — `cargo fmt`, `clippy -D warnings`, tests, and CI green (including `cargo deny` when dependencies or license config change).
