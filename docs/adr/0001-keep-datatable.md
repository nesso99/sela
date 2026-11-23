# ADR 0001: Keep gpui-component DataTable

- **Status:** Accepted
- **Date:** 2026-11-23
- **PR:** [#8](https://github.com/nesso99/sela/pull/8)

## Context

The results grid needed a keep-or-rewrite decision at **100 000 rows × 50 columns**. Memory and scroll fps were the risks.

## Decision

**Keep** gpui-component `DataTable`. It already virtualizes rows and columns. Do not rewrite the grid for virtualization.

## Consequences

- Follow-up work targets the **result model** (bounded window, lazy Json, truncated display), not a custom widget.
- Measured numbers and method live in [benchmarks.md](../dev/benchmarks.md).
- Revisit a custom grid only if a real GPU still misses frame budget after the model work, or if DataTable cannot grow into needed features.
