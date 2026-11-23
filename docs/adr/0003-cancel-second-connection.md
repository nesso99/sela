# ADR 0003: Cancel through a second connection

- **Status:** Accepted
- **Date:** 2026-11-23
- **PR:** [#10](https://github.com/nesso99/sela/pull/10)

## Context

sqlx has no standard cancel/interrupt API. Long queries must be stoppable from the UI (Stop / Cmd+Ctrl+.) without freezing GPUI or poisoning the connection pool.

## Decision

- Every DB task gets a `tokio_util::sync::CancellationToken`.
- **Postgres:** `pg_cancel_backend(pid)` from a **second** pool connection.
- **MySQL:** `KILL QUERY` from a **second** pool connection.
- After cancel, await the cancel task (short timeout) and **`conn.close()`** the cancelled connection so an in-flight cancel cannot land on the next checkout.
- **SQLite:** drop the `fetch` stream only (documented limitation; no rusqlite interrupt switch).

## Consequences

- Server-side cancel works for PG/MySQL within about a second in integration tests (`pg_sleep` / `SLEEP`).
- Pool stays usable after cancel (next query on the same pool succeeds).
- SQLite cancel is best-effort local only.
