> Source of truth: this file.

# Contributing to Sela

Sela is open source under **GPL-3.0-or-later** (see [`LICENSE`](./LICENSE)). Team workflow, roles and release flow: [AGENTS.md](./AGENTS.md). Product and technical docs: [PRD](./docs/design/PRD.md), [SPECS](./docs/design/SPECS.md), [roadmap](./docs/design/roadmap.md).

## Language

All project text — docs, source comments, UI strings, commit messages, and pull request descriptions — must be in **English**.

## How to contribute

1. Fork the repository and create a topic branch from `main`.
2. Make a focused change with tests for any observable behavior you touch.
3. Run the [local checks](#local-checks) below until they pass. Do **not** wait on GitHub Actions during the authoring loop (no `gh pr checks` / `gh run watch` / poll).
4. Open a pull request against `main`. Fill in the PR template checklist (tick each item or mark **N/A**). CI runs after push as multi-platform verify; merge still requires green required checks, but you do not block on them while iterating.
5. Maintainers review the PR. It is **squash-merged** only after approval **and** green CI, and never while `main` is red.
6. PRs touching `crates/sela-app/src/ui/**` add or extend a headless UI test (see below) or explain why not.

Prefer small, independent PRs that match the milestone order in [roadmap.md](./docs/design/roadmap.md).

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses bans sources
```

More detail (Docker DBs, env vars, `grid_bench`, headless UI tests): [building-and-testing.md](./docs/dev/building-and-testing.md).

### Headless UI tests

High-risk GPUI flows (connection dialog, cell editor, reconnect) use
`gpui_kit::TestAppContext` so they run without a display:

```bash
./scripts/ui-tests.sh
```

On Linux CI, install `xvfb` (and optionally `mesa-vulkan-drivers`) in addition to
the usual GPUI packages; the script wraps with `xvfb-run -a` when present.
Write new cases under `crates/sela-app/src/ui/ui_tests/` with `#[gpui_kit::test]`,
drive real key/mouse events via `TestWindowExt`, and never log URLs or passwords.

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
- [ ] **UI test** — if this PR touches `crates/sela-app/src/ui/**`, a headless test in `ui_tests` is added or extended, or the PR body says why that isn't possible.
- [ ] **Docs / backlog** — design docs / README / CHANGELOG updated when behavior or plans change.
- [ ] **Behavior change?** — CHANGELOG + relevant docs updated in this PR.
- [ ] **fmt / clippy / local tests** — `cargo fmt`, `clippy -D warnings`, and tests run locally (including `cargo deny` when dependencies or license config change). CI verifies after push; do not wait on it in the authoring loop.
