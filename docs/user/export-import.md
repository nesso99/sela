# Export and CSV import

Core streaming export and CSV import for US-12 (M3-3). The UI (file dialogs, progress chrome, palette actions) lands after a UX spec; this document describes the `sela-core` behaviour that the UI will call.

## Export

Writers take any `std::io::Write` (tests inject a `Vec<u8>` or `Cursor`; the app will open a file and pass the handle). Rows are formatted one at a time — the whole result is never buffered. A `CancellationToken` is checked between rows; progress is the number of data rows written so far.

| Format | Notes |
| --- | --- |
| **CSV** | RFC 4180 quoting; configurable delimiter, header, and NULL text. Default NULL is `\N` so it stays distinct from `''` and from the text `NULL` (provisional until the UX spec). Binary cells are lowercase hex or Base64. |
| **JSON** | Array of objects, or NDJSON (one object per line). SQL NULL is JSON `null`; the text `NULL` is a JSON string. |
| **SQL INSERT** | Dialect-quoted identifiers and literals for Postgres, MySQL, and SQLite; batched `INSERT` statements. Decode/unsupported cells are refused (column named). MySQL/SQLite refuse non-finite floats; Postgres emits `'NaN'` / `'Infinity'` / `'-Infinity'`. Binary uses `'\x…'` (Postgres) or `X'…'` (MySQL/SQLite). |

Clipboard copy of a selection (US-14) stays in [copying.md](./copying.md) and still uses the keyword `NULL` in CSV.

## CSV import

Readers take any `std::io::Read`. Column metadata comes from an injectable `TableSchemaSource` (tests use `StaticTableSchema`; the app will wrap the schema cache). Mapped rows feed the existing insert path via `ImportRowSink` (`ChangeSet::insert_row` or a transactional test sink).

- First row is headers by default; columns map by name. Unknown headers and missing required columns are errors (not silent mis-columns).
- Per-row type/parse failures include a 1-based line number.
- Safe mode gates import as a write (`decide_import_gate` / `ensure_import_allowed`).
- Cancel leaves the transactional sink uncommitted (driver-level one-transaction import is a follow-up).
- Input must be valid UTF-8 (invalid UTF-8 fails clearly). A leading UTF-8 BOM is stripped when present.

## Tests

Golden fixtures: `crates/sela-core/tests/export_snapshots/`. Acceptance IDs: [v0.3-test-plan.md](../qa/v0.3-test-plan.md) (`EX-*`, `IM-*`).
