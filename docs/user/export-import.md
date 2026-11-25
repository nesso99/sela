# Export and CSV import

Core streaming export and CSV import for US-12 (M3-3). UI (dialogs, progress chrome, palette) follows [docs/design/ux/11-export-import.md](../design/ux/11-export-import.md). This page describes the `sela-core` behaviour the UI calls.

## Export

Writers take any `std::io::Write` (tests inject a `Vec<u8>` or `Cursor`; the app opens a file and passes the handle). Rows are formatted one at a time — the whole result is never buffered. A `CancellationToken` is checked between rows; progress is the number of data rows written so far. On cancel, callers should delete any visible partial file via `discard_export_partial` (status `Export cancelled.`).

| Format | Notes |
| --- | --- |
| **CSV** | RFC 4180 quoting. Delimiter `,` (default), `;`, or tab. Header on by default. **NULL = empty field** by default; optional literal `\N` (text equal to the sentinel is always quoted). Empty string is written as `""` so NULL ≠ `''`. Optional UTF-8 BOM (off by default). Binary: lowercase hex or Base64. Line endings are **LF** (`\n`). |
| **JSON** | Array of objects, or NDJSON. SQL NULL → JSON `null`; text `NULL` stays a string. Object keys follow column order. `NUMERIC` is an exact raw JSON number; JSON column text is written after validation with raw `\n`/`\r` collapsed to spaces so NDJSON stays one record per line. |
| **SQL INSERT** | Dialect-quoted identifiers and literals for Postgres, MySQL, and SQLite. Batches flush at **100 rows** or about **1 MiB** of statement text, whichever comes first. Decode/unsupported cells are refused (column named). MySQL/SQLite refuse non-finite floats; Postgres emits `'NaN'` / `'Infinity'` / `'-Infinity'`. Binary: `'\x…'` (Postgres) or `X'…'` (MySQL/SQLite). On a SQL result, use placeholder table `table` plus a trailing `-- replace table name`. |

Clipboard copy of a selection (US-14) stays in [copying.md](./copying.md) and still uses the keyword `NULL` in CSV.

## CSV import

Readers take any `std::io::Read`. Column metadata comes from an injectable `TableSchemaSource` (tests use `StaticTableSchema`). Mapped rows feed `ImportRowSink` (`ChangeSet::insert_row` or a transactional test sink).

- First row is headers by default. Headers match table columns **case-insensitively**; unmatched CSV columns are **Skip**. Required columns left unmapped block with `Map every required column or cancel.`
- **Empty field = NULL** is on by default (unquoted empty → NULL; quoted `""` → `''`).
- Per-row type/parse failures include a 1-based line number; stop and let the caller roll back.
- Safe mode gates import as a write (`decide_import_gate` / `ensure_import_allowed`).
- Input must be valid UTF-8. A leading UTF-8 BOM is stripped when present.

## Tests

Golden fixtures: `crates/sela-core/tests/export_snapshots/`. Acceptance IDs: [v0.3-test-plan.md](../qa/v0.3-test-plan.md) (`EX-*`, `IM-*`).
