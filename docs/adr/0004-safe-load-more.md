# ADR 0004: Safe load-more re-run rule

- **Status:** Accepted
- **Date:** 2026-11-23
- **PR:** [#10](https://github.com/nesso99/sela/pull/10)

## Context

Arbitrary SQL is **not** wrapped in `SELECT * FROM (…) LIMIT`. Results stream up to `DEFAULT_SQL_ROW_CAP` (**1 000** rows). When the live stream is gone, “Load more” must not re-execute statements with side effects.

## Decision

Raise the live `AtomicUsize` cap when a stream is still open. If the stream is gone, re-run only when `is_safe_to_rerun_for_load_more` classifies the statement as:

- read-only `SELECT` / `VALUES`
- `WITH` without DML
- `SHOW`
- `EXPLAIN` without `ANALYZE`

Otherwise disable Load more and show “Result capped at N rows. Re-run manually to see more.”

## Consequences

- `INSERT … RETURNING` and similar are not re-run by Load more (covered in SQLite tests).
- **Known limitation:** the text classifier still treats `SELECT my_func()` as safe even if the function has side effects.
