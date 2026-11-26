# AGENTS.md

How work gets done on Sela. Applies to every contributor and automated coding agent. Details: [CONTRIBUTING.md](./CONTRIBUTING.md).

## Roles and definition of done

| Role | Owns | Done when |
| --- | --- | --- |
| Engineering | Implementation | Local verification green + PR open with full checklist and tests (headless UI test for UI changes). Do **not** wait on CI. |
| Project management | Scope, review | PR reviewed and signed off; release scope confirmed. |
| QA | Test specs, RC check | At spec time: acceptance criteria turned into automated test specs. At RC: manual pass **only** for rendering, window manager and OS behavior on the owner's Fedora GNOME Wayland machine (primary, via orca-computer-use); X11 is an optional add-on, not a replacement. |
| DevOps | CI, RCs, tags | RC built from a fully green `main`; tag cut after sign-off. |
| UX | Specs | UI spec with acceptance criteria before implementation. |

## Rules

- **Local-first verification.** Prefer local `cargo fmt` / `clippy` / `test`, headless UI tests, and Docker integration when needed. CI is multi-platform verify **after** push only — do not poll, watch, or block the dev loop on `gh pr checks`, `gh run watch`, or similar.
- **UI changes need a UI test.** Any PR touching `crates/sela-app/src/ui/**` adds or extends a headless test in `crates/sela-app/src/ui/ui_tests/`, or states in the PR body why that isn't possible.
- **`main` is always green.** No merges while `main` is red. RCs are cut only from a fully green `main` (the `ci-gate` check). Merge still requires required checks; authors and agents do not wait on them during the PR loop.
- **Automated tests first.** Manual checks cover only what automation can't (rendering, window manager, OS).
- **English** in everything in the repo.
- **At most 3 open PRs** in total.
- **One version per request.** Never start the next version unprompted; new ideas go to the backlog issue. Planned release themes and grouping live in [docs/design/version-roadmap.md](docs/design/version-roadmap.md) — ship a **version bundle**, not one issue per tag.
- **Tags need QA + PM sign-off.**
- **Keep cost low.** Coding agents use the Auto model; keep messages, logs and CI runs lean.

## Release flow

1. Spec (UX + PM), with acceptance criteria.
2. QA test plan, with the acceptance criteria written as automated tests.
3. PRs (local checks green, checklist complete, reviewed).
4. RC from green `main` (DevOps).
5. QA manual pass on Fedora GNOME Wayland (primary; rendering / window manager / OS only via orca-computer-use). X11 optional add-on.
6. QA + PM sign-off.
7. Tag (DevOps).

## Message protocol

- First line: `[role] action-needed|FYI: <summary>`, e.g. `[QA] action-needed: RC3 fails #101`.
- Mark a message high priority whenever the receiver must act or someone is waiting; FYIs are low priority.
- At the start of a version, PM sends one handoff message per role, quoting the maintainer's request verbatim.
