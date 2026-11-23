# ADR 0002: Bounded ResultWindow and lazy Json

- **Status:** Accepted
- **Date:** 2026-11-23
- **PR:** [#9](https://github.com/nesso99/sela/pull/9)

## Context

At 100k×50, DataTable paint was fine; RSS of the result set was not (especially wide Json). M0-6 measured about **+625 MiB** (narrow) and **+11.6 GiB** (wide) after load.

## Decision

Introduce `ResultWindow`:

- At most **`ROW_WINDOW_MAX_PAGES` (16)** pages of **`PAGE_SIZE` (100)** rows resident; pages load on demand; the cap may be raised for a fully materialized result.
- Store `Value::Json` as raw `Arc<str>`; parse lazily.
- Truncate cell display to ~200 Unicode scalars (newlines → `⏎`) and cache per cell.
- Decode errors become `Value::Other` / decode markers — **never** silent `Null`.

## Consequences

- 100k×50 RSS delta dropped to roughly **+30–76 MiB** depending on narrow / wide / wide-unique datasets.
- Streaming and cancel (M1-2b) insert into the same window.
- Details: [benchmarks.md](../dev/benchmarks.md), [SPECS.md](../design/SPECS.md).
