# Copying grid data

Select cells in the results grid, then copy them to the clipboard. This covers US-14 (clipboard copy of a selection). Full-table export is a later milestone.

## Selecting

- Click a cell to focus it.
- Shift-click another cell to grow a rectangular range from the first focus.
- Click a row or column header when the table offers that selection (same as the DataTable selection model).

## Copy formats

With the grid focused (`DataTable` key context):

| Action | Shortcut / control |
| --- | --- |
| Copy as TSV (default) | Cmd/Ctrl+C |
| Copy | Right-click → Copy (same TSV as Cmd/Ctrl+C) |
| Copy as TSV / CSV / JSON / INSERT | Right-click context menu on a cell or row header |

- **TSV** — tab-separated values with a header row. Default for Cmd/Ctrl+C so spreadsheets paste cleanly.
- **CSV** — comma-separated values; fields with commas, quotes, or newlines are quoted (RFC 4180-style).
- **JSON** — a pretty-printed array of objects keyed by column name. SQL NULL becomes JSON `null`. A text cell whose value is the four characters `NULL` stays a JSON string. Non-finite floats (`NaN`, `inf`, `-inf`) are not valid JSON numbers, so they are emitted as JSON strings with the cell’s display text (not `null`).
- **INSERT** — one `INSERT INTO … VALUES (…);` statement. On a table Data tab the target is `schema.table` with dialect quoting (`quote_ident`). Finite numbers and booleans are unquoted; strings are quoted (`'` → `''`; on MySQL `\` → `\\`). On a SQL result tab the placeholder name is `table`, followed by `-- replace table name`.
  - **Postgres:** non-finite floats become quoted literals `'NaN'`, `'Infinity'`, or `'-Infinity'` (valid for `float4` / `float8` / `numeric`).
  - **MySQL and SQLite:** INSERT is refused when a selected number is non-finite; the status bar names the column.

SQL NULL copies as the keyword `NULL` in TSV/CSV/INSERT (and as JSON `null`). A text value equal to `NULL` copies as ordinary text / a quoted string / a JSON string — it is never treated as SQL NULL. TSV and CSV keep the display text for non-finite floats (`NaN`, `inf`, `-inf`).

If the selection includes rows that are not loaded yet, copy is refused and the status bar shows how many rows are missing (scroll or narrow the selection). INSERT is also refused when a selected cell is a decode error or unsupported type, or (on MySQL/SQLite) a non-finite number; the status bar names that column.

Copy uses rows already present in the result window. Full-table export remains a later milestone.
